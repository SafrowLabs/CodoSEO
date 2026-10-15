//! Markdown for AI agents: the home page asked for with `Accept: text/markdown`, and, when that is
//! not Markdown, the Markdown alternate it advertises. At most two requests per crawl, both
//! through the crawl's limiter, kept to robots.txt and ended by the deadline. What they mean is in
//! [`codoseo_geo::markdown`].

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use codoseo_core::output::{AlternateSource, MarkdownFetch, MarkdownProbe};
use codoseo_geo::markdown::{
    ACCEPT, MAX_ALTERNATE_URL, MAX_BYTES, is_markdown, link_header_alternate,
};
use lol_html::{HtmlRewriter, Settings, element};
use reqwest::header::{HeaderMap, HeaderValue, LINK};
use tokio::time::{Instant, timeout_at};
use url::Url;

use crate::fetch::{FetchError, FetchResult, Fetcher, is_html};
use crate::politeness::{Limiter, retry_after_of};
use crate::robots::RobotsRules;

/// The longest a Markdown request waits for its turn at the site.
const MAX_SLOT_WAIT: Duration = Duration::from_secs(3);

/// Asks for `home` as Markdown, then for the alternate it advertises when the answer was not
/// Markdown. `None` when nothing was asked: robots.txt disallows the home page, the address is
/// refused, or the deadline came first. A request that got no response is kept as status 0.
pub(crate) async fn fetch_markdown(
    fetcher: &Fetcher,
    limiter: &Limiter,
    home: &Url,
    rules: &RobotsRules,
    deadline: Instant,
) -> Option<MarkdownProbe> {
    if !rules.allowed(home.as_str()) {
        return None;
    }
    let (negotiated, res) = fetch_one(fetcher, limiter, home, deadline).await?;
    let mut probe = MarkdownProbe {
        negotiated,
        alternate: None,
        alternate_fetch: None,
    };
    let Some(res) = res else {
        return Some(probe);
    };
    if is_markdown(res.content_type.as_deref()) {
        return Some(probe);
    }
    probe.alternate = advertised(&res);
    if let Some((href, _)) = &probe.alternate
        && let Ok(url) = res.final_url.join(href)
        && url.origin() == home.origin()
        && rules.allowed(url.as_str())
    {
        probe.alternate_fetch = fetch_one(fetcher, limiter, &url, deadline)
            .await
            .map(|(f, _)| f);
    }
    Some(probe)
}

/// One request with the Markdown `Accept` header: the answer as kept, and the response when one
/// arrived. `None` when the address is refused or the deadline came first.
async fn fetch_one(
    fetcher: &Fetcher,
    limiter: &Limiter,
    url: &Url,
    deadline: Instant,
) -> Option<(MarkdownFetch, Option<FetchResult>)> {
    let mut headers = HeaderMap::new();
    headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static(ACCEPT));
    // A site that asked us to back off (a 429 or 503 with a long `Retry-After`) is not waited on
    // for a nice-to-have: past a short wait the request is not made.
    let _permit = timeout_at(
        deadline.min(Instant::now() + MAX_SLOT_WAIT),
        limiter.acquire(),
    )
    .await
    .ok()?;
    let fetched = timeout_at(deadline, fetcher.fetch_with(url, &headers, MAX_BYTES))
        .await
        .ok()?;
    match fetched {
        Err(FetchError::Blocked(_)) => None,
        Err(e) => Some((
            MarkdownFetch {
                url: url.to_string(),
                status: 0,
                error: Some(e.to_string()),
                content_type: None,
                body_bytes: 0,
            },
            None,
        )),
        Ok(res) => {
            limiter.on_response(res.status, retry_after_of(&res.headers));
            let body_bytes = res.body.as_ref().map_or(0, |b| b.trim_ascii().len() as u64);
            Some((
                MarkdownFetch {
                    url: url.to_string(),
                    status: res.status,
                    error: None,
                    content_type: res.content_type.clone(),
                    body_bytes,
                },
                Some(res),
            ))
        }
    }
}

/// The Markdown alternate a response advertises: a `Link` header first, then a
/// `<link rel="alternate" type="text/markdown">` in its HTML.
fn advertised(res: &FetchResult) -> Option<(String, AlternateSource)> {
    let header = link_header_alternate(
        res.headers
            .get_all(LINK)
            .iter()
            .filter_map(|v| v.to_str().ok()),
    );
    let found = match header {
        Some(href) => Some((href, AlternateSource::LinkHeader)),
        None if (200..300).contains(&res.status) && is_html(res.content_type.as_deref()) => res
            .body
            .as_deref()
            .and_then(link_tag_alternate)
            .map(|href| (href, AlternateSource::LinkTag)),
        None => None,
    };
    found.map(|(href, source)| (href.chars().take(MAX_ALTERNATE_URL).collect(), source))
}

/// The `href` of the first `<link rel="alternate" type="text/markdown">` in `html`.
pub fn link_tag_alternate(html: &[u8]) -> Option<String> {
    let found: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&found);
    let settings = Settings::new().append_element_content_handler(element!(
        "link[href][rel][type]",
        move |el| {
            if slot.borrow().is_some() {
                return Ok(());
            }
            let alternate = el.get_attribute("rel").is_some_and(|rel| {
                rel.split_ascii_whitespace()
                    .any(|r| r.eq_ignore_ascii_case("alternate"))
            });
            if alternate
                && is_markdown(el.get_attribute("type").as_deref())
                && let Some(href) = el.get_attribute("href")
                && !href.trim().is_empty()
            {
                *slot.borrow_mut() = Some(href.trim().to_owned());
            }
            Ok(())
        }
    ));
    let mut rewriter = HtmlRewriter::new(settings, |_: &[u8]| {});
    if rewriter.write(html).is_err() || rewriter.end().is_err() {
        return None;
    }
    found.borrow_mut().take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_markdown_alternate_link() {
        let html = br#"<html><head>
            <link rel="alternate" hreflang="fr" href="/fr/">
            <link rel="stylesheet" type="text/css" href="/s.css">
            <LINK REL="Alternate" TYPE="text/markdown" HREF=" /index.md ">
            <link rel="alternate" type="text/markdown" href="/second.md">
            </head><body></body></html>"#;
        assert_eq!(link_tag_alternate(html), Some("/index.md".to_owned()));
        assert_eq!(
            link_tag_alternate(b"<link rel=alternate type=text/html href=/x>"),
            None
        );
        assert_eq!(link_tag_alternate(b""), None);
    }
}
