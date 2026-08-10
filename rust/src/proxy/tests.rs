use super::*;

use crate::crypto::HANDSHAKE_LEN;

#[test]
fn balanced_rotates_the_starting_domain() {
    let counter = AtomicUsize::new(0);
    let workers = vec![
        "w1.example.workers.dev".to_string(),
        "w2.example.workers.dev".to_string(),
        "w3.example.workers.dev".to_string(),
    ];

    assert_eq!(
        balanced(&workers, &counter),
        ["w1", "w2", "w3"].map(|w| format!("{w}.example.workers.dev"))
    );
    assert_eq!(
        balanced(&workers, &counter),
        ["w2", "w3", "w1"].map(|w| format!("{w}.example.workers.dev"))
    );
    assert_eq!(
        balanced(&workers, &counter),
        ["w3", "w1", "w2"].map(|w| format!("{w}.example.workers.dev"))
    );
}

#[test]
fn balanced_leaves_short_lists_untouched() {
    let counter = AtomicUsize::new(0);
    let single = vec!["only.example".to_string()];

    assert_eq!(balanced(&[], &counter), Vec::<String>::new());
    assert_eq!(balanced(&single, &counter), single);
    // A list that cannot be rotated must not consume a counter tick either.
    assert_eq!(counter.load(Ordering::Relaxed), 0);
}

#[test]
fn balance_order_borrows_when_balancing_is_disabled() {
    let counter = AtomicUsize::new(0);
    let domains = vec!["a.example".to_string(), "b.example".to_string()];

    assert!(matches!(
        balance_order(&domains, false, &counter),
        Cow::Borrowed(_)
    ));
    assert!(matches!(
        balance_order(&domains, true, &counter),
        Cow::Owned(_)
    ));
}

#[test]
fn cooldown_is_tracked_per_key() {
    let cooldowns: CooldownMap<String> = CooldownMap::new();

    assert!(!cooldowns.active("w1.example.workers.dev"));

    cooldowns.set(
        "w1.example.workers.dev".to_string(),
        Duration::from_secs(60),
    );
    assert!(cooldowns.active("w1.example.workers.dev"));
    assert!(!cooldowns.active("w2.example.workers.dev"));

    cooldowns.clear("w1.example.workers.dev");
    assert!(!cooldowns.active("w1.example.workers.dev"));
}

#[test]
fn cooldown_expires_once_the_window_passes() {
    let cooldowns: CooldownMap<(u32, bool)> = CooldownMap::new();

    cooldowns.set((2, false), Duration::ZERO);

    assert!(!cooldowns.active(&(2, false)));
}

#[test]
fn cooldown_keys_include_the_media_flag() {
    let cooldowns: CooldownMap<(u32, bool)> = CooldownMap::new();

    cooldowns.set((2, false), Duration::from_secs(60));

    assert!(cooldowns.active(&(2, false)));
    // The media flag is part of the key — a non-media cooldown must not leak
    // into the media bucket for the same DC.
    assert!(!cooldowns.active(&(2, true)));
}

#[test]
fn upstream_key_joins_host_and_port() {
    assert_eq!(upstream_key("proxy.example", 443), "proxy.example:443");
}

#[test]
fn human_bytes_scales_through_the_units() {
    assert_eq!(human_bytes(0), "0.0B");
    assert_eq!(human_bytes(1023), "1023.0B");
    assert_eq!(human_bytes(1024), "1.0KB");
    assert_eq!(human_bytes(1024 * 1024), "1.0MB");
    assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0GB");
}

#[tokio::test]
async fn an_aborted_direction_still_reports_the_bytes_it_moved() {
    // Regression test: the directions used to return their total, and
    // `join_bridge` cancels whichever one is still running — so the cancelled
    // side's count was lost and every session logged one direction as zero.
    let counters = Arc::new(BridgeCounters::default());

    let upload = tokio::spawn({
        let counters = Arc::clone(&counters);
        async move {
            counters.add_up(4096);
            // Finishes first, which is what triggers the abort below.
        }
    });

    let download = tokio::spawn({
        let counters = Arc::clone(&counters);
        async move {
            counters.add_down(1024);
            // Still running when the peer finishes, so this task is aborted
            // partway — exactly the case that used to lose the count.
            std::future::pending::<()>().await;
            counters.add_down(999_999);
        }
    });

    let closed_by = join_bridge(upload, download).await;

    assert_eq!(counters.totals(), (4096, 1024));
    // The upload direction ran out first, which means the client stopped.
    assert!(matches!(closed_by, ClosedBy::Client));
}

#[tokio::test]
async fn the_side_that_stops_first_is_the_one_reported() {
    // A session that transfers nothing looks identical whichever side hung
    // up; this is what tells a client-side timeout apart from an upstream
    // that dropped us right after the handshake.
    let upload = tokio::spawn(std::future::pending::<()>());
    let download = tokio::spawn(async {});
    assert!(matches!(
        join_bridge(upload, download).await,
        ClosedBy::Upstream
    ));

    let upload = tokio::spawn(async {});
    let download = tokio::spawn(std::future::pending::<()>());
    assert!(matches!(
        join_bridge(upload, download).await,
        ClosedBy::Client
    ));
}

#[test]
fn bridge_counters_accumulate_across_many_chunks() {
    let counters = BridgeCounters::default();

    for _ in 0..10 {
        counters.add_up(100);
        counters.add_down(250);
    }

    assert_eq!(counters.totals(), (1000, 2500));
}

// ─── WebSocket framing ───────────────────────────────────────────────────────

/// Bridge one client payload through `bridge_ws` and report the size of every
/// WebSocket message the upstream received.
///
/// Both ends are plain TCP: the framing decision under test happens above the
/// transport, so wrapping the stream in `MaybeTlsStream::Plain` exercises the
/// same code a real TLS connection would.
async fn upstream_frame_sizes(framing: WsFraming, payload: &[u8]) -> Vec<usize> {
    use tokio::net::TcpListener;
    use tokio_tungstenite::{MaybeTlsStream, accept_async, client_async};

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    let sizes = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let mut sizes = Vec::new();

        while let Some(Ok(message)) = ws.next().await {
            match message {
                Message::Binary(data) => sizes.push(data.len()),
                Message::Close(_) => break,
                _ => {}
            }
        }

        sizes
    });

    let tcp = TcpStream::connect(upstream_addr).await.unwrap();
    let (ws, _) = client_async("ws://127.0.0.1/apiws", MaybeTlsStream::Plain(tcp))
        .await
        .unwrap();

    // The client half of the bridge, driven from this test.
    let clients = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let clients_addr = clients.local_addr().unwrap();
    let mut client = TcpStream::connect(clients_addr).await.unwrap();
    let (server, _) = clients.accept().await.unwrap();
    let (reader, writer) = tokio::io::split(server);

    let relay_init = generate_relay_init(ProtoTag::PaddedIntermediate, 2);
    let ciphers = build_connection_ciphers(&[0u8; 48], &[0u8; 32], &relay_init);

    let bridge = tokio::spawn(async move {
        bridge_ws(
            ClientReader::Plain(reader),
            ClientWriter::Plain(writer),
            WsBridgeParams {
                label: "test",
                ws,
                framing,
                relay_init,
                ciphers,
                proto: ProtoTag::PaddedIntermediate,
                dc: 2,
                is_media: false,
            },
        )
        .await;
    });

    client.write_all(payload).await.unwrap();
    drop(client);
    bridge.await.unwrap();

    sizes.await.unwrap()
}

#[tokio::test]
async fn a_worker_tunnel_never_emits_an_oversized_frame() {
    // Regression for the upload bug upstream fixed in v1.9.1: packet-aligning
    // a Worker tunnel produces one WebSocket message per MTProto packet, and a
    // media upload's packets run past Cloudflare's 1 MiB message cap, which
    // kills the connection mid-transfer. A tunnel is a raw TCP socket at the
    // far end, so it is chunked by client reads instead — and the relay init
    // still goes out as its own first frame.
    const CF_MESSAGE_CAP: usize = 1024 * 1024;
    let payload = vec![0xA5; 3 * CF_MESSAGE_CAP];

    let sizes = upstream_frame_sizes(WsFraming::Tunnel, &payload).await;

    assert_eq!(sizes.first(), Some(&HANDSHAKE_LEN));
    assert!(
        sizes[1..].iter().all(|size| *size <= CLIENT_READ_BUF_SIZE),
        "a tunnel frame must not exceed one client read: {sizes:?}"
    );
    assert_eq!(
        sizes[1..].iter().sum::<usize>(),
        payload.len(),
        "every byte the client sent has to reach the upstream: {sizes:?}"
    );
}
