use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::IpAddr;

use codoseo_core::Url;
use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::guard::{GuardError, GuardedResolver, Lookup, check_url, is_blocked};
use reqwest::dns::Resolve;

#[test]
fn private_and_internal_addresses_are_blocked() {
    for ip in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.1",
        "169.254.169.254",
        "0.0.0.0",
        "100.64.0.1",
        "192.0.0.8",
        "198.18.0.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::1",
        "::",
        "::127.0.0.1",
        "fc00::1",
        "fd12::1",
        "fe80::1",
        "ff02::1",
        "2001:db8::1",
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.1",
        "64:ff9b::a00:1",
        "2002:7f00:1::",
    ] {
        let parsed: IpAddr = ip.parse().unwrap();
        assert!(is_blocked(parsed), "{ip} should be blocked");
    }
}

#[test]
fn public_addresses_are_allowed() {
    for ip in [
        "8.8.8.8",
        "172.32.0.1",
        "100.128.0.1",
        "2606:4700::1111",
        "64:ff9b::808:808",
    ] {
        let parsed: IpAddr = ip.parse().unwrap();
        assert!(!is_blocked(parsed), "{ip} should be allowed");
    }
}

#[test]
fn ip_literal_hosts_are_checked_directly() {
    let public = AddressPolicy::Public;
    for url in [
        "http://2130706433/",
        "http://0x7f.1/",
        "http://[::1]/",
        "http://10.0.0.1:8080/x",
    ] {
        let err = check_url(&Url::parse(url).unwrap(), public).unwrap_err();
        assert!(
            matches!(err, GuardError::BlockedAddress(_)),
            "{url}: {err:?}"
        );
    }
    assert!(check_url(&Url::parse("http://8.8.8.8/").unwrap(), public).is_ok());
    assert!(check_url(&Url::parse("https://example.com/").unwrap(), public).is_ok());
}

#[test]
fn allow_private_lets_everything_through() {
    let url = Url::parse("http://127.0.0.1:3000/").unwrap();
    assert!(check_url(&url, AddressPolicy::AllowPrivate).is_ok());
}

struct StubLookup(HashMap<&'static str, Vec<IpAddr>>);

impl StubLookup {
    fn new(entries: &[(&'static str, &[&str])]) -> Self {
        StubLookup(
            entries
                .iter()
                .map(|(host, ips)| (*host, ips.iter().map(|ip| ip.parse().unwrap()).collect()))
                .collect(),
        )
    }
}

impl Lookup for StubLookup {
    fn lookup(&self, host: &str) -> impl Future<Output = io::Result<Vec<IpAddr>>> + Send {
        let found = self.0.get(host).cloned();
        async move { found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host")) }
    }
}

fn resolver() -> GuardedResolver<StubLookup> {
    GuardedResolver::new(StubLookup::new(&[
        ("evil.test", &["127.0.0.1"]),
        ("ok.test", &["93.184.216.34"]),
        ("mixed.test", &["10.0.0.5", "93.184.216.34"]),
    ]))
}

#[tokio::test]
async fn names_resolving_only_to_blocked_addresses_fail() {
    let err = resolver().lookup_checked("evil.test").await.unwrap_err();
    assert!(
        matches!(err, GuardError::OnlyBlockedAddresses { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn blocked_addresses_are_dropped_from_mixed_answers() {
    let addrs = resolver().lookup_checked("mixed.test").await.unwrap();
    assert_eq!(
        addrs.iter().map(|a| a.ip().to_string()).collect::<Vec<_>>(),
        ["93.184.216.34"]
    );
}

#[tokio::test]
async fn lookup_failures_are_reported() {
    let err = resolver().lookup_checked("missing.test").await.unwrap_err();
    assert!(matches!(err, GuardError::Lookup { .. }), "{err:?}");
}

#[tokio::test]
async fn works_as_a_reqwest_resolver() {
    let r = resolver();
    let ok: Vec<_> = r
        .resolve("ok.test".parse().unwrap())
        .await
        .unwrap()
        .collect();
    assert_eq!(ok.len(), 1);
    assert!(r.resolve("evil.test".parse().unwrap()).await.is_err());
}
