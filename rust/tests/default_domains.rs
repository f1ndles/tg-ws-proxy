use tg_ws_proxy_rs::default_domains::{deobfuscate, fetch_default_domains_with_outbound};
use tg_ws_proxy_rs::outbound::OutboundConnector;

mod common;

use common::{await_proxy_request, install_rustls_provider, rejecting_http_proxy};

#[test]
fn deobfuscate_restores_the_real_suffix_and_shift() {
    // Every alphabetic character in the prefix is shifted back by the number
    // of alphabetic characters it contains, and `.com` maps to `.co.uk`.
    assert_eq!(deobfuscate("virkgj.com").as_deref(), Some("pclead.co.uk"));
}

#[test]
fn deobfuscate_rejects_anything_that_is_not_a_com_domain() {
    assert!(deobfuscate("example.org").is_none());
    assert!(deobfuscate("nocomhere").is_none());
    assert!(deobfuscate("").is_none());
}

#[tokio::test]
async fn default_domain_fetch_attempts_github_through_outbound_proxy_before_fallback() {
    install_rustls_provider();

    let (proxy_addr, proxy_task) = rejecting_http_proxy().await;
    let outbound =
        OutboundConnector::from_config(Some(&format!("http://{proxy_addr}")), None, false).unwrap();

    let domains = fetch_default_domains_with_outbound(&outbound).await;

    // The fetch went through the outbound proxy...
    let request = await_proxy_request(proxy_task).await;
    assert!(request.starts_with("CONNECT raw.githubusercontent.com:443 HTTP/1.1"));

    // ...and its failure fell back to the built-in list rather than leaving
    // the proxy with no CF domains at all.
    assert!(!domains.is_empty());
    assert!(
        domains.iter().all(|domain| domain.ends_with(".co.uk")),
        "fallback list should be deobfuscated, got {domains:?}"
    );
}
