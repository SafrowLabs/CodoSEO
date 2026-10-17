//! The live crawl panel: the counters and newest pages of a running crawl, shown by the first
//! crawl on the site audit, the Crawls screen and the no-signup audit while they poll.
//!
//! Nothing here is kept in the web process. The worker's throttled heartbeat (every 2 s) stores
//! the counters and the newest pages on the crawl row (`LiveProgress`), and each poll reads that
//! one row, so the web and the worker can run as separate processes.

use askama::Template;
use codoseo_core::output::{LiveProgress, RecentPage};
use codoseo_core::page::FetchFailure;
use codoseo_store::crawls::{Crawl, CrawlStatus};
use time::OffsetDateTime;

use crate::fmt;

/// The longest a batch of new rows takes to fade in, about one poll.
const REVEAL_MS: u32 = 1600;
/// The most time between two new rows, so a short batch doesn't drag.
const MAX_STEP_MS: u32 = 220;

/// The counters and the newest pages. Rows newer than the last poll's (`seen`) fade in one
/// after another, oldest first, so a batch of pages reads as pages arriving.
#[derive(Template)]
#[template(path = "partials/live_crawl.html")]
pub struct LiveCrawl {
    pub stats: Vec<Stat>,
    pub pages: Vec<LivePage>,
    /// The newest page's place in the crawl, for the next poll's `seen`.
    pub newest: u32,
}

pub struct Stat {
    pub label: &'static str,
    pub value: String,
    /// A tone class for the value, `c-err` for errors.
    pub class: &'static str,
}

pub struct LivePage {
    /// `200`, or `ERR` / `robots` when there was no response.
    pub status: String,
    /// The badge tone, as in the URL explorer.
    pub tone: &'static str,
    pub url: String,
    /// The URL without its scheme and host: `/blog/post?page=2`.
    pub path: String,
    /// Set on a row new since the last poll: when it starts fading in.
    pub delay_ms: Option<u32>,
}

impl LiveCrawl {
    /// The panel for a running crawl; `None` for one that is queued or over. `seen` is the
    /// newest page the poller already shows.
    pub fn for_crawl(c: &Crawl, seen: Option<u32>, now: OffsetDateTime) -> Option<LiveCrawl> {
        if c.status != CrawlStatus::Running {
            return None;
        }
        let live = c.live_progress();
        // Counted from the claim, so the clock ticks between heartbeats; the crawler's own
        // count wins should the two disagree.
        let since_start = c
            .started_at
            .map_or(0, |s| (now - s).whole_milliseconds().max(0) as u64);
        let elapsed = live
            .as_ref()
            .map_or(0, |l| l.progress.elapsed_ms)
            .max(since_start);
        Some(LiveCrawl::new(live.as_ref(), seen, elapsed))
    }

    fn new(live: Option<&LiveProgress>, seen: Option<u32>, elapsed_ms: u64) -> LiveCrawl {
        let (done, queued, errors) = live.map_or((0, 0, 0), |l| {
            (l.progress.pages_done, l.progress.queued, l.errors)
        });
        let recent = live.map_or(&[][..], |l| &l.recent[..]);
        let newest = recent.first().map_or(0, |p| p.n);
        // A `seen` past the newest page is from an earlier crawl: nothing is new to it.
        let seen = seen.filter(|&s| s <= newest);
        let fresh = seen.map_or(0, |s| recent.iter().filter(|p| p.n > s).count()) as u32;
        let step = if fresh > 1 {
            (REVEAL_MS / fresh).min(MAX_STEP_MS)
        } else {
            0
        };
        let pages = recent
            .iter()
            .enumerate()
            .map(|(i, p)| {
                // Newest first, so the oldest new row (the last of them) goes first.
                let delay_ms =
                    (seen.is_some_and(|s| p.n > s)).then(|| (fresh - 1 - i as u32) * step);
                LivePage {
                    status: status_label(p),
                    tone: status_tone(p),
                    url: p.url.clone(),
                    path: path_of(&p.url),
                    delay_ms,
                }
            })
            .collect();
        LiveCrawl {
            stats: vec![
                stat("Pages crawled", fmt::thousands(done), ""),
                stat("Queued", fmt::thousands(queued), ""),
                stat(
                    "Errors",
                    fmt::thousands(errors),
                    if errors > 0 { "c-err" } else { "" },
                ),
                stat("Elapsed", fmt::millis(elapsed_ms), ""),
            ],
            pages,
            newest,
        }
    }
}

impl LiveCrawl {
    /// The counters on one line: `1,284 pages · 40 queued · 2 errors · 1m 02s`.
    pub fn summary(&self) -> String {
        let [pages, queued, errors, elapsed] = &self.stats[..] else {
            return String::new();
        };
        let s = |v: &str| if v == "1" { "" } else { "s" };
        format!(
            "{} page{} · {} queued · {} error{} · {}",
            pages.value,
            s(&pages.value),
            queued.value,
            errors.value,
            s(&errors.value),
            elapsed.value
        )
    }
}

fn stat(label: &'static str, value: String, class: &'static str) -> Stat {
    Stat {
        label,
        value,
        class,
    }
}

fn status_label(p: &RecentPage) -> String {
    match (p.status, p.error) {
        (0, _) if p.robots => "robots".to_owned(),
        (0, Some(FetchFailure::Timeout)) => "timeout".to_owned(),
        (0, _) => "ERR".to_owned(),
        (s, _) => s.to_string(),
    }
}

fn status_tone(p: &RecentPage) -> &'static str {
    match p.status {
        0 if p.robots => "t-muted",
        200..=299 => "t-ok",
        300..=399 => "t-warn",
        _ => "t-err",
    }
}

/// `https://example.com/a/b?q=1` → `/a/b?q=1`.
fn path_of(url: &str) -> String {
    url.split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_owned()))
        .unwrap_or_else(|| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_core::output::{LiveFiles, LivePhase, Progress};

    fn page(n: u32, status: u16) -> RecentPage {
        RecentPage {
            n,
            url: format!("https://example.com/p/{n}"),
            status,
            error: None,
            robots: false,
        }
    }

    fn live(recent: Vec<RecentPage>) -> LiveProgress {
        LiveProgress {
            progress: Progress {
                pages_done: 1284,
                queued: 40,
                failures: 0,
                depth: 3,
                elapsed_ms: 62_000,
            },
            errors: 2,
            recent,
            phase: LivePhase::Pages,
            files: LiveFiles::default(),
            seo_estimate: None,
        }
    }

    #[test]
    fn the_counters_and_the_newest_pages() {
        let l = live(vec![page(12, 404), page(11, 200), page(10, 301)]);
        let panel = LiveCrawl::new(Some(&l), None, 62_000);
        let stats: Vec<_> = panel.stats.iter().map(|s| s.value.as_str()).collect();
        assert_eq!(stats, ["1,284", "40", "2", "1m 02s"]);
        assert_eq!(panel.stats[2].class, "c-err");
        assert_eq!(
            panel.summary(),
            "1,284 pages · 40 queued · 2 errors · 1m 02s"
        );
        assert_eq!(panel.newest, 12);
        let rows: Vec<_> = panel
            .pages
            .iter()
            .map(|p| (p.status.as_str(), p.tone, p.path.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("404", "t-err", "/p/12"),
                ("200", "t-ok", "/p/11"),
                ("301", "t-warn", "/p/10")
            ]
        );
        // The first render animates nothing.
        assert!(panel.pages.iter().all(|p| p.delay_ms.is_none()));
    }

    #[test]
    fn rows_new_since_the_last_poll_fade_in_oldest_first() {
        let l = live(vec![
            page(12, 200),
            page(11, 200),
            page(10, 200),
            page(9, 200),
        ]);
        let panel = LiveCrawl::new(Some(&l), Some(10), 0);
        let delays: Vec<_> = panel.pages.iter().map(|p| p.delay_ms).collect();
        assert_eq!(delays, [Some(MAX_STEP_MS), Some(0), None, None]);

        // A `seen` from an earlier, longer crawl doesn't hide or animate anything.
        let panel = LiveCrawl::new(Some(&l), Some(500), 0);
        assert!(panel.pages.iter().all(|p| p.delay_ms.is_none()));
    }

    #[test]
    fn pages_without_a_response() {
        let mut timeout = page(3, 0);
        timeout.error = Some(FetchFailure::Timeout);
        let mut robots = page(2, 0);
        robots.robots = true;
        let mut refused = page(1, 0);
        refused.error = Some(FetchFailure::Connect);
        let labels: Vec<_> = [timeout, robots, refused]
            .iter()
            .map(|p| (status_label(p), status_tone(p)))
            .collect();
        assert_eq!(
            labels,
            [
                ("timeout".to_owned(), "t-err"),
                ("robots".to_owned(), "t-muted"),
                ("ERR".to_owned(), "t-err")
            ]
        );
        assert_eq!(path_of("https://e.com"), "https://e.com");
        assert_eq!(path_of("https://e.com/?a=1"), "/?a=1");
    }

    #[test]
    fn before_the_first_heartbeat() {
        let panel = LiveCrawl::new(None, Some(0), 3_000);
        assert!(panel.pages.is_empty());
        assert_eq!(panel.stats[0].value, "0");
        assert_eq!(panel.stats[3].value, "3s");
    }
}
