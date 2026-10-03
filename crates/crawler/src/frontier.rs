//! The URL queue: depth levels, de-duplication and a hard cap on the total.

use std::collections::{HashSet, VecDeque};

use codoseo_core::url::url_hash;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub url: Url,
    /// Clicks from the start page; `None` for pages found only in sitemaps.
    pub depth: Option<u16>,
}

/// Holds the URLs still to fetch. Never admits more than `max_pages` URLs in total
/// (fetched plus queued), so memory stays flat on endless URL spaces.
///
/// Levels are a barrier: `pop` only returns URLs of the current level, and the next level
/// becomes current when the caller asks with `advance`. Sitemap-only URLs come last.
#[derive(Debug)]
pub struct Frontier {
    seen: HashSet<u64>,
    current: VecDeque<Queued>,
    next: VecDeque<Queued>,
    sitemap_only: VecDeque<Queued>,
    admitted: u32,
    cap: u32,
    capped: bool,
}

impl Frontier {
    pub fn new(max_pages: u32) -> Frontier {
        Frontier {
            seen: HashSet::new(),
            current: VecDeque::new(),
            next: VecDeque::new(),
            sitemap_only: VecDeque::new(),
            admitted: 0,
            cap: max_pages,
            capped: false,
        }
    }

    /// Queues the start URL at depth 0. False when it was seen already or the cap is reached.
    pub fn seed(&mut self, url: Url) -> bool {
        if !self.try_admit(&url) {
            return false;
        }
        self.current.push_back(Queued {
            url,
            depth: Some(0),
        });
        true
    }

    /// Queues a link found on a page at `from_depth` for the next level.
    pub fn push_link(&mut self, url: Url, from_depth: u16) -> bool {
        if !self.try_admit(&url) {
            return false;
        }
        self.next.push_back(Queued {
            url,
            depth: Some(from_depth.saturating_add(1)),
        });
        true
    }

    /// Marks a URL as seen and counts it without queueing it (a redirect target whose
    /// record is built from the response that already arrived).
    pub fn admit(&mut self, url: &Url) -> bool {
        self.try_admit(url)
    }

    /// Queues sitemap URLs that nothing linked to; they are fetched after link exploration.
    pub fn add_sitemap_urls(&mut self, urls: impl IntoIterator<Item = Url>) {
        for url in urls {
            if !self.try_admit(&url) {
                if self.capped {
                    break;
                }
                continue;
            }
            self.sitemap_only.push_back(Queued { url, depth: None });
        }
    }

    /// The next URL of the current level.
    pub fn pop(&mut self) -> Option<Queued> {
        self.current.pop_front()
    }

    /// Starts the next level. When link exploration is over, starts the sitemap-only URLs
    /// (once). False when nothing is left.
    pub fn advance(&mut self) -> bool {
        if !self.next.is_empty() {
            self.current.append(&mut self.next);
        } else if !self.sitemap_only.is_empty() {
            self.current.append(&mut self.sitemap_only);
        }
        !self.current.is_empty()
    }

    /// Puts a URL back at the front of the current level (a 429/503 retry). It was counted
    /// when it was first queued and is not counted again.
    pub fn requeue_front(&mut self, q: Queued) {
        self.current.push_front(q);
    }

    pub fn is_seen(&self, url: &Url) -> bool {
        self.seen.contains(&url_hash(url))
    }

    /// True once a URL was refused because the cap was reached.
    pub fn capped(&self) -> bool {
        self.capped
    }

    /// URLs waiting to be fetched, in every queue.
    pub fn queued(&self) -> usize {
        self.current.len() + self.next.len() + self.sitemap_only.len()
    }

    /// Remembers and counts a new URL. A refused URL is not remembered.
    fn try_admit(&mut self, url: &Url) -> bool {
        let hash = url_hash(url);
        if self.seen.contains(&hash) {
            return false;
        }
        if self.admitted >= self.cap {
            self.capped = true;
            return false;
        }
        self.seen.insert(hash);
        self.admitted += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use codoseo_core::url::normalize;

    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn levels_dedup_sitemap_and_cap() {
        let mut f = Frontier::new(5);
        assert!(f.seed(u("https://e.com/")));
        assert_eq!(f.pop().unwrap().depth, Some(0));
        assert!(f.pop().is_none());
        assert!(f.push_link(u("https://e.com/a"), 0));
        assert!(!f.push_link(u("https://e.com/a"), 0)); // dedup
        assert!(f.pop().is_none()); // next level waits for advance()
        assert!(f.advance());
        assert_eq!(f.pop().unwrap().depth, Some(1));
        f.add_sitemap_urls([u("https://e.com/a"), u("https://e.com/s")]); // /a already seen
        assert!(f.advance());
        let s = f.pop().unwrap();
        assert_eq!((s.url.path(), s.depth), ("/s", None));
        assert!(!f.advance());
        for i in 0..10 {
            f.push_link(u(&format!("https://e.com/p{i}")), 1); // 3 admitted so far, cap 5
        }
        assert!(f.capped());
        assert_eq!(f.queued(), 2);
    }

    #[test]
    fn trivial_variants_are_one_page() {
        let base = u("https://e.com/");
        let mut f = Frontier::new(10);
        for href in ["/a", "/a?", "/a#x"] {
            f.push_link(normalize(&base, href).unwrap(), 0);
        }
        assert_eq!(f.queued(), 1);
        assert!(f.push_link(normalize(&base, "/A").unwrap(), 0));
    }

    #[test]
    fn admit_counts_and_marks_seen_without_queueing() {
        let mut f = Frontier::new(2);
        assert!(f.seed(u("https://e.com/")));
        assert!(f.admit(&u("https://e.com/new")));
        assert!(f.is_seen(&u("https://e.com/new")));
        assert!(!f.admit(&u("https://e.com/new")));
        assert_eq!(f.queued(), 1);
        // The cap is full: a link is refused, flagged, and not remembered.
        assert!(!f.capped());
        assert!(!f.push_link(u("https://e.com/x"), 0));
        assert!(f.capped());
        assert!(!f.is_seen(&u("https://e.com/x")));
    }

    #[test]
    fn seen_url_does_not_trip_the_cap() {
        let mut f = Frontier::new(1);
        assert!(f.seed(u("https://e.com/")));
        assert!(!f.push_link(u("https://e.com/"), 0));
        assert!(!f.capped());
    }

    #[test]
    fn requeue_front_goes_first_and_is_not_counted_again() {
        let mut f = Frontier::new(2);
        f.seed(u("https://e.com/"));
        f.push_link(u("https://e.com/a"), 0);
        f.pop();
        f.advance();
        let a = f.pop().unwrap();
        f.requeue_front(a.clone());
        assert_eq!(f.queued(), 1);
        assert_eq!(f.pop().unwrap(), a);
        // Still room for nothing new: 2 admitted, cap 2.
        assert!(!f.push_link(u("https://e.com/b"), 1));
        assert!(f.capped());
    }

    #[test]
    fn sitemap_urls_wait_for_link_levels_and_respect_the_cap() {
        let mut f = Frontier::new(3);
        f.seed(u("https://e.com/"));
        f.add_sitemap_urls((0..10).map(|i| u(&format!("https://e.com/s{i}"))));
        assert!(f.capped());
        assert_eq!(f.queued(), 3); // seed + 2 sitemap URLs
        assert_eq!(f.pop().unwrap().depth, Some(0));
        assert!(f.pop().is_none()); // sitemap URLs are not in the current level yet
        assert!(f.advance());
        assert_eq!(f.pop().unwrap().depth, None);
        assert_eq!(f.pop().unwrap().depth, None);
        assert!(!f.advance());
    }

    #[test]
    fn link_levels_come_before_sitemap_urls() {
        let mut f = Frontier::new(10);
        f.seed(u("https://e.com/"));
        f.add_sitemap_urls([u("https://e.com/s")]);
        f.pop();
        f.push_link(u("https://e.com/a"), 0);
        assert!(f.advance());
        assert_eq!(f.pop().unwrap().url.path(), "/a");
        assert!(f.pop().is_none());
        assert!(f.advance());
        assert_eq!(f.pop().unwrap().url.path(), "/s");
    }
}
