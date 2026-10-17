//! The site's icon, so the app can show the site as its owner knows it: what the home page's
//! `<link rel="icon">` (or Apple touch icon) points to, else `/favicon.ico`. At most two requests
//! per crawl, through the crawl's limiter, kept to robots.txt and ended by the deadline.
//!
//! Only raster images are kept, typed by their own first bytes rather than by the server: an SVG
//! can carry script, and the app serves the icon from its own origin.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use codoseo_core::output::SiteIcon;
use lol_html::{HtmlRewriter, Settings, element};
use tokio::time::{Instant, timeout_at};
use url::Url;

use crate::fetch::{FetchResult, Fetcher};
use crate::politeness::Limiter;
use crate::robots::RobotsRules;

/// The largest icon kept.
pub const MAX_ICON_BYTES: usize = 64 * 1024;
/// Icon URLs tried at most, `/favicon.ico` included.
const MAX_TRIES: usize = 2;
/// The longest an icon request waits for its turn at the site: it is a nicety.
const MAX_SLOT_WAIT: Duration = Duration::from_secs(3);

/// The icon of the site whose home page answered `home`. `None` when there is none to keep:
/// nothing usable was found, or robots.txt or the deadline stopped the requests.
pub(crate) async fn fetch_icon(
    fetcher: &Fetcher,
    limiter: &Limiter,
    home: &FetchResult,
    rules: &RobotsRules,
    deadline: Instant,
) -> Option<SiteIcon> {
    let mut urls: Vec<Url> = home
        .body
        .as_deref()
        .map(declared)
        .unwrap_or_default()
        .iter()
        .filter_map(|href| home.final_url.join(href).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .collect();
    if let Ok(ico) = home.final_url.join("/favicon.ico") {
        urls.push(ico);
    }
    urls.dedup();
    for url in urls.into_iter().take(MAX_TRIES) {
        if !rules.allowed(url.as_str()) {
            continue;
        }
        let Ok(_permit) = timeout_at(
            deadline.min(Instant::now() + MAX_SLOT_WAIT),
            limiter.acquire(),
        )
        .await
        else {
            return None;
        };
        let Ok(Ok(res)) = timeout_at(deadline, fetcher.fetch_raw(&url, MAX_ICON_BYTES + 1)).await
        else {
            continue;
        };
        if let Some(icon) = icon_of(&res) {
            return Some(icon);
        }
    }
    None
}

/// The icon a response holds: a 200 within the size cap whose bytes are a raster image.
fn icon_of(res: &FetchResult) -> Option<SiteIcon> {
    let body = res.body.as_deref()?;
    if res.status != 200 || res.truncated || body.is_empty() || body.len() > MAX_ICON_BYTES {
        return None;
    }
    Some(SiteIcon {
        content_type: sniff(body)?,
        bytes: body.to_vec(),
    })
}

/// The image type of `bytes` by their signature; `None` for anything else, SVG included.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0, 0, 1, 0]) {
        Some("image/x-icon")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The icon `href`s a page declares, best first: `rel="icon"` (and `shortcut icon`), then
/// `apple-touch-icon`. SVG icons are left out.
fn declared(html: &[u8]) -> Vec<String> {
    let found: Rc<RefCell<Vec<(u8, String)>>> = Rc::default();
    let sink = Rc::clone(&found);
    let settings =
        Settings::new().append_element_content_handler(element!("link[rel][href]", move |el| {
            let rel = el
                .get_attribute("rel")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let href = el.get_attribute("href").unwrap_or_default();
            let svg = el
                .get_attribute("type")
                .is_some_and(|t| t.to_ascii_lowercase().contains("svg"))
                || href
                    .to_ascii_lowercase()
                    .split(['?', '#'])
                    .next()
                    .is_some_and(|p| p.ends_with(".svg"));
            let rank = if rel.split_ascii_whitespace().any(|t| t == "icon") {
                Some(0)
            } else if rel
                .split_ascii_whitespace()
                .any(|t| t.starts_with("apple-touch-icon"))
            {
                Some(1)
            } else {
                None
            };
            if let Some(rank) = rank
                && !svg
                && !href.trim().is_empty()
            {
                sink.borrow_mut().push((rank, href.trim().to_owned()));
            }
            Ok(())
        }));
    let mut rewriter = HtmlRewriter::new(settings, |_: &[u8]| {});
    // A page that doesn't parse declares nothing.
    if rewriter.write(html).is_err() || rewriter.end().is_err() {
        return Vec::new();
    }
    let mut found = found.take();
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().map(|(_, href)| href).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_icons_come_best_first_without_svg() {
        let html = br#"<html><head>
            <link rel="apple-touch-icon" href="/touch.png">
            <link rel="icon" type="image/svg+xml" href="/icon.svg">
            <link rel="shortcut icon" href="/fav.png?v=2">
            <link rel="stylesheet" href="/site.css">
        </head></html>"#;
        assert_eq!(declared(html), ["/fav.png?v=2", "/touch.png"]);
        assert!(declared(b"<p>no head</p>").is_empty());
    }

    #[test]
    fn only_raster_images_are_recognised() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff(&[0, 0, 1, 0, 1]), Some("image/x-icon"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(
            sniff(b"<svg xmlns='http://www.w3.org/2000/svg'><script/></svg>"),
            None
        );
        assert_eq!(sniff(b"<html>not found</html>"), None);
    }
}
