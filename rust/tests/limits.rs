use tg_ws_proxy_rs::limits::{auto_max_connections, soft_nofile_limit};

/// The floor `auto_max_connections` never goes below.
const MIN_MAX_CONNECTIONS: usize = 4;

#[test]
fn auto_max_connections_leaves_room_for_the_pool_and_runtime() {
    // 1024 − (1 listener + 4×4×2 pool + 32 runtime) = 959 → 479 connections.
    assert_eq!(auto_max_connections(1024, 4, 4), 479);
}

#[test]
fn auto_max_connections_grows_with_the_fd_limit() {
    assert!(auto_max_connections(65536, 4, 4) > auto_max_connections(1024, 4, 4));
}

#[test]
fn a_bigger_pool_reserves_fewer_client_slots() {
    // The pool's own descriptors come out of the same budget.
    assert!(auto_max_connections(1024, 16, 4) < auto_max_connections(1024, 4, 4));
}

#[test]
fn auto_max_connections_caps_an_unlimited_budget() {
    assert_eq!(auto_max_connections(usize::MAX, 4, 4), 512);
}

#[test]
fn auto_max_connections_stays_serviceable_on_a_tiny_budget() {
    // A pool that alone over-subscribes the FD limit must not yield a zero
    // (or underflowed) cap — the proxy still has to accept some clients.
    assert_eq!(auto_max_connections(64, 64, 12), MIN_MAX_CONNECTIONS);
    assert_eq!(auto_max_connections(0, 0, 0), MIN_MAX_CONNECTIONS);
}

#[test]
fn auto_max_connections_survives_an_overflowing_pool_config() {
    // `--pool-size` and the DC count are user-controlled; their product must
    // not overflow into a bogus (huge) connection cap.
    assert_eq!(
        auto_max_connections(1024, usize::MAX, usize::MAX),
        MIN_MAX_CONNECTIONS
    );
}

#[test]
fn soft_nofile_limit_reports_a_usable_budget() {
    // The exact value is host-dependent (parsed from /proc on Linux, a fixed
    // fallback elsewhere); what matters is that it yields a servable cap.
    let limit = soft_nofile_limit();

    assert!(limit > 0);
    assert!(auto_max_connections(limit, 4, 4) >= MIN_MAX_CONNECTIONS);
}
