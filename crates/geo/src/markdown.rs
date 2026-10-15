//! Markdown for AI agents: whether the home page answers `Accept: text/markdown` with Markdown, as
//! Cloudflare's "Markdown for Agents" does ([`MARKDOWN_FOR_AGENTS_DOC`]), or advertises a Markdown
//! version of itself (`<link rel="alternate" type="text/markdown">` or the same as a `Link`
//! header) that answers with Markdown.
//!
//! The crawler makes at most two requests per crawl for it (the home page with [`ACCEPT`], then
//! the advertised alternate when the first answer was not Markdown); [`assess`] turns what came
//! back into a [`MarkdownReport`]. A nice-to-have: a Notice at most, never in the health score.

use codoseo_core::output::{AlternateSource, MarkdownFetch, MarkdownProbe};
use serde::{Deserialize, Serialize};

use crate::declared::clean_declared;

/// Cloudflare's documentation of Markdown for Agents.
pub const MARKDOWN_FOR_AGENTS_DOC: &str =
    "https://developers.cloudflare.com/fundamentals/reference/markdown-for-agents/";
/// The `Accept` header of the Markdown request: Markdown first, HTML as the fallback.
pub const ACCEPT: &str = "text/markdown, text/html;q=0.9";
/// The crawler reads at most this much of each answer: enough to tell an empty body and to find
/// the alternate link in the HTML head.
pub const MAX_BYTES: usize = 256 * 1024;
/// Longest advertised alternate URL kept, in characters.
pub const MAX_ALTERNATE_URL: usize = 500;

/// Whether a `Content-Type` is Markdown (`text/markdown`, any parameters).
pub fn is_markdown(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|ct| {
        ct.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/markdown")
    })
}

/// The first Markdown alternate a `Link` header advertises: an entry with `rel` holding
/// `alternate` and `type` `text/markdown` (RFC 8288), its URL as written. Every value of the
/// header is passed; each may hold several comma-separated entries.
pub fn link_header_alternate<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<String> {
    for value in values {
        let mut rest = value;
        while let Some(open) = rest.find('<') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('>') else {
                break;
            };
            let target = after[..close].trim();
            let params_and_more = &after[close + 1..];
            // The parameters run to the next entry's `<`.
            let end = params_and_more.find('<').unwrap_or(params_and_more.len());
            let params = &params_and_more[..end];
            let mut rel_alternate = false;
            let mut markdown = false;
            for param in params.split([';', ',']) {
                let Some((name, value)) = param.split_once('=') else {
                    continue;
                };
                let value = value.trim().trim_matches('"').trim();
                match name.trim().to_ascii_lowercase().as_str() {
                    "rel" => {
                        rel_alternate = value
                            .split_ascii_whitespace()
                            .any(|r| r.eq_ignore_ascii_case("alternate"));
                    }
                    "type" => markdown = is_markdown(Some(value)),
                    _ => {}
                }
            }
            if rel_alternate && markdown && !target.is_empty() {
                return Some(target.to_owned());
            }
            rest = params_and_more;
        }
    }
    None
}

/// What the Markdown requests came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkdownState {
    /// The home page answered `Accept: text/markdown` with Markdown.
    Negotiated,
    /// The home page's advertised Markdown alternate answered with Markdown.
    Alternate,
    /// Neither: HTML only, an error status, an empty body.
    NotServed,
    /// The home page gave no answer at all: nothing to judge.
    Unreachable,
}

/// The Markdown requests as a report keeps them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownReport {
    pub state: MarkdownState,
    pub negotiated: MarkdownFetch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate: Option<(String, AlternateSource)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_fetch: Option<MarkdownFetch>,
}

impl MarkdownReport {
    /// It serves Markdown one way or the other.
    pub fn served(&self) -> bool {
        matches!(
            self.state,
            MarkdownState::Negotiated | MarkdownState::Alternate
        )
    }

    /// What came back, in words: `HTTP 200 text/html`, `HTTP 406`, `no answer (timeout)`, plus
    /// the alternate's when one was asked for.
    pub fn describe(&self) -> String {
        let mut out = describe_fetch(&self.negotiated);
        match (&self.alternate, &self.alternate_fetch) {
            (Some((url, _)), Some(f)) => {
                out.push_str(&format!(
                    "; its Markdown alternate {url} answered {}",
                    describe_fetch(f)
                ));
            }
            (Some((url, _)), None) => {
                out.push_str(&format!(
                    "; it advertises a Markdown alternate at {url}, not requested (another site, or robots.txt disallows it)"
                ));
            }
            _ => {}
        }
        out
    }
}

/// `HTTP 200 text/html`, `HTTP 200 text/markdown (empty)`, `no answer (request timed out)`.
pub fn describe_fetch(f: &MarkdownFetch) -> String {
    if f.status == 0 {
        return match &f.error {
            Some(e) => format!("no answer ({e})"),
            None => "no answer".to_owned(),
        };
    }
    let mut out = format!("HTTP {}", f.status);
    if let Some(ct) = &f.content_type {
        let mime = ct.split(';').next().unwrap_or("").trim();
        if !mime.is_empty() {
            out.push(' ');
            out.push_str(mime);
        }
    }
    if (200..300).contains(&f.status) && f.body_bytes == 0 {
        out.push_str(" (empty)");
    }
    out
}

fn serves_markdown(f: &MarkdownFetch) -> bool {
    (200..300).contains(&f.status) && is_markdown(f.content_type.as_deref()) && f.body_bytes > 0
}

fn cleaned(f: &MarkdownFetch) -> MarkdownFetch {
    MarkdownFetch {
        url: f.url.chars().take(MAX_ALTERNATE_URL).collect(),
        status: f.status,
        error: f.error.as_deref().map(clean_declared),
        content_type: f.content_type.as_deref().map(clean_declared),
        body_bytes: f.body_bytes,
    }
}

/// The report of the Markdown requests: served by content negotiation, by the advertised
/// alternate, not served, or the home page could not be reached.
pub fn assess(p: &MarkdownProbe) -> MarkdownReport {
    let state = if p.negotiated.status == 0 {
        MarkdownState::Unreachable
    } else if serves_markdown(&p.negotiated) {
        MarkdownState::Negotiated
    } else if p.alternate_fetch.as_ref().is_some_and(serves_markdown) {
        MarkdownState::Alternate
    } else {
        MarkdownState::NotServed
    };
    MarkdownReport {
        state,
        negotiated: cleaned(&p.negotiated),
        alternate: p.alternate.as_ref().map(|(url, source)| {
            (
                clean_declared(url)
                    .chars()
                    .take(MAX_ALTERNATE_URL)
                    .collect(),
                *source,
            )
        }),
        alternate_fetch: p.alternate_fetch.as_ref().map(cleaned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_content_types() {
        assert!(is_markdown(Some("text/markdown")));
        assert!(is_markdown(Some("Text/Markdown; charset=utf-8")));
        assert!(!is_markdown(Some("text/html")));
        assert!(!is_markdown(Some("text/plain")));
        assert!(!is_markdown(None));
    }

    #[test]
    fn link_headers() {
        assert_eq!(
            link_header_alternate([r#"</index.md>; rel="alternate"; type="text/markdown""#]),
            Some("/index.md".to_owned())
        );
        assert_eq!(
            link_header_alternate([
                r#"</style.css>; rel=preload; as=style, <https://e.com/a.md>; rel="alternate canonical"; type="text/markdown; charset=utf-8""#
            ]),
            Some("https://e.com/a.md".to_owned())
        );
        assert_eq!(
            link_header_alternate([r#"</fr/>; rel="alternate"; hreflang="fr""#]),
            None
        );
        assert_eq!(
            link_header_alternate(["</a.md>; type=\"text/markdown\""]),
            None
        );
        assert_eq!(link_header_alternate(["garbage <unclosed"]), None);
    }
}
