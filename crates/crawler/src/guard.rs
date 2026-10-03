//! Keeps the cloud crawler away from private and internal addresses.
//!
//! Two layers, because reqwest never calls the resolver for IP-literal hosts:
//! [`check_url`] rejects blocked IP literals before a request is made, and
//! [`GuardedResolver`] drops blocked addresses from DNS answers. The fetcher
//! re-runs [`check_url`] on every redirect hop.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use codoseo_core::crawl::AddressPolicy;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    #[error("{0} is a private or internal address")]
    BlockedAddress(IpAddr),
    #[error("{host} only resolves to private or internal addresses")]
    OnlyBlockedAddresses { host: String },
    #[error("could not resolve {host}: {reason}")]
    Lookup { host: String, reason: String },
}

/// True for addresses the cloud crawler must never connect to.
pub fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    a == 0 // "this network", including 0.0.0.0
        || ip.is_private() // 10/8, 172.16/12, 192.168/16
        || ip.is_loopback() // 127/8
        || ip.is_link_local() // 169.254/16, includes cloud metadata
        || (a == 100 && (b & 0xC0) == 64) // 100.64/10 carrier-grade NAT
        || (a == 192 && b == 0 && c == 0) // 192.0.0/24 protocol assignments
        || ip.is_documentation()
        || (a == 198 && (b == 18 || b == 19)) // 198.18/15 benchmarking
        || ip.is_multicast() // 224/4
        || a >= 240 // 240/4 reserved, includes broadcast
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = embedded_v4(ip) {
        return is_blocked_v4(v4);
    }
    let s = ip.segments();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unique_local() // fc00::/7
        || ip.is_unicast_link_local() // fe80::/10
        || (s[0] & 0xffc0) == 0xfec0 // fec0::/10 deprecated site-local
        || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32 documentation
        || (s[0] == 0x2001 && s[1] == 0x0000) // 2001::/32 Teredo
        || (s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0x0001) // 64:ff9b:1::/48 local NAT64
        || (s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0) // 100::/64 discard
}

/// IPv4 addresses carried inside IPv6 ones: mapped, compatible, NAT64 and 6to4.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let tail =
        |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    if s[..6].iter().all(|&x| x == 0) && !(s[6] == 0 && s[7] <= 1) {
        return Some(tail(s[6], s[7])); // ::a.b.c.d (but not :: or ::1)
    }
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0) {
        return Some(tail(s[6], s[7])); // 64:ff9b::/96
    }
    if s[0] == 0x2002 {
        return Some(tail(s[1], s[2])); // 2002::/16 6to4
    }
    None
}

/// Rejects URLs whose host is a blocked IP literal. Domain names are checked
/// later, by [`GuardedResolver`].
pub fn check_url(url: &Url, policy: AddressPolicy) -> Result<(), GuardError> {
    if policy == AddressPolicy::AllowPrivate {
        return Ok(());
    }
    let ip = match url.host() {
        Some(Host::Ipv4(v4)) => IpAddr::V4(v4),
        Some(Host::Ipv6(v6)) => IpAddr::V6(v6),
        _ => return Ok(()),
    };
    if is_blocked(ip) {
        return Err(GuardError::BlockedAddress(ip));
    }
    Ok(())
}

/// Turns a host name into IP addresses.
pub trait Lookup: Send + Sync + 'static {
    fn lookup(&self, host: &str) -> impl Future<Output = io::Result<Vec<IpAddr>>> + Send;
}

/// The operating system's resolver.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemLookup;

impl Lookup for SystemLookup {
    fn lookup(&self, host: &str) -> impl Future<Output = io::Result<Vec<IpAddr>>> + Send {
        let host = host.to_owned();
        async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        }
    }
}

/// A reqwest resolver that only returns addresses allowed by [`is_blocked`].
pub struct GuardedResolver<L: Lookup> {
    lookup: Arc<L>,
}

impl<L: Lookup> GuardedResolver<L> {
    pub fn new(lookup: L) -> Self {
        GuardedResolver {
            lookup: Arc::new(lookup),
        }
    }

    pub async fn lookup_checked(&self, host: &str) -> Result<Vec<SocketAddr>, GuardError> {
        checked(self.lookup.as_ref(), host).await
    }
}

async fn checked<L: Lookup>(lookup: &L, host: &str) -> Result<Vec<SocketAddr>, GuardError> {
    let ips = lookup.lookup(host).await.map_err(|e| GuardError::Lookup {
        host: host.to_owned(),
        reason: e.to_string(),
    })?;
    let allowed: Vec<SocketAddr> = ips
        .into_iter()
        .filter(|ip| !is_blocked(*ip))
        .map(|ip| SocketAddr::new(ip, 0))
        .collect();
    if allowed.is_empty() {
        return Err(GuardError::OnlyBlockedAddresses {
            host: host.to_owned(),
        });
    }
    Ok(allowed)
}

impl<L: Lookup> Resolve for GuardedResolver<L> {
    fn resolve(&self, name: Name) -> Resolving {
        let lookup = Arc::clone(&self.lookup);
        Box::pin(async move {
            let addrs = checked(lookup.as_ref(), name.as_str()).await?;
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}
