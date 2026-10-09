//! Parsers for the AI-use preferences a site declares outside the allow/deny rules:
//! Cloudflare's `Content-Signal`, the IETF AIPREF `Content-Usage`, and TDMRep.
//!
//! The same value syntax is used in robots.txt lines and in HTTP response headers, so
//! the parsers take just the value.

use serde::{Deserialize, Serialize};

/// Keys Cloudflare's Content Signals policy defines.
pub const SIGNAL_KEYS: &[&str] = &["search", "ai-input", "ai-train", "use"];
/// Keys the AIPREF attach draft defines for `Content-Usage`.
pub const USAGE_KEYS: &[&str] = &["train-ai", "ai-use", "search"];

pub fn is_known_signal_key(key: &str) -> bool {
    SIGNAL_KEYS.contains(&key)
}

pub fn is_known_usage_key(key: &str) -> bool {
    USAGE_KEYS.contains(&key)
}

/// Longest declared string kept, in characters.
pub const MAX_DECLARED_TEXT: usize = 512;

/// A declared string as it is kept: trimmed, without control characters (a stray `\r` or escape
/// would otherwise reach change rows and alerts) and cut to [`MAX_DECLARED_TEXT`] characters.
pub fn clean_declared(s: &str) -> String {
    s.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DECLARED_TEXT)
        .collect()
}

/// `search=yes, ai-train=no` → `[("search","yes"), ("ai-train","no")]`. Keys are
/// lower-cased, values are kept as written (trimmed). Pieces without `=` are skipped.
pub fn parse_content_signal(value: &str) -> Vec<(String, String)> {
    pairs(split_pieces(&tighten_equals(value)))
}

/// `[/path] key=value …` → the optional path (starts with `/`) and the pairs, with the
/// same key and value handling as [`parse_content_signal`].
pub fn parse_content_usage(value: &str) -> (Option<String>, Vec<(String, String)>) {
    let mut path = None;
    let mut rest = Vec::new();
    let value = tighten_equals(value);
    for piece in split_pieces(&value) {
        if path.is_none() && rest.is_empty() && piece.starts_with('/') {
            path = Some(piece.to_owned());
        } else {
            rest.push(piece);
        }
    }
    (path, pairs(rest.into_iter()))
}

fn split_pieces(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
}

/// Drops whitespace around every `=`, so `search = yes` reads as `search=yes`.
fn tighten_equals(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_space = false;
    let mut after_equals = false;
    for c in value.chars() {
        if c.is_whitespace() {
            if !after_equals {
                pending_space = true;
            }
            continue;
        }
        if c == '=' {
            pending_space = false;
            after_equals = true;
        } else {
            if pending_space {
                out.push(' ');
            }
            pending_space = false;
            after_equals = false;
        }
        out.push(c);
    }
    out
}

fn pairs<'a>(pieces: impl Iterator<Item = &'a str>) -> Vec<(String, String)> {
    pieces
        .filter_map(|piece| {
            let (key, value) = piece.split_once('=')?;
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            (!key.is_empty() && !value.is_empty()).then(|| (key, value.to_owned()))
        })
        .collect()
}

/// One object of `/.well-known/tdmrep.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TdmRepEntry {
    pub location: String,
    /// `tdm-reservation`: 1 reserves text-and-data-mining rights, 0 does not.
    pub reservation: Option<u8>,
    /// `tdm-policy`: a URL of the licensing policy.
    pub policy: Option<String>,
}

/// Parses a TDMRep file: a JSON array of objects with `location`, `tdm-reservation`
/// (0 or 1) and an optional `tdm-policy`. Objects without a `location` string are an
/// error, as is a reservation other than 0 or 1.
pub fn parse_tdmrep(body: &str) -> Result<Vec<TdmRepEntry>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("not valid JSON: {e}"))?;
    let items = value.as_array().ok_or("expected a JSON array")?;
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let obj = item
                .as_object()
                .ok_or_else(|| format!("entry {i} is not an object"))?;
            let location = obj
                .get("location")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("entry {i} has no `location` string"))?
                .to_owned();
            let reservation = match obj.get("tdm-reservation") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => match v
                    .as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                {
                    Some(n @ (0 | 1)) => Some(n as u8),
                    _ => return Err(format!("entry {i}: `tdm-reservation` must be 0 or 1")),
                },
            };
            let policy = obj
                .get("tdm-policy")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            Ok(TdmRepEntry {
                location,
                reservation,
                policy,
            })
        })
        .collect()
}
