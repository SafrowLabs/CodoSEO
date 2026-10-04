//! URL normalisation, so the same page always maps to the same URL and hash.

use ::url::{Host, Url};
use xxhash_rust::xxh3::xxh3_64;

/// Resolves `href` against `base` and normalises it. Returns `None` for links that
/// aren't crawlable pages: non-HTTP schemes, fragment-only links and blanks.
///
/// On top of what the URL parser already does (lowercase scheme and host, punycode,
/// default ports, dot segments), this drops the fragment and a trailing dot on the
/// host, upper-cases percent-escapes and decodes escaped unreserved characters.
/// An empty query (`/a?`) is dropped; other query strings are kept as written.
pub fn normalize(base: &Url, href: &str) -> Option<Url> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let mut url = base.join(href).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);

    if let Some(Host::Domain(domain)) = url.host() {
        if domain.is_empty() {
            return None;
        }
        if let Some(stripped) = domain.strip_suffix('.') {
            let stripped = stripped.to_owned();
            url.set_host(Some(&stripped)).ok()?;
        }
    } else if url.host().is_none() {
        return None;
    }

    let path = normalize_escapes(url.path());
    if path != url.path() {
        url.set_path(&path);
    }
    if let Some(query) = url.query() {
        let query = normalize_escapes(query);
        if Some(query.as_str()) != url.query() {
            url.set_query(Some(&query));
        }
    }
    if url.query() == Some("") {
        url.set_query(None);
    }
    Some(url)
}

/// Stable 64-bit hash of a normalised URL.
pub fn url_hash(url: &Url) -> u64 {
    xxh3_64(url.as_str().as_bytes())
}

fn normalize_escapes(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let (hi, lo) = (bytes[i + 1], bytes[i + 2]);
            if let (Some(h), Some(l)) = (hex_value(hi), hex_value(lo)) {
                let decoded = h * 16 + l;
                if is_unreserved(decoded) {
                    out.push(decoded as char);
                } else {
                    out.push('%');
                    out.push(hi.to_ascii_uppercase() as char);
                    out.push(lo.to_ascii_uppercase() as char);
                }
                i += 3;
                continue;
            }
        }
        // The input is a valid &str, so copying char by char keeps it valid.
        let ch = s[i..].chars().next().expect("index is on a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}
