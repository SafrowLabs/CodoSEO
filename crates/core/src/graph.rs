//! Per-page facts read off a crawl's link graph: each page's place in the crawl tree and how
//! much of the site's internal link weight reaches it.

use std::collections::HashMap;

use crate::output::LinkGraph;
use crate::page::PageRecord;
use crate::url::url_hash;

/// PageRank's damping factor: the chance a visitor follows a link rather than starting over.
const DAMPING: f64 = 0.85;
/// PageRank rounds; the scores settle well before this on a site's link graph.
const ITERATIONS: usize = 30;

/// What [`facts`] reads off the graph, one entry per page in `CrawlOutput::pages` order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphFacts {
    /// The page the crawler found it from: the source of the first edge, in edge order, that
    /// comes from a page one click closer to the homepage. `None` for the start page, pages
    /// only found in sitemaps and pages no such edge reaches.
    pub parent: Vec<Option<u32>>,
    /// Internal PageRank over the followed links, scaled to 1..=100 on a log scale so the
    /// spread reads: 100 is the best-linked page, 1 the worst.
    pub link_score: Vec<u8>,
}

/// The crawl-tree parent and link score of every page. Edges pointing outside `pages` are
/// ignored.
pub fn facts(pages: &[PageRecord], links: &LinkGraph) -> GraphFacts {
    GraphFacts {
        parent: parents(pages, links),
        link_score: link_scores(pages.len(), links),
    }
}

/// Mirrors the BFS crawler's first discovery, so a parent always sits at `depth - 1`, as
/// stored in `pages.depth`. A redirect's target is recorded at the redirect's own depth and no
/// link points at it, so it takes the parent of the redirect that led to it.
fn parents(pages: &[PageRecord], links: &LinkGraph) -> Vec<Option<u32>> {
    let mut parent = vec![None; pages.len()];
    for e in &links.edges {
        let (Some(from), Some(to)) = (pages.get(e.from as usize), pages.get(e.to as usize)) else {
            continue;
        };
        let slot = &mut parent[e.to as usize];
        if slot.is_some() {
            continue;
        }
        if let (Some(d), Some(fd)) = (to.depth, from.depth)
            && d > 0
            && fd == d - 1
        {
            *slot = Some(e.from);
        }
    }
    let redirected_from: HashMap<u64, usize> = pages
        .iter()
        .enumerate()
        .filter_map(|(i, p)| Some((url_hash(p.redirect_target.as_ref()?), i)))
        .collect();
    // A few rounds, for a target whose redirect is itself the target of another one.
    for _ in 0..4 {
        let mut changed = false;
        for (i, p) in pages.iter().enumerate() {
            if parent[i].is_some() || !p.depth.is_some_and(|d| d > 0) {
                continue;
            }
            let Some(&r) = redirected_from.get(&url_hash(&p.url)) else {
                continue;
            };
            if r != i
                && pages[r].depth == p.depth
                && let Some(rp) = parent[r]
            {
                parent[i] = Some(rp);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    parent
}

fn link_scores(n: usize, links: &LinkGraph) -> Vec<u8> {
    if n == 0 {
        return Vec::new();
    }
    // Followed links only, each (from, to) pair once, no self-links.
    let mut edges: Vec<(u32, u32)> = links
        .edges
        .iter()
        .filter(|e| !e.nofollow && e.from != e.to)
        .filter(|e| (e.from as usize) < n && (e.to as usize) < n)
        .map(|e| (e.from, e.to))
        .collect();
    edges.sort_unstable();
    edges.dedup();

    let mut out_degree = vec![0u32; n];
    for &(from, _) in &edges {
        out_degree[from as usize] += 1;
    }
    let nf = n as f64;
    let mut rank = vec![1.0 / nf; n];
    let mut next = vec![0.0; n];
    for _ in 0..ITERATIONS {
        // Pages without followed links hand their weight to every page alike.
        let dangling: f64 = rank
            .iter()
            .zip(&out_degree)
            .filter(|&(_, &d)| d == 0)
            .map(|(r, _)| r)
            .sum();
        next.fill((1.0 - DAMPING) / nf + DAMPING * dangling / nf);
        for &(from, to) in &edges {
            next[to as usize] +=
                DAMPING * rank[from as usize] / f64::from(out_degree[from as usize]);
        }
        std::mem::swap(&mut rank, &mut next);
    }

    let logs: Vec<f64> = rank.iter().map(|r| r.ln()).collect();
    let min = logs.iter().copied().fold(f64::INFINITY, f64::min);
    let max = logs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let spread = max - min;
    // One page, or every page linked alike: all are the best-linked.
    if spread < 1e-9 {
        return vec![100; n];
    }
    logs.iter()
        .map(|l| (1.0 + 99.0 * (l - min) / spread).round().clamp(1.0, 100.0) as u8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::IssueBits;
    use crate::output::Edge;
    use crate::page::{Indexability, PageFields};
    use url::Url;

    fn page(i: usize, depth: Option<u16>) -> PageRecord {
        PageRecord {
            url: Url::parse(&format!("https://e.com/p{i}")).unwrap(),
            url_hash: i as u64,
            status: 200,
            redirect_chain: Vec::new(),
            response_ms: 0,
            size_bytes: 0,
            content_type: None,
            depth,
            in_sitemap: false,
            indexability: Indexability::Indexable,
            fields: PageFields::default(),
            inlinks: 0,
            outlinks_internal: 0,
            outlinks_external: 0,
            issues: IssueBits::default(),
            key_hash: 0,
            redirect_target: None,
            outlinks_nofollow: 0,
            error: None,
        }
    }

    fn graph(edges: &[(u32, u32, bool)]) -> LinkGraph {
        LinkGraph {
            edges: edges
                .iter()
                .map(|&(from, to, nofollow)| Edge {
                    from,
                    to,
                    anchor: 0,
                    nofollow,
                })
                .collect(),
            anchors: vec![String::new()],
        }
    }

    #[test]
    fn the_parent_is_the_first_edge_from_one_click_closer() {
        let pages = [
            page(0, Some(0)),
            page(1, Some(1)),
            page(2, Some(1)),
            page(3, Some(2)),
            page(4, None),
        ];
        // Page 3 is linked from itself, from a sibling at its own depth, then from page 2 and
        // page 1: page 2 came first among the edges one click closer.
        let links = graph(&[
            (0, 1, false),
            (0, 2, false),
            (3, 3, false),
            (1, 2, false),
            (2, 3, false),
            (1, 3, false),
            (0, 4, false),
            (9, 3, false),
        ]);
        let f = facts(&pages, &links);
        assert_eq!(f.parent, vec![None, Some(0), Some(0), Some(2), None]);
    }

    #[test]
    fn a_redirect_target_takes_the_parent_of_its_redirect() {
        // Page 1 links to page 2, which redirects to page 3; the crawler records page 3 at
        // page 2's depth with no link pointing at it.
        let mut pages = [
            page(0, Some(0)),
            page(1, Some(1)),
            page(2, Some(2)),
            page(3, Some(2)),
        ];
        pages[2].status = 301;
        pages[2].redirect_target = Some(pages[3].url.clone());
        let links = graph(&[(0, 1, false), (1, 2, false)]);
        let f = facts(&pages, &links);
        assert_eq!(f.parent, vec![None, Some(0), Some(1), Some(1)]);
    }

    #[test]
    fn scores_are_in_range_and_the_hub_scores_highest() {
        let pages: Vec<_> = (0..6).map(|i| page(i, Some(1))).collect();
        // Every page links to page 0; page 0 links to page 1 only.
        let links = graph(&[
            (0, 1, false),
            (1, 0, false),
            (2, 0, false),
            (3, 0, false),
            (4, 0, false),
            (5, 0, false),
            (5, 0, false),
        ]);
        let s = facts(&pages, &links).link_score;
        assert_eq!(s.len(), 6);
        assert!(s.iter().all(|&v| (1..=100).contains(&v)));
        assert_eq!(s[0], 100);
        assert!(s[1] > s[2] && s[1] < 100);
        assert_eq!(s[2..].iter().min(), Some(&1));
    }

    #[test]
    fn nofollow_links_pass_no_weight() {
        let pages: Vec<_> = (0..4).map(|i| page(i, Some(1))).collect();
        // Page 0 links to pages 1 and 3; page 2 links to page 1 too.
        let followed = facts(
            &pages,
            &graph(&[(0, 1, false), (0, 3, false), (2, 1, false)]),
        );
        assert!(followed.link_score[1] > followed.link_score[3]);
        // Made nofollow, page 2's link passes nothing and pages 1 and 3 score alike.
        let nofollowed = facts(
            &pages,
            &graph(&[(0, 1, false), (0, 3, false), (2, 1, true)]),
        );
        assert_eq!(nofollowed.link_score[1], nofollowed.link_score[3]);
        let without = facts(&pages, &graph(&[(0, 1, false), (0, 3, false)]));
        assert_eq!(nofollowed, without);
    }

    #[test]
    fn an_empty_graph_and_a_single_page() {
        assert_eq!(facts(&[], &LinkGraph::default()), GraphFacts::default());
        let one = facts(&[page(0, Some(0))], &LinkGraph::default());
        assert_eq!(one.link_score, vec![100]);
        assert_eq!(one.parent, vec![None]);
        // Pages with no links at all are linked alike.
        let three: Vec<_> = (0..3).map(|i| page(i, None)).collect();
        assert_eq!(
            facts(&three, &LinkGraph::default()).link_score,
            vec![100; 3]
        );
    }
}
