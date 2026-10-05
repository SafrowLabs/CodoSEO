//! Abuse controls for the public audit (spec section 8): who is asking (the client address and
//! its daily-salted hash) and which email domains are throwaways. The limits themselves live in
//! the store, next to the data they count; Turnstile is in [`crate::turnstile`].

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use sha2::{Digest, Sha256};
use time::Date;

use crate::config::{Config, Mode};
use crate::state::AppState;

/// The visitor's address as far as limits are concerned. `None` only when it can't be told
/// (no proxy header and no socket address, which doesn't happen when serving), in which case
/// the per-IP limits don't apply.
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub Option<IpAddr>);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<ClientIp, Infallible> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        Ok(ClientIp(client_ip(&parts.headers, peer, &state.config)))
    }
}

/// In the cloud, the address the proxy in front of us reports (`CLIENT_IP_HEADER`, by default
/// `CF-Connecting-IP`); everywhere else, and when that header is missing or isn't an address,
/// the socket's peer. Self-hosted instances never trust a client-supplied header.
///
/// The cloud must only be reachable through Cloudflare for the header to be trustworthy.
pub fn client_ip(headers: &HeaderMap, peer: Option<IpAddr>, config: &Config) -> Option<IpAddr> {
    if config.mode == Mode::Cloud
        && let Some(ip) = headers
            .get(config.client_ip_header.as_str())
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    {
        return Some(ip);
    }
    peer
}

/// What is stored on a crawl instead of the visitor's address: SHA-256 over a salt that
/// changes every UTC day (derived from the instance secret), then the address. Yesterday's
/// hashes can't be matched to today's, so the counters can't build a history of a person.
/// IPv6 addresses count by their /64, since a person controls every address in it.
pub fn ip_hash(secret: &str, ip: IpAddr, day: Date) -> Vec<u8> {
    let salt = Sha256::digest(format!("codoseo.ip-salt:{secret}:{day}").as_bytes());
    let mut hash = Sha256::new();
    hash.update(salt);
    hash.update(normalise(ip).as_bytes());
    hash.finalize().to_vec()
}

fn normalise(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

/// Throwaway-inbox providers. One free account per email only means something if an email
/// costs more than a click. Matches the domain and any subdomain of it.
const DISPOSABLE: &[&str] = &[
    "10minutemail.co.uk",
    "10minutemail.com",
    "10minutemail.net",
    "20minutemail.com",
    "anonbox.net",
    "burnermail.io",
    "byom.de",
    "crazymailing.com",
    "discard.email",
    "discardmail.com",
    "dispostable.com",
    "emailfake.com",
    "emailondeck.com",
    "fakeinbox.com",
    "fakemail.net",
    "fexpost.com",
    "getairmail.com",
    "getnada.com",
    "grr.la",
    "guerrillamail.biz",
    "guerrillamail.com",
    "guerrillamail.de",
    "guerrillamail.net",
    "guerrillamail.org",
    "guerrillamailblock.com",
    "harakirimail.com",
    "inboxkitten.com",
    "jetable.org",
    "mail.tm",
    "mailcatch.com",
    "maildrop.cc",
    "mailforspam.com",
    "mailinator.com",
    "mailnesia.com",
    "mailto.plus",
    "minuteinbox.com",
    "mintemail.com",
    "moakt.com",
    "mohmal.com",
    "mytemp.email",
    "nada.email",
    "owlymail.com",
    "sharklasers.com",
    "spam4.me",
    "spamgourmet.com",
    "temp-mail.io",
    "temp-mail.org",
    "tempinbox.com",
    "tempmail.com",
    "tempmail.net",
    "tempmailo.com",
    "tempr.email",
    "throwawaymail.com",
    "tmail.ws",
    "trash-mail.com",
    "trashmail.com",
    "trashmail.de",
    "trashmail.net",
    "trbvm.com",
    "yopmail.com",
    "yopmail.fr",
    "yopmail.net",
];

pub fn is_disposable(address: &str) -> bool {
    let domain = address
        .rsplit_once('@')
        .map_or(address, |(_, d)| d)
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    DISPOSABLE
        .iter()
        .any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
}

/// `about 59 minutes`, `about 22 hours`: how long until a limit lifts.
pub fn wait_text(secs: i64) -> String {
    if secs < 90 * 60 {
        let minutes = (secs + 59) / 60;
        format!(
            "about {} minute{}",
            minutes.max(1),
            if minutes == 1 { "" } else { "s" }
        )
    } else {
        let hours = (secs + 3599) / 3600;
        format!("about {hours} hours")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn cloud() -> Config {
        Config::from_lookup(|k| match k {
            "CODOSEO_MODE" => Some("cloud".into()),
            "BASE_URL" => Some("https://codoseo.com".into()),
            "SECRET_KEY" => Some("k".into()),
            "SMTP_URL" => Some("smtp://127.0.0.1:2525".into()),
            _ => None,
        })
        .unwrap()
    }

    fn with_header(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("cf-connecting-ip", HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn the_cloud_trusts_its_proxy_header_and_nothing_else() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let cfg = cloud();
        assert_eq!(
            client_ip(&with_header("203.0.113.9"), Some(peer), &cfg),
            Some("203.0.113.9".parse().unwrap())
        );
        assert_eq!(
            client_ip(&with_header(" 2001:db8::1 "), Some(peer), &cfg),
            Some("2001:db8::1".parse().unwrap())
        );
        // Garbage or a missing header falls back to the socket.
        assert_eq!(
            client_ip(&with_header("not an ip"), Some(peer), &cfg),
            Some(peer)
        );
        assert_eq!(client_ip(&HeaderMap::new(), Some(peer), &cfg), Some(peer));
        // `X-Forwarded-For` is the client's to forge: never read.
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        assert_eq!(client_ip(&h, Some(peer), &cfg), Some(peer));
        assert_eq!(client_ip(&HeaderMap::new(), None, &cfg), None);
    }

    #[test]
    fn self_hosted_ignores_the_header() {
        let peer: IpAddr = "192.168.1.5".parse().unwrap();
        let cfg = Config::for_tests();
        assert_eq!(
            client_ip(&with_header("203.0.113.9"), Some(peer), &cfg),
            Some(peer)
        );
    }

    #[test]
    fn hashes_depend_on_secret_day_and_address() {
        let day = Date::from_calendar_date(2026, time::Month::October, 5).unwrap();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        let h = ip_hash("s", ip, day);
        assert_eq!(h.len(), 32);
        assert_eq!(h, ip_hash("s", ip, day));
        assert_ne!(h, ip_hash("s", ip, day.next_day().unwrap()));
        assert_ne!(h, ip_hash("t", ip, day));
        assert_ne!(h, ip_hash("s", "203.0.113.10".parse().unwrap(), day));
    }

    #[test]
    fn ipv6_counts_by_its_64_and_mapped_v4_by_its_v4() {
        let day = Date::from_calendar_date(2026, time::Month::October, 5).unwrap();
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::ffff".parse().unwrap();
        let other: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(ip_hash("s", a, day), ip_hash("s", b, day));
        assert_ne!(ip_hash("s", a, day), ip_hash("s", other, day));
        assert_eq!(
            ip_hash("s", "::ffff:203.0.113.9".parse().unwrap(), day),
            ip_hash("s", "203.0.113.9".parse().unwrap(), day)
        );
    }

    #[test]
    fn disposable_domains_match_subdomains_but_not_lookalikes() {
        for bad in [
            "a@mailinator.com",
            "A@MAILINATOR.COM",
            "a@x.mailinator.com",
            "a@yopmail.com.",
        ] {
            assert!(is_disposable(bad), "{bad}");
        }
        for ok in [
            "a@gmail.com",
            "a@notmailinator.com",
            "a@mailinator.example.org",
            "a@company.io",
        ] {
            assert!(!is_disposable(ok), "{ok}");
        }
    }

    #[test]
    fn waits_read_naturally() {
        assert_eq!(wait_text(1), "about 1 minute");
        assert_eq!(wait_text(3590), "about 60 minutes");
        assert_eq!(wait_text(5400), "about 2 hours");
        assert_eq!(wait_text(80_000), "about 23 hours");
    }
}
