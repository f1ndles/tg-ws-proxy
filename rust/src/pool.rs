//! Pre-warmed WebSocket connection pool.
//!
//! Maintaining a small pool of idle WebSocket connections to each Telegram DC
//! eliminates the TLS + WebSocket handshake latency on the critical path of a
//! new client connection (typical saving: 100–400 ms).
//!
//! The pool is keyed by `(dc_id, is_media)`.  Background refill tasks run
//! after each pool hit to keep the bucket at `pool_size` connections.
//!
//! The Cloudflare tiers are pooled in the same struct but held to a much
//! tighter budget — see [`CF_POOL_MAX`].  They matter more than the direct pool
//! does: a client that reaches Telegram through Cloudflare pays *two*
//! handshakes (to the Cloudflare edge, then Cloudflare's own connection onward)
//! before its first byte moves, and Telegram opens a fresh connection per media
//! transfer, so on a blocked network that cost lands on every download.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::{debug, warn};

use futures_util::{FutureExt, StreamExt};

use crate::config::Config;
use crate::outbound::OutboundConnector;
use crate::runtime::Runtime;
use crate::ws_client::{
    TgWsStream, connect_cf_record_with_outbound, connect_cf_worker_ws_for_dc_with_outbound,
    connect_ws_for_dc_with_outbound, media_tag,
};

/// Idle Cloudflare connections kept per `(tier, dc, is_media)`.
///
/// One, regardless of `--pool-size`.  Each one holds a connection open on
/// someone else's account — a Worker request against its free-plan daily quota,
/// or a socket on whichever `--cf-domain` served it, which with
/// `--default-domains` is a community-run zone.  The pool is there to cover the
/// *next* connection, not a burst.  Upstream settled on the same number for its
/// Worker pool in v1.9.1.
const CF_POOL_MAX: usize = 1;

/// Which Cloudflare tier a pooled connection belongs to.  Pooled separately
/// because the two are not interchangeable at the far end: a Worker is a raw
/// TCP tunnel, a `--cf-domain` fronts Telegram's own WebSocket endpoint.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CfTier {
    Worker,
    Proxy,
}

impl CfTier {
    fn label(self) -> &'static str {
        match self {
            Self::Worker => "CF Worker",
            Self::Proxy => "CF proxy",
        }
    }
}

struct PoolEntry {
    ws: TgWsStream,
    created: Instant,
}

/// A pooled Cloudflare connection, tagged with the domain it was opened
/// through so a pool hit logs like a fresh connect.
struct CfEntry {
    ws: TgWsStream,
    created: Instant,
    domain: String,
    /// Kept alongside the connection so taking it from the pool is enough to
    /// describe its replacement.
    dst: String,
    skip_tls_verify: bool,
    connect_timeout: Duration,
}

/// Everything a background Cloudflare refill needs to reopen one connection.
pub struct CfTarget {
    pub tier: CfTier,
    pub dc: u32,
    pub is_media: bool,
    /// Telegram DC IP the Worker opens its TCP tunnel to.  Unused by
    /// [`CfTier::Proxy`], which reaches the DC through Cloudflare's own routing.
    pub dst: String,
    /// The domain that just served a connection for this tier.
    pub domain: String,
    pub skip_tls_verify: bool,
    pub connect_timeout: Duration,
}

impl CfTarget {
    fn key(&self) -> CfKey {
        (self.tier, self.dc, self.is_media)
    }
}

type CfKey = (CfTier, u32, bool);
type Bucket = Vec<PoolEntry>;
type PoolMap = HashMap<(u32, bool), Bucket>;

pub struct WsPool {
    pool_size: usize,
    /// Maximum age for a pooled connection.  Connections older than this are
    /// discarded on next use rather than handed to a client.
    max_age: Duration,
    runtime: Arc<Runtime>,
    idle: Mutex<PoolMap>,
    cf_idle: Mutex<HashMap<CfKey, Vec<CfEntry>>>,
    cf_refilling: StdMutex<HashSet<CfKey>>,
    /// Tracks which (dc, is_media) buckets currently have a refill in flight.
    /// Prevents a stampede of concurrent refill tasks when many clients arrive
    /// simultaneously — each `pool.get()` call spawns a refill, and without
    /// this guard they all open `pool_size` connections at once, exhausting FDs.
    ///
    /// Uses a standard (non-async) mutex because the critical section is tiny
    /// (a single HashSet insert/remove) and never holds the lock across an
    /// await point, which enables a simple Drop-based cleanup guard.
    refilling: StdMutex<HashSet<(u32, bool)>>,
}

/// RAII guard that removes a bucket key from a `refilling` set when dropped,
/// guaranteeing cleanup even on early returns or panics.
struct RefillGuard<'a, K: Eq + Hash> {
    set: &'a StdMutex<HashSet<K>>,
    key: K,
}

impl<K: Eq + Hash> Drop for RefillGuard<'_, K> {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.key);
    }
}

impl WsPool {
    pub fn new(pool_size: usize, max_age: Duration) -> Self {
        Self::with_runtime(
            pool_size,
            max_age,
            Arc::new(Runtime::new(OutboundConnector::direct())),
        )
    }

    pub fn with_runtime(pool_size: usize, max_age: Duration, runtime: Arc<Runtime>) -> Self {
        Self {
            pool_size,
            max_age,
            runtime,
            idle: Mutex::new(HashMap::new()),
            cf_idle: Mutex::new(HashMap::new()),
            cf_refilling: StdMutex::new(HashSet::new()),
            refilling: StdMutex::new(HashSet::new()),
        }
    }

    /// Take a pre-warmed connection from the pool, if available and fresh.
    ///
    /// Returns `Some(ws)` on a pool hit, `None` if the bucket is empty or
    /// all entries were stale.  Schedules a background refill either way.
    /// `allow_refill` is the caller's verdict on whether `target_ip` is worth
    /// dialling in the background at all.  A pre-connect into an address that
    /// is currently timing out costs a connect timeout per pooled slot and can
    /// never succeed, so the routing layer — which owns that knowledge — gets
    /// to veto it.
    pub async fn get(
        self: &Arc<Self>,
        dc: u32,
        is_media: bool,
        target_ip: String,
        skip_tls_verify: bool,
        allow_refill: bool,
    ) -> Option<TgWsStream> {
        let now = Instant::now();
        let mut lock = self.idle.lock().await;
        let bucket = lock.entry((dc, is_media)).or_default();

        // Drain from the back (LIFO) so the freshest connections are used first.
        while let Some(mut entry) = bucket.pop() {
            if now.saturating_duration_since(entry.created) > self.max_age {
                // Entry is stale; drop it (close happens on drop via tungstenite).
                continue;
            }

            // Non-blocking liveness check: if the server has already closed the
            // WebSocket (TCP FIN received), `next()` resolves immediately with
            // `None` or an error.  Any message arriving on an idle pre-warmed
            // connection (close, error, or unexpected data) is treated as a sign
            // that the connection is in an invalid state and should be discarded.
            if entry.ws.next().now_or_never().is_some() {
                debug!(
                    "pool: discarding stale DC{}{} connection",
                    dc,
                    media_tag(is_media)
                );
                continue;
            }

            let remaining = bucket.len();
            drop(lock);

            debug!(
                "pool hit DC{}{} ({} left)",
                dc,
                media_tag(is_media),
                remaining
            );

            // Schedule a background task to refill the bucket.
            if allow_refill {
                let pool = Arc::clone(self);
                tokio::spawn(async move {
                    pool.refill(dc, is_media, target_ip, skip_tls_verify).await;
                });
            }

            return Some(entry.ws);
        }

        // Bucket is empty (or fully stale).
        drop(lock);

        if allow_refill {
            let pool = Arc::clone(self);
            tokio::spawn(async move {
                pool.refill(dc, is_media, target_ip, skip_tls_verify).await;
            });
        }

        None
    }

    /// Take a pre-opened Cloudflare connection, if one is idle and fresh.
    ///
    /// Returns the connection and the domain it runs through.  Cheap on a
    /// miss — the routing path reaches this before it knows which domain it
    /// will end up using, so nothing is built until there is something to
    /// re-open.
    pub async fn cf_get(
        self: &Arc<Self>,
        tier: CfTier,
        dc: u32,
        is_media: bool,
    ) -> Option<(TgWsStream, String)> {
        let now = Instant::now();
        let mut lock = self.cf_idle.lock().await;
        let bucket = lock.get_mut(&(tier, dc, is_media))?;

        while let Some(mut entry) = bucket.pop() {
            if now.saturating_duration_since(entry.created) > self.max_age
                || entry.ws.next().now_or_never().is_some()
            {
                debug!(
                    "{} pool: discarding stale DC{}{} connection",
                    tier.label(),
                    dc,
                    media_tag(is_media)
                );
                continue;
            }

            drop(lock);
            self.cf_prefetch(CfTarget {
                tier,
                dc,
                is_media,
                dst: entry.dst,
                domain: entry.domain.clone(),
                skip_tls_verify: entry.skip_tls_verify,
                connect_timeout: entry.connect_timeout,
            });

            return Some((entry.ws, entry.domain));
        }

        None
    }

    /// Start opening one spare Cloudflare connection in the background.
    ///
    /// Deliberately only called off a connection that just worked, so a dead
    /// Worker (or a network that cannot reach Cloudflare at all) is not
    /// dialled again behind the user's back on every client connection.
    pub fn cf_prefetch(self: &Arc<Self>, target: CfTarget) {
        let pool = Arc::clone(self);

        tokio::spawn(async move {
            pool.cf_refill(target).await;
        });
    }

    /// Warm up the pool for all configured DCs on startup.
    pub async fn warmup(&self, config: &Config) {
        let dc_redirects = config.dc_redirects();
        let skip_tls = config.skip_tls_verify;
        let pool_size = self.pool_size;

        for (dc, ip) in dc_redirects {
            for is_media in [false, true] {
                let new_conns = self
                    .connect_batch(&ip, dc, is_media, skip_tls, pool_size)
                    .await;
                let mut lock = self.idle.lock().await;
                let bucket = lock.entry((dc, is_media)).or_default();

                for ws in new_conns {
                    bucket.push(PoolEntry {
                        ws,
                        created: Instant::now(),
                    });
                }
            }
        }

        debug!("WS pool warmup complete");
    }

    // ── Internal ─────────────────────────────────────────────────────────

    async fn refill(&self, dc: u32, is_media: bool, target_ip: String, skip_tls: bool) {
        // Ensure only one refill runs at a time per (dc, is_media) key.
        // Without this, a burst of simultaneous pool.get() calls spawns N
        // refill tasks that each open pool_size connections concurrently,
        // exhausting file descriptors well beyond the intended pool budget.
        let registered = self.refilling.lock().unwrap().insert((dc, is_media));
        if !registered {
            return; // another refill is already in progress for this key
        }

        // The guard removes the key from `refilling` when it goes out of scope,
        // covering all exit paths (normal return, early return, or panic).
        let _guard = RefillGuard {
            set: &self.refilling,
            key: (dc, is_media),
        };

        let needed = {
            let lock = self.idle.lock().await;

            let current = lock.get(&(dc, is_media)).map_or(0, |b| b.len());
            if current >= self.pool_size {
                return;
            }

            self.pool_size - current
        };

        let new_conns = self
            .connect_batch(&target_ip, dc, is_media, skip_tls, needed)
            .await;
        if !new_conns.is_empty() {
            let mut lock = self.idle.lock().await;
            let bucket = lock.entry((dc, is_media)).or_default();

            // Re-check available space; another path (e.g. warmup) may have
            // filled the bucket while we were connecting.  Drop any surplus
            // connections so their FDs are closed immediately.
            let can_add = self.pool_size.saturating_sub(bucket.len());
            for ws in new_conns.into_iter().take(can_add) {
                bucket.push(PoolEntry {
                    ws,
                    created: Instant::now(),
                });
            }

            debug!(
                "pool refilled DC{}{}: {} ready",
                dc,
                media_tag(is_media),
                lock.get(&(dc, is_media)).map_or(0, |b| b.len())
            );
        }
    }

    async fn cf_refill(&self, target: CfTarget) {
        let key = target.key();
        if !self.cf_refilling.lock().unwrap().insert(key) {
            return; // another refill is already in progress for this key
        }
        let _guard = RefillGuard {
            set: &self.cf_refilling,
            key,
        };

        // `--pool-size 0` disables pooling outright, exactly as it does for
        // the direct pool — which an empty bucket must not talk its way out of.
        let budget = CF_POOL_MAX.min(self.pool_size);
        if self.cf_idle.lock().await.get(&key).map_or(0, Vec::len) >= budget {
            return;
        }

        let Some(ws) = self.cf_connect_one(&target).await else {
            return;
        };

        let mut lock = self.cf_idle.lock().await;
        let bucket = lock.entry(key).or_default();

        // The bucket may have been filled while this connect was in flight;
        // dropping the surplus here closes its FD immediately.
        if bucket.len() < budget {
            debug!(
                "{} pool refilled DC{}{} via {}",
                target.tier.label(),
                target.dc,
                media_tag(target.is_media),
                target.domain
            );
            bucket.push(CfEntry {
                ws,
                created: Instant::now(),
                domain: target.domain,
                dst: target.dst,
                skip_tls_verify: target.skip_tls_verify,
                connect_timeout: target.connect_timeout,
            });
        }
    }

    /// Re-open one connection of `target`'s tier through the domain that last
    /// served this DC.
    ///
    /// The proxy tier is dialled with a single domain rather than the whole
    /// `--cf-domain` list; that still covers its `kwsN` *and* `kwsN-1` records,
    /// exactly as the inline path does.
    async fn cf_connect_one(&self, target: &CfTarget) -> Option<TgWsStream> {
        match target.tier {
            CfTier::Worker => {
                connect_cf_worker_ws_for_dc_with_outbound(
                    &target.domain,
                    &target.dst,
                    target.dc,
                    target.is_media,
                    target.skip_tls_verify,
                    target.connect_timeout,
                    self.runtime.outbound(),
                )
                .await
            }
            CfTier::Proxy => {
                connect_cf_record_with_outbound(
                    &target.domain,
                    target.skip_tls_verify,
                    target.connect_timeout,
                    self.runtime.outbound(),
                )
                .await
            }
        }
    }

    async fn connect_batch(
        &self,
        ip: &str,
        dc: u32,
        is_media: bool,
        skip_tls: bool,
        count: usize,
    ) -> Vec<TgWsStream> {
        let mut results = Vec::new();
        // Limit pool fill timeout to avoid blocking for too long.
        let timeout = Duration::from_secs(8);
        // While the domain-fronting fallback is in its sticky window, warm the
        // pool with fronted connections too — otherwise a pool hit would hand
        // a client a connection that never had to front in the first place,
        // defeating the point of staying "sticky".
        let fronting_domain = self
            .runtime
            .fronting_active()
            .then(|| self.runtime.fronting_domain())
            .flatten();

        for _ in 0..count {
            match connect_ws_for_dc_with_outbound(
                ip,
                dc,
                is_media,
                skip_tls,
                timeout,
                self.runtime.outbound(),
                fronting_domain,
            )
            .await
            .ws
            {
                Some(ws) => results.push(ws),
                None => {
                    warn!(
                        "pool: failed to pre-connect DC{}{}",
                        dc,
                        media_tag(is_media)
                    );

                    break;
                }
            }
        }
        results
    }
}
