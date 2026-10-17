//! What the live views show of a running crawl: every page with what its own checks found and
//! which AI products can use it, the stage the crawl is in, and a running SEO score.
//!
//! The crawler calls [`LiveTracker::observe`] after every recorded page and as each stage
//! starts. The page checks are pure and the robots.txt verdicts are read from the file the crawl
//! already has, so this costs no request. The worker writes what is new on its throttled
//! heartbeat ([`LiveTracker::take`]).

use std::sync::Mutex;

use codoseo_checks::{Thresholds, health_score, page_issues_with};
use codoseo_core::check::CheckId;
use codoseo_core::output::{
    LiveFiles, LivePhase, LiveProgress, LiveRow, Progress, RECENT_PAGES, RecentPage,
};
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_crawler::crawl::Live;
use codoseo_geo::eligibility::{Effect, engines, record_effect};
use codoseo_geo::report::agent_rules;
use codoseo_geo::robots::{AgentRules, RobotsTxt};

/// What one heartbeat writes: the progress for the crawl row and the pages not written yet.
pub struct LiveWrite {
    pub progress: LiveProgress,
    pub rows: Vec<LiveRow>,
}

pub struct LiveTracker {
    thresholds: Thresholds,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    rows: Vec<LiveRow>,
    /// `rows[..written]` went out with an earlier write.
    written: usize,
    /// The robots.txt the AI verdicts are read from, parsed once from the first page's view.
    /// `Some(None)` when it gave no verdicts.
    robots: Option<Option<AiRobots>>,
    progress: Option<Progress>,
    phase: LivePhase,
    files: LiveFiles,
    /// The stage the last write carried, so a new stage is written straight away.
    written_phase: Option<LivePhase>,
}

/// robots.txt with each answer engine's rules chosen once.
struct AiRobots {
    txt: RobotsTxt,
    agents: Vec<AgentRules>,
}

impl LiveTracker {
    pub fn new(thresholds: Thresholds) -> LiveTracker {
        LiveTracker {
            thresholds,
            state: Mutex::new(State::default()),
        }
    }

    /// Takes in what the crawler reports: checks the pages recorded since the last call.
    pub fn observe(&self, live: &Live<'_>) {
        let mut st = self.state.lock().expect("live state lock");
        st.progress = Some(live.progress);
        st.phase = live.phase;
        st.files = live.files;
        if live.pages.len() <= st.rows.len() {
            return;
        }
        if st.robots.is_none() {
            st.robots = Some(live.robots.and_then(|f| {
                RobotsTxt::from_response(Some(f.status), f.body.as_bytes()).map(AiRobots::new)
            }));
        }
        let from = st.rows.len();
        for (i, page) in live.pages.iter().enumerate().skip(from) {
            let row = self.row(i, page, st.robots.as_ref().and_then(Option::as_ref));
            st.rows.push(row);
        }
    }

    /// The pages are in: what is left is the scoring.
    pub fn scoring(&self) {
        self.state.lock().expect("live state lock").phase = LivePhase::Scoring;
    }

    /// What a heartbeat should write, or `None` when nothing is due: `due` is the throttle's
    /// say, and a new stage is due whatever it says.
    pub fn take(&self, due: bool) -> Option<LiveWrite> {
        let mut st = self.state.lock().expect("live state lock");
        let progress = st.progress?;
        if !due && st.written_phase == Some(st.phase) {
            return None;
        }
        let rows = st.rows[st.written..].to_vec();
        st.written = st.rows.len();
        st.written_phase = Some(st.phase);
        let live = LiveProgress {
            progress,
            errors: st.rows.iter().filter(|r| is_error(r)).count() as u32,
            recent: st
                .rows
                .iter()
                .rev()
                .take(RECENT_PAGES)
                .map(recent)
                .collect(),
            phase: st.phase,
            files: st.files,
            seo_estimate: seo_estimate(&st.rows),
        };
        Some(LiveWrite {
            progress: live,
            rows,
        })
    }

    fn row(&self, i: usize, page: &PageRecord, robots: Option<&AiRobots>) -> LiveRow {
        let (ai_blocked, ai_of) = robots.map_or((0, 0), |r| r.blocked(page));
        LiveRow {
            n: u32::try_from(i + 1).unwrap_or(u32::MAX),
            url: page.url.to_string(),
            status: page.status,
            error: page.error,
            response_ms: page.response_ms,
            indexability: page.indexability,
            issues: page_issues_with(page, &self.thresholds),
            ai_blocked,
            ai_of,
        }
    }
}

impl AiRobots {
    fn new(txt: RobotsTxt) -> AiRobots {
        let agents = engines()
            .iter()
            .map(|e| agent_rules(&txt, e.crawler))
            .collect();
        AiRobots { txt, agents }
    }

    /// The answer engines that can't use `page`, as bits in registry order, and how many were
    /// judged. robots.txt decides for every page; a page that answered decides too, through its
    /// own controls (`noai`-style metas and headers that exclude it).
    fn blocked(&self, page: &PageRecord) -> (u16, u8) {
        let path = path_and_query(page);
        let mut bits = 0u16;
        for (i, (engine, agent)) in engines().iter().zip(&self.agents).enumerate() {
            let robots_out = !self.txt.allowed_for(agent, &path);
            let page_out = matches!(record_effect(engine, page), Some((Effect::Excluded, _)));
            if robots_out || page_out {
                bits |= 1 << i;
            }
        }
        (bits, engines().len() as u8)
    }
}

fn path_and_query(page: &PageRecord) -> String {
    match page.url.query() {
        Some(q) => format!("{}?{q}", page.url.path()),
        None => page.url.path().to_owned(),
    }
}

/// As `LiveProgress::errors` counts them: 4xx, 5xx, or no response that robots.txt didn't
/// cause.
fn is_error(r: &LiveRow) -> bool {
    r.status >= 400 || (r.status == 0 && r.indexability != Indexability::BlockedByRobots)
}

/// The row as the newest-pages panel shows it.
fn recent(r: &LiveRow) -> RecentPage {
    RecentPage {
        n: r.n,
        url: r.url.clone(),
        status: r.status,
        error: r.error,
        robots: r.indexability == Indexability::BlockedByRobots,
    }
}

/// The SEO score of the rows so far, from their page checks alone.
fn seo_estimate(rows: &[LiveRow]) -> Option<u8> {
    if rows.is_empty() {
        return None;
    }
    let mut affected = [0u32; 64];
    for r in rows {
        for bit in r.issues.iter() {
            affected[usize::from(bit)] += 1;
        }
    }
    let counts: Vec<(CheckId, u32)> = CheckId::ALL
        .into_iter()
        .map(|id| (id, affected[usize::from(id.bit())]))
        .filter(|&(_, n)| n > 0)
        .collect();
    Some(health_score(&counts, rows.len() as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_core::crawl::RobotsFile;
    use codoseo_core::page::PageFields;
    use url::Url;

    fn page(path: &str, status: u16) -> PageRecord {
        PageRecord {
            url: Url::parse(&format!("https://example.com{path}")).unwrap(),
            url_hash: 0,
            status,
            redirect_chain: Vec::new(),
            response_ms: 120,
            size_bytes: 2048,
            content_type: Some("text/html".to_owned()),
            depth: Some(1),
            in_sitemap: true,
            indexability: Indexability::Indexable,
            fields: PageFields::default(),
            inlinks: 0,
            outlinks_internal: 0,
            outlinks_external: 0,
            issues: Default::default(),
            key_hash: 0,
            redirect_target: None,
            outlinks_nofollow: 0,
            error: None,
        }
    }

    fn robots(body: &str) -> RobotsFile {
        RobotsFile {
            status: 200,
            body: body.to_owned(),
            hash: 0,
        }
    }

    fn view<'a>(
        pages: &'a [PageRecord],
        robots: Option<&'a RobotsFile>,
        phase: LivePhase,
    ) -> Live<'a> {
        Live {
            progress: Progress {
                pages_done: pages.len() as u32,
                queued: 0,
                failures: 0,
                depth: 1,
                elapsed_ms: 1000,
            },
            phase,
            files: LiveFiles::default(),
            pages,
            robots,
        }
    }

    fn engine_bit(crawler: &str) -> u16 {
        1 << engines().iter().position(|e| e.crawler == crawler).unwrap()
    }

    #[test]
    fn every_page_is_checked_once_in_crawl_order() {
        let t = LiveTracker::new(Thresholds::DEFAULT);
        let file = robots("User-agent: *\nAllow: /");
        let mut pages = vec![page("/", 200)];
        t.observe(&view(&pages, Some(&file), LivePhase::Pages));
        pages.push(page("/gone", 404));
        t.observe(&view(&pages, Some(&file), LivePhase::Pages));
        let w = t.take(true).unwrap();
        let ns: Vec<u32> = w.rows.iter().map(|r| r.n).collect();
        assert_eq!(ns, [1, 2]);
        assert_eq!(w.rows[1].status, 404);
        assert_eq!(w.progress.errors, 1);
        assert_eq!(w.progress.phase, LivePhase::Pages);
        // The newest-pages panel keeps working from the same write, newest first.
        assert_eq!(
            w.progress.recent.iter().map(|r| r.n).collect::<Vec<_>>(),
            [2, 1]
        );
        // A bare page fails page checks (no title, no H1...), so the estimate is below 100.
        assert!(w.rows[0].issues.iter().next().is_some());
        assert!(w.progress.seo_estimate.unwrap() < 100);
        // Written rows don't go out again.
        pages.push(page("/a", 200));
        t.observe(&view(&pages, Some(&file), LivePhase::Pages));
        let w = t.take(true).unwrap();
        assert_eq!(w.rows.iter().map(|r| r.n).collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn robots_txt_and_page_controls_decide_which_ai_products_can_use_a_page() {
        let t = LiveTracker::new(Thresholds::DEFAULT);
        let file = robots("User-agent: PerplexityBot\nDisallow: /private\n");
        let pages = vec![page("/", 200), page("/private/x", 200)];
        t.observe(&view(&pages, Some(&file), LivePhase::Pages));
        let w = t.take(true).unwrap();
        let of = engines().len() as u8;
        assert_eq!((w.rows[0].ai_blocked, w.rows[0].ai_of), (0, of));
        assert_eq!(w.rows[1].ai_blocked, engine_bit("PerplexityBot"));
    }

    #[test]
    fn without_a_readable_robots_txt_no_ai_verdict_is_given() {
        let t = LiveTracker::new(Thresholds::DEFAULT);
        let failed = RobotsFile {
            status: 503,
            body: String::new(),
            hash: 0,
        };
        let pages = vec![page("/", 200)];
        t.observe(&view(&pages, Some(&failed), LivePhase::Pages));
        let w = t.take(true).unwrap();
        assert_eq!((w.rows[0].ai_blocked, w.rows[0].ai_of), (0, 0));
    }

    #[test]
    fn a_new_stage_is_written_even_when_the_throttle_says_wait() {
        let t = LiveTracker::new(Thresholds::DEFAULT);
        assert!(
            t.take(true).is_none(),
            "nothing to write before the crawl reports"
        );
        t.observe(&view(&[], None, LivePhase::Robots));
        assert_eq!(t.take(false).unwrap().progress.phase, LivePhase::Robots);
        assert!(t.take(false).is_none());
        t.observe(&view(&[], None, LivePhase::Sitemap));
        assert_eq!(t.take(false).unwrap().progress.phase, LivePhase::Sitemap);
        t.scoring();
        let w = t.take(false).unwrap();
        assert_eq!(w.progress.phase, LivePhase::Scoring);
        assert_eq!(w.progress.seo_estimate, None);
    }
}
