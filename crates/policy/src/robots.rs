//! "robots.txt that matches": the site's own robots.txt rewritten so the AI bots get what the
//! policy says, with a line diff against the file as the latest crawl read it. A suggestion to
//! paste or hand to a developer; CodoSEO never changes the site.
//!
//! The rewrite keeps the file as it is except for the AI bots the policy has a stance on: they
//! are taken out of groups that shut them out entirely (`Disallow: /`), the bots to block get one
//! group of their own, the bots to allow get an `Allow: /` group when the `*` group shuts
//! everyone out, and the `*` group gets the matching `Content-Signal` line.

use codoseo_geo::intent::{Intent, Stance};
use codoseo_geo::registry::{Purpose, registry};
use serde::{Deserialize, Serialize};

/// Above this many line pairs the diff compares lines as sets instead of aligning them.
const MAX_DIFF_CELLS: usize = 250_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sign {
    Same,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub sign: Sign,
    pub text: String,
}

/// The suggested file and how it differs from the current one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    /// The suggested file, newline-terminated.
    pub file: String,
    pub diff: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
    /// The `Content-Signal` line the `*` group gets.
    pub content_signal: String,
}

/// `Content-Signal: search=yes, ai-input=yes, ai-train=no`, from the stances of search crawlers,
/// user-triggered fetchers and training crawlers: `no` only for a purpose the policy blocks.
pub fn content_signal(intent: &Intent) -> String {
    let yn = |p: Purpose| {
        if intent.purpose_stance(p) == Stance::Block {
            "no"
        } else {
            "yes"
        }
    };
    format!(
        "Content-Signal: search={}, ai-input={}, ai-train={}",
        yn(Purpose::Search),
        yn(Purpose::UserFetch),
        yn(Purpose::Training)
    )
}

struct Group {
    /// `(line, token)` per `User-agent` line.
    agents: Vec<(String, String)>,
    rules: Vec<String>,
}

fn key_value(line: &str) -> Option<(String, &str)> {
    let body = line.split('#').next().unwrap_or_default();
    let (k, v) = body.split_once(':')?;
    Some((k.trim().to_ascii_lowercase(), v.trim()))
}

impl Group {
    fn is_star(&self) -> bool {
        self.agents.iter().any(|(_, t)| t == "*")
    }

    /// Shuts its agents out of everything: a `Disallow: /` and no `Allow`.
    fn blanket(&self) -> bool {
        let mut disallow_all = false;
        for r in &self.rules {
            match key_value(r) {
                Some((k, v)) if k == "disallow" && v == "/" => disallow_all = true,
                Some((k, _)) if k == "allow" => return false,
                _ => {}
            }
        }
        disallow_all
    }
}

/// The stance the policy has on a robots.txt token: `None` for one it says nothing about.
fn managed(intent: &Intent, token: &str) -> Option<Stance> {
    let stance = match registry().bot(token) {
        Some(bot) => intent.effective(bot),
        None => intent.bot_override(token)?,
    };
    (stance != Stance::Any).then_some(stance)
}

/// The tokens with `stance`, registry bots first in registry order, then the policy's other
/// overrides.
fn tokens_with(intent: &Intent, stance: Stance) -> Vec<String> {
    let mut out: Vec<String> = registry()
        .bots
        .iter()
        .filter(|b| intent.effective(b) == stance)
        .map(|b| b.token.clone())
        .collect();
    for (token, s) in &intent.bots {
        if *s == stance && registry().bot(token).is_none() {
            out.push(token.clone());
        }
    }
    out
}

/// The suggested robots.txt for `intent`, from the `current` file (`None`: the site has none).
pub fn suggest(current: Option<&str>, intent: &Intent) -> Suggestion {
    let current_text = current.unwrap_or_default();
    let mut preamble: Vec<String> = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut sitemaps: Vec<String> = Vec::new();
    let mut in_rules = true;
    for raw in current_text.lines() {
        let line = raw.trim_end_matches('\r').trim().to_owned();
        if line.is_empty() {
            continue;
        }
        match key_value(&line) {
            Some((k, v)) if k == "user-agent" => {
                if in_rules || groups.is_empty() {
                    groups.push(Group {
                        agents: Vec::new(),
                        rules: Vec::new(),
                    });
                }
                in_rules = false;
                let token = v.split_whitespace().next().unwrap_or_default().to_owned();
                groups
                    .last_mut()
                    .expect("a group was just pushed")
                    .agents
                    .push((line, token));
            }
            Some((k, _)) if k == "sitemap" => sitemaps.push(line),
            Some((k, _)) if k == "content-signal" && groups.last().is_none_or(Group::is_star) => {
                // The `*` group's signal is rewritten below.
                in_rules = true;
            }
            _ => {
                in_rules = true;
                match groups.last_mut() {
                    Some(g) => g.rules.push(line),
                    None => preamble.push(line),
                }
            }
        }
    }

    // Take the bots the policy decides out of the groups that shut them out entirely.
    for g in &mut groups {
        if g.blanket() && !g.is_star() {
            g.agents.retain(|(_, t)| managed(intent, t).is_none());
        }
    }
    groups.retain(|g| !g.agents.is_empty());

    let signal = content_signal(intent);
    let star_blanket = match groups.iter_mut().find(|g| g.is_star()) {
        Some(star) => {
            star.rules.insert(0, signal.clone());
            star.blanket()
        }
        None => {
            groups.insert(
                0,
                Group {
                    agents: vec![("User-agent: *".to_owned(), "*".to_owned())],
                    rules: vec![signal.clone(), "Allow: /".to_owned()],
                },
            );
            false
        }
    };
    if star_blanket {
        let allow = tokens_with(intent, Stance::Allow);
        if !allow.is_empty() {
            groups.push(Group {
                agents: allow
                    .iter()
                    .map(|t| (format!("User-agent: {t}"), t.clone()))
                    .collect(),
                rules: vec!["Allow: /".to_owned()],
            });
        }
    }
    let block = tokens_with(intent, Stance::Block);
    if !block.is_empty() {
        groups.push(Group {
            agents: block
                .iter()
                .map(|t| (format!("User-agent: {t}"), t.clone()))
                .collect(),
            rules: vec!["Disallow: /".to_owned()],
        });
    }

    let mut want: Vec<String> = Vec::new();
    if !preamble.is_empty() {
        want.extend(preamble);
        want.push(String::new());
    }
    for g in groups {
        want.extend(g.agents.into_iter().map(|(l, _)| l));
        want.extend(g.rules);
        want.push(String::new());
    }
    want.extend(sitemaps);
    while want.last().is_some_and(String::is_empty) {
        want.pop();
    }

    let have: Vec<String> = current_text
        .lines()
        .map(|l| l.trim_end_matches('\r').trim().to_owned())
        .collect();
    let diff = diff_lines(&have, &want);
    let added = diff.iter().filter(|d| d.sign == Sign::Added).count();
    let removed = diff.iter().filter(|d| d.sign == Sign::Removed).count();
    let mut file = want.join("\n");
    file.push('\n');
    Suggestion {
        file,
        diff,
        added,
        removed,
        content_signal: signal,
    }
}

/// A line diff of `want` against `have`, blank lines ignored for the comparison.
pub fn diff_lines(have: &[String], want: &[String]) -> Vec<DiffLine> {
    let a: Vec<&String> = have.iter().filter(|l| !l.is_empty()).collect();
    let b: &[String] = want;
    let line = |sign, text: &str| DiffLine {
        sign,
        text: text.to_owned(),
    };
    let bn: Vec<&String> = b.iter().filter(|l| !l.is_empty()).collect();
    if a.len().saturating_mul(bn.len()) > MAX_DIFF_CELLS {
        let mut out: Vec<DiffLine> = b
            .iter()
            .map(|l| {
                let sign = if l.is_empty() || a.contains(&l) {
                    Sign::Same
                } else {
                    Sign::Added
                };
                line(sign, l)
            })
            .collect();
        out.extend(
            a.iter()
                .filter(|l| !b.contains(l))
                .map(|l| line(Sign::Removed, l)),
        );
        return out;
    }
    // Longest common subsequence over the non-blank lines.
    let (n, m) = (a.len(), bn.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == bn[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    for l in b {
        if l.is_empty() {
            out.push(line(Sign::Same, ""));
            continue;
        }
        // Lines of `have` that come before this one of `want` are removed.
        while i < n && a[i] != bn[j] && lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(line(Sign::Removed, a[i]));
            i += 1;
        }
        if i < n && a[i] == bn[j] {
            out.push(line(Sign::Same, l));
            i += 1;
        } else {
            out.push(line(Sign::Added, l));
        }
        j += 1;
    }
    while i < n {
        out.push(line(Sign::Removed, a[i]));
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn training_blocked() -> Intent {
        let mut i = Intent::default();
        i.purposes.insert(Purpose::Training, Stance::Block);
        i
    }

    #[test]
    fn the_signal_follows_the_stances() {
        assert_eq!(
            content_signal(&Intent::default()),
            "Content-Signal: search=yes, ai-input=yes, ai-train=yes"
        );
        assert_eq!(
            content_signal(&training_blocked()),
            "Content-Signal: search=yes, ai-input=yes, ai-train=no"
        );
    }

    #[test]
    fn blocked_bots_get_a_group_and_the_rest_of_the_file_stays() {
        let current = "User-agent: *\nDisallow: /checkout/\nAllow: /\n\nUser-agent: PerplexityBot\nDisallow: /\n\nSitemap: https://shop.example.com/sitemap.xml\n";
        let s = suggest(Some(current), &training_blocked());
        assert!(s.file.starts_with(
            "User-agent: *\nContent-Signal: search=yes, ai-input=yes, ai-train=no\nDisallow: /checkout/\nAllow: /\n"
        ));
        // PerplexityBot is a search crawler the policy allows: its blanket block goes.
        assert!(!s.file.contains("User-agent: PerplexityBot"), "{}", s.file);
        assert!(s.file.contains("User-agent: GPTBot\n"));
        assert!(s.file.contains("User-agent: CCBot\n"));
        assert!(
            s.file
                .trim_end()
                .ends_with("Sitemap: https://shop.example.com/sitemap.xml")
        );
        assert!(
            s.diff
                .iter()
                .any(|d| d.sign == Sign::Removed && d.text == "User-agent: PerplexityBot")
        );
        assert!(
            s.diff
                .iter()
                .any(|d| d.sign == Sign::Added && d.text == "User-agent: GPTBot")
        );
        assert_eq!(
            s.added,
            s.diff.iter().filter(|d| d.sign == Sign::Added).count()
        );
    }

    #[test]
    fn a_site_without_robots_gets_a_star_group() {
        let s = suggest(None, &Intent::default());
        assert_eq!(
            s.file,
            "User-agent: *\nContent-Signal: search=yes, ai-input=yes, ai-train=yes\nAllow: /\n"
        );
        assert_eq!(s.removed, 0);
        assert_eq!(s.added, 3);
    }

    #[test]
    fn an_unchanged_file_has_no_diff() {
        let s = suggest(None, &training_blocked());
        let again = suggest(Some(&s.file), &training_blocked());
        assert_eq!(again.file, s.file);
        assert_eq!((again.added, again.removed), (0, 0));
    }
}
