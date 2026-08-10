use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use tg_ws_proxy_rs::config::Config;
use tg_ws_proxy_rs::outbound::OutboundConnector;
use tg_ws_proxy_rs::pool::{CfTarget, CfTier, WsPool};
use tg_ws_proxy_rs::runtime::Runtime;

mod common;

use common::{await_proxy_requests, rejecting_http_proxy, rejecting_http_proxy_requests};

fn pool_with_outbound(pool_size: usize, outbound: OutboundConnector) -> Arc<WsPool> {
    Arc::new(WsPool::with_runtime(
        pool_size,
        Duration::from_secs(55),
        Arc::new(Runtime::new(outbound)),
    ))
}

#[tokio::test]
async fn an_empty_pool_misses_and_lets_the_caller_connect_directly() {
    let pool = pool_with_outbound(0, OutboundConnector::direct());

    let hit = pool
        .get(2, false, "203.0.113.10".to_string(), false, true)
        .await;

    assert!(hit.is_none());
}

#[tokio::test]
async fn pool_refill_dials_through_the_outbound_connector() {
    // A miss schedules a background refill; with an unreachable upstream it
    // must give up rather than spin, and it must honour the outbound proxy.
    let (proxy_addr, proxy_task) = rejecting_http_proxy_requests().await;
    let outbound =
        OutboundConnector::from_config(Some(&format!("http://{proxy_addr}")), None, false).unwrap();
    let pool = pool_with_outbound(1, outbound);

    assert!(
        pool.get(2, false, "203.0.113.10".to_string(), false, true)
            .await
            .is_none()
    );

    let requests = await_proxy_requests(proxy_task).await;
    assert!(
        requests[0].starts_with("CONNECT 203.0.113.10:443 HTTP/1.1"),
        "unexpected refill target: {requests:?}"
    );
}

#[tokio::test]
async fn warmup_gives_up_on_unreachable_dcs_instead_of_hanging() {
    // `connect_batch` abandons a bucket after its first failure, so a fully
    // blocked network costs one *attempt* per (DC, media) bucket rather than
    // `--pool-size` of them. Each attempt still tries both Telegram hostnames,
    // so one DC costs 2 buckets × 2 records = 4 CONNECTs, not 2 × 4 = 8.
    let (proxy_addr, proxy_task) = rejecting_http_proxy_requests().await;
    let config = Config::try_parse_from([
        "tg-ws-proxy",
        "--dc-ip",
        "2:203.0.113.10",
        "--outbound-proxy",
        &format!("http://{proxy_addr}"),
        "--no-outbound-proxy",
        "--no-proxy",
        "",
    ])
    .unwrap();
    let pool = pool_with_outbound(4, config.outbound_connector().unwrap());

    tokio::time::timeout(Duration::from_secs(10), pool.warmup(&config))
        .await
        .expect("warmup should not hang on an unreachable DC");

    let requests = await_proxy_requests(proxy_task).await;
    assert_eq!(
        requests.len(),
        4,
        "warmup should try each bucket once, not --pool-size times: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.starts_with("CONNECT 203.0.113.10:443 HTTP/1.1")),
        "warmup dialled something other than the configured DC IP: {requests:?}"
    );

    // Nothing was pooled, so a subsequent get still misses.
    assert!(
        pool.get(2, false, "203.0.113.10".to_string(), false, true)
            .await
            .is_none()
    );
}

// ─── Cloudflare tiers ────────────────────────────────────────────────────────

fn worker_target(domain: &str) -> CfTarget {
    CfTarget {
        tier: CfTier::Worker,
        dc: 2,
        is_media: false,
        dst: "149.154.167.51".to_string(),
        domain: domain.to_string(),
        skip_tls_verify: false,
        connect_timeout: Duration::from_secs(2),
    }
}

#[tokio::test]
async fn a_cf_tier_that_was_never_primed_misses_without_dialling() {
    // The routing path asks the pool before it knows which domain it will
    // use, so a miss has to be free — no connect, nothing to build.
    let (proxy_addr, proxy_task) = rejecting_http_proxy().await;
    let outbound =
        OutboundConnector::from_config(Some(&format!("http://{proxy_addr}")), None, false).unwrap();
    let pool = pool_with_outbound(1, outbound);

    assert!(pool.cf_get(CfTier::Worker, 2, false).await.is_none());
    assert!(pool.cf_get(CfTier::Proxy, 2, false).await.is_none());

    assert!(
        tokio::time::timeout(Duration::from_millis(200), proxy_task)
            .await
            .is_err(),
        "a pool miss must not dial anything"
    );
}

#[tokio::test]
async fn cf_prefetch_reopens_the_worker_through_the_outbound_connector() {
    let (proxy_addr, proxy_task) = rejecting_http_proxy_requests().await;
    let outbound =
        OutboundConnector::from_config(Some(&format!("http://{proxy_addr}")), None, false).unwrap();
    let pool = pool_with_outbound(1, outbound);

    pool.cf_prefetch(worker_target("worker-prefetch.example.dev"));

    let requests = await_proxy_requests(proxy_task).await;
    assert_eq!(
        requests.len(),
        1,
        "a prefetch opens exactly one spare tunnel: {requests:?}"
    );
    assert!(
        requests[0].starts_with("CONNECT worker-prefetch.example.dev:443 HTTP/1.1"),
        "unexpected prefetch target: {requests:?}"
    );
}

#[tokio::test]
async fn cf_prefetch_does_nothing_when_pooling_is_disabled() {
    // --pool-size 0 turns pooling off for the Cloudflare tiers too, rather
    // than quietly keeping one connection per DC open on someone else's
    // Worker quota.
    let (proxy_addr, proxy_task) = rejecting_http_proxy().await;
    let outbound =
        OutboundConnector::from_config(Some(&format!("http://{proxy_addr}")), None, false).unwrap();
    let pool = pool_with_outbound(0, outbound);

    pool.cf_prefetch(worker_target("worker-disabled.example.dev"));

    assert!(
        tokio::time::timeout(Duration::from_millis(200), proxy_task)
            .await
            .is_err(),
        "pooling is disabled, so nothing should be dialled"
    );
}
