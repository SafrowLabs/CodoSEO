//! The 0-100 GEO score of a crawl, and the Overall score that blends it with the SEO health
//! score ([`codoseo_checks::health_score`]).
//!
//! The GEO score answers "how ready is this site for AI products?" in two tiers:
//!
//! - **Essential, 70 points**: can AI products reach and read the site. robots.txt answers, the
//!   AI search and answer bots the owner wants are allowed, the bots the owner wants kept out are,
//!   pages are not taken out of AI answers, a fetch as each bot gets the page, and the important
//!   pages have their text in the HTML before JavaScript runs. These are the AI access findings
//!   ([`crate::findings`]), one check per [`FindingKind`].
//! - **Recommended, 30 points**: is the site easy for AI to understand. Structured data parses,
//!   pages have titles and descriptions, a sitemap is there, and so is an llms.txt. The first
//!   four reuse the SEO checks' counts; llms.txt is read from the access report.
//!
//! A check that does not apply to the crawl (the report could not judge it, there were no
//! pages, the probes did not run, or CodoSEO does not check it yet) is *skipped*: left out of
//! both what the site scored and what it could have scored, never failed. A block the owner
//! chose in their [`Intent`] opens no finding, so it costs nothing.
//!
//! Inside a tier the tier's points are shared by its applicable checks in proportion to a
//! nominal weight per check, the severity cap the SEO score uses (Critical 15, Warning 5,
//! Notice 1) at the worst severity the check can report. A failing check loses
//! `cap x (0.25 + 0.75 x share)` of its weight, like an SEO check, where `cap` is the cap of the
//! severity it failed with (a Notice-level block of a bot that may ignore robots.txt costs far
//! less than a Critical one), never more than the whole weight, and `share` is the part of the
//! important pages (or of all pages, for the SEO checks) it affects, 1 for a site-wide failure.
//! When a whole tier is skipped, the other tier is scaled up to 100.
//!
//! Overall is `round(0.6 x SEO + 0.4 x GEO)`, and its check counts are the SEO checks plus the
//! applicable GEO checks.

use codoseo_checks::{Scope, def};
use codoseo_core::check::{CheckId, Severity};
use serde::{Deserialize, Serialize};

use crate::findings::{Finding, FindingKind, evaluated_kinds, findings};
use crate::intent::Intent;
use crate::llms::LlmsState;
use crate::report::AccessReport;

/// What the SEO health score weighs in Overall.
pub const SEO_WEIGHT: f64 = 0.6;
/// What the GEO score weighs in Overall.
pub const GEO_WEIGHT: f64 = 0.4;

/// The two tiers of GEO checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Can AI products reach and read the site.
    Essential,
    /// Is the site easy for AI to understand.
    Recommended,
}

impl Tier {
    /// The points the tier is worth when both tiers have applicable checks.
    pub fn points(self) -> f64 {
        match self {
            Tier::Essential => 70.0,
            Tier::Recommended => 30.0,
        }
    }
}

/// One GEO check. The slug names what fails, like the finding kinds and SEO check ids it reads
/// (`sitemap`, `llms_txt`, `content_signals` and `markdown_negotiation` have no single one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeoCheck {
    /// robots.txt can't be read ([`FindingKind::RobotsUnavailable`]).
    RobotsUnavailable,
    /// AI search and answer bots the owner wants are blocked ([`FindingKind::BotsBlocked`]).
    BotsBlocked,
    /// Bots the owner wants kept out still get in ([`FindingKind::BotsNotBlocked`]).
    BotsNotBlocked,
    /// Page markup takes pages out of AI answers ([`FindingKind::AnswersRestricted`]).
    AnswersRestricted,
    /// A fetch as a bot is blocked, challenged or diverted ([`FindingKind::FetchBlocked`]).
    FetchBlocked,
    /// Important pages are an empty shell without JavaScript ([`FindingKind::ContentMissing`]).
    ContentMissing,
    /// JSON-LD does not parse ([`CheckId::JsonldInvalid`]).
    JsonldInvalid,
    /// Pages without a title ([`CheckId::TitleMissing`]).
    TitleMissing,
    /// Pages without a meta description ([`CheckId::DescriptionMissing`]).
    DescriptionMissing,
    /// No sitemap, or sitemap URLs that don't answer 200 ([`CheckId::SitemapMissing`],
    /// [`CheckId::SitemapNon200`]).
    Sitemap,
    /// The site serves an llms.txt (the report's `llms_txt`; an adopted file that stopped
    /// answering is [`FindingKind::LlmsTxtUnavailable`]).
    LlmsTxt,
    /// A `Content-Signal` line in robots.txt. Not checked yet, so always skipped.
    ContentSignals,
    /// Pages answer `Accept: text/markdown` with Markdown. Not checked yet, so always skipped.
    MarkdownNegotiation,
}

impl GeoCheck {
    /// Every check, in the order the breakdown lists them.
    pub const ALL: [GeoCheck; 13] = [
        GeoCheck::RobotsUnavailable,
        GeoCheck::BotsBlocked,
        GeoCheck::BotsNotBlocked,
        GeoCheck::AnswersRestricted,
        GeoCheck::FetchBlocked,
        GeoCheck::ContentMissing,
        GeoCheck::JsonldInvalid,
        GeoCheck::TitleMissing,
        GeoCheck::DescriptionMissing,
        GeoCheck::Sitemap,
        GeoCheck::LlmsTxt,
        GeoCheck::ContentSignals,
        GeoCheck::MarkdownNegotiation,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            GeoCheck::RobotsUnavailable => "robots_unavailable",
            GeoCheck::BotsBlocked => "bots_blocked",
            GeoCheck::BotsNotBlocked => "bots_not_blocked",
            GeoCheck::AnswersRestricted => "answers_restricted",
            GeoCheck::FetchBlocked => "fetch_blocked",
            GeoCheck::ContentMissing => "content_missing",
            GeoCheck::JsonldInvalid => "jsonld_invalid",
            GeoCheck::TitleMissing => "title_missing",
            GeoCheck::DescriptionMissing => "description_missing",
            GeoCheck::Sitemap => "sitemap",
            GeoCheck::LlmsTxt => "llms_txt",
            GeoCheck::ContentSignals => "content_signals",
            GeoCheck::MarkdownNegotiation => "markdown_negotiation",
        }
    }

    pub fn tier(self) -> Tier {
        match self {
            GeoCheck::RobotsUnavailable
            | GeoCheck::BotsBlocked
            | GeoCheck::BotsNotBlocked
            | GeoCheck::AnswersRestricted
            | GeoCheck::FetchBlocked
            | GeoCheck::ContentMissing => Tier::Essential,
            GeoCheck::JsonldInvalid
            | GeoCheck::TitleMissing
            | GeoCheck::DescriptionMissing
            | GeoCheck::Sitemap
            | GeoCheck::LlmsTxt
            | GeoCheck::ContentSignals
            | GeoCheck::MarkdownNegotiation => Tier::Recommended,
        }
    }

    /// The severity the check's weight is read from: the worst it can fail with (the finding
    /// kinds' own range, the SEO checks' registry severity). llms.txt, Content-Signal and
    /// Markdown are nice to have, so Notice whatever an llms.txt finding says.
    pub fn nominal(self) -> Severity {
        match self {
            GeoCheck::RobotsUnavailable | GeoCheck::BotsBlocked | GeoCheck::AnswersRestricted => {
                Severity::Critical
            }
            GeoCheck::BotsNotBlocked | GeoCheck::FetchBlocked | GeoCheck::ContentMissing => {
                Severity::Warning
            }
            GeoCheck::JsonldInvalid => def(CheckId::JsonldInvalid).severity,
            GeoCheck::TitleMissing => def(CheckId::TitleMissing).severity,
            GeoCheck::DescriptionMissing => def(CheckId::DescriptionMissing).severity,
            GeoCheck::Sitemap => def(CheckId::SitemapNon200)
                .severity
                .min(def(CheckId::SitemapMissing).severity),
            GeoCheck::LlmsTxt | GeoCheck::ContentSignals | GeoCheck::MarkdownNegotiation => {
                Severity::Notice
            }
        }
    }

    /// The check's share of its tier, before the tier is split.
    pub fn weight(self) -> f64 {
        cap(self.nominal())
    }

    /// The finding kind the check reads, for the Essential tier.
    fn finding_kind(self) -> Option<FindingKind> {
        Some(match self {
            GeoCheck::RobotsUnavailable => FindingKind::RobotsUnavailable,
            GeoCheck::BotsBlocked => FindingKind::BotsBlocked,
            GeoCheck::BotsNotBlocked => FindingKind::BotsNotBlocked,
            GeoCheck::AnswersRestricted => FindingKind::AnswersRestricted,
            GeoCheck::FetchBlocked => FindingKind::FetchBlocked,
            GeoCheck::ContentMissing => FindingKind::ContentMissing,
            _ => return None,
        })
    }
}

/// The severity caps of the SEO score ([`codoseo_checks::health_score`]), used as weights.
fn cap(severity: Severity) -> f64 {
    match severity {
        Severity::Critical => 15.0,
        Severity::Warning => 5.0,
        Severity::Notice => 1.0,
    }
}

/// What one check came to on a crawl.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    Pass,
    /// Failed with this severity on this share (0 to 1) of the pages it looks at.
    Fail {
        severity: Severity,
        share: f64,
    },
    /// Does not apply, or was not checked: counts for nothing either way.
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    Skipped,
}

/// One check's line in the breakdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckScore {
    pub check: GeoCheck,
    pub tier: Tier,
    pub status: Status,
    /// The severity it failed with; `None` unless it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    /// GEO points the failure costs, unrounded; 0 unless it failed.
    pub points_lost: f64,
    /// GEO points fixing it alone would give back, unrounded: the unrounded score without the
    /// failure minus the unrounded score with it (equal to `points_lost`, as no check can cost
    /// more than its weight and the score never reaches its floor). Overall gains
    /// [`GEO_WEIGHT`] times this.
    pub points_regained: f64,
}

/// A crawl's GEO score and how it came about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeoScore {
    /// 0-100.
    pub score: u8,
    pub checks_passed: u16,
    /// Checks that were not skipped.
    pub checks_applicable: u16,
    /// Every check in [`GeoCheck::ALL`] order, skipped ones included.
    pub checks: Vec<CheckScore>,
}

/// The GEO score of these outcomes. A check missing from `outcomes` is skipped; one listed twice
/// counts once, as its first entry. `None` when no check applies, so there is nothing to score.
pub fn geo_score(outcomes: &[(GeoCheck, Outcome)]) -> Option<GeoScore> {
    let outcome = |c: GeoCheck| {
        outcomes
            .iter()
            .find(|(k, _)| *k == c)
            .map_or(Outcome::Skipped, |(_, o)| *o)
    };
    let applicable = |c: &GeoCheck| outcome(*c) != Outcome::Skipped;
    let tier_weight = |t: Tier| -> f64 {
        GeoCheck::ALL
            .iter()
            .filter(|c| c.tier() == t && applicable(c))
            .map(|c| c.weight())
            .sum()
    };
    let (essential, recommended) = (tier_weight(Tier::Essential), tier_weight(Tier::Recommended));
    // The points on offer: a tier with nothing applicable gives its points to the other.
    let available: f64 = [
        (Tier::Essential, essential),
        (Tier::Recommended, recommended),
    ]
    .iter()
    .filter(|(_, w)| *w > 0.0)
    .map(|(t, _)| t.points())
    .sum();
    if available == 0.0 {
        return None;
    }
    // GEO points per unit of weight in each tier.
    let rate = |t: Tier| {
        let w = match t {
            Tier::Essential => essential,
            Tier::Recommended => recommended,
        };
        if w > 0.0 {
            t.points() * 100.0 / available / w
        } else {
            0.0
        }
    };

    let checks: Vec<CheckScore> = GeoCheck::ALL
        .into_iter()
        .map(|c| {
            let (status, severity, lost) = match outcome(c) {
                Outcome::Pass => (Status::Pass, None, 0.0),
                Outcome::Skipped => (Status::Skipped, None, 0.0),
                Outcome::Fail { severity, share } => {
                    let share = share.clamp(0.0, 1.0);
                    let cost = (cap(severity) * (0.25 + 0.75 * share)).min(c.weight());
                    (Status::Fail, Some(severity), cost * rate(c.tier()))
                }
            };
            CheckScore {
                check: c,
                tier: c.tier(),
                status,
                severity,
                points_lost: lost,
                points_regained: lost,
            }
        })
        .collect();
    let lost: f64 = checks.iter().map(|c| c.points_lost).sum();
    let count = |s: Status| checks.iter().filter(|c| c.status == s).count() as u16;
    Some(GeoScore {
        score: (100.0 - lost).clamp(0.0, 100.0).round() as u8,
        checks_passed: count(Status::Pass),
        checks_applicable: count(Status::Pass) + count(Status::Fail),
        checks,
    })
}

/// The GEO score of a crawl: its access report's findings under the owner's `intent`, and the
/// SEO checks' failing `counts` over its `pages` (as in [`codoseo_core::report::CrawlReport`]).
pub fn assess(
    report: &AccessReport,
    intent: &Intent,
    counts: &[(CheckId, u32)],
    pages: u32,
) -> Option<GeoScore> {
    geo_score(&outcomes(report, intent, counts, pages))
}

/// What each check came to on a crawl; see [`assess`].
pub fn outcomes(
    report: &AccessReport,
    intent: &Intent,
    counts: &[(CheckId, u32)],
    pages: u32,
) -> Vec<(GeoCheck, Outcome)> {
    let evaluated = evaluated_kinds(report);
    let found = findings(report, intent);
    // Without a readable robots.txt there are no verdicts, so `findings` judges nothing that
    // rests on them: those checks are skipped rather than passed.
    let robots_failed = found
        .iter()
        .any(|f| f.kind == FindingKind::RobotsUnavailable);
    let important = report.important.len();
    let essential = GeoCheck::ALL.into_iter().filter_map(|c| {
        let kind = c.finding_kind()?;
        let rests_on_verdicts = matches!(
            kind,
            FindingKind::BotsBlocked
                | FindingKind::BotsNotBlocked
                | FindingKind::FetchBlocked
                | FindingKind::ContentMissing
        );
        if !evaluated.contains(&kind) || (robots_failed && rests_on_verdicts) {
            return Some((c, Outcome::Skipped));
        }
        Some((c, worst(found.iter().filter(|f| f.kind == kind), important)))
    });

    let seo = |ids: &[CheckId]| -> Outcome {
        if pages == 0 {
            return Outcome::Skipped;
        }
        ids.iter()
            .filter_map(|id| {
                let (_, n) = counts.iter().find(|(c, _)| c == id)?;
                let d = def(*id);
                let share = match d.scope {
                    Scope::SiteWide => 1.0,
                    _ => f64::from(*n) / f64::from(pages),
                };
                Some(Outcome::Fail {
                    severity: d.severity,
                    share,
                })
            })
            .reduce(merge)
            .unwrap_or(Outcome::Pass)
    };
    let llms = match &report.llms_txt {
        None => Outcome::Skipped,
        Some(r) => match r.state {
            LlmsState::Available => Outcome::Pass,
            LlmsState::Absent | LlmsState::SoftNotFound | LlmsState::Unavailable => Outcome::Fail {
                severity: Severity::Notice,
                share: 1.0,
            },
        },
    };

    essential
        .chain([
            (GeoCheck::JsonldInvalid, seo(&[CheckId::JsonldInvalid])),
            (GeoCheck::TitleMissing, seo(&[CheckId::TitleMissing])),
            (
                GeoCheck::DescriptionMissing,
                seo(&[CheckId::DescriptionMissing]),
            ),
            (
                GeoCheck::Sitemap,
                seo(&[CheckId::SitemapMissing, CheckId::SitemapNon200]),
            ),
            (GeoCheck::LlmsTxt, llms),
            // Extension points: until CodoSEO checks them they are not checked, so skipped.
            (GeoCheck::ContentSignals, Outcome::Skipped),
            (GeoCheck::MarkdownNegotiation, Outcome::Skipped),
        ])
        .collect()
}

/// The findings of one kind as one outcome: the worst severity and the widest share; a pass when
/// there are none.
fn worst<'a>(found: impl Iterator<Item = &'a Finding>, important: usize) -> Outcome {
    found
        .map(|f| Outcome::Fail {
            severity: f.severity,
            share: finding_share(f, important),
        })
        .reduce(merge)
        .unwrap_or(Outcome::Pass)
}

/// Two failures of one check as one: the worse severity, the wider share.
fn merge(a: Outcome, b: Outcome) -> Outcome {
    match (a, b) {
        (
            Outcome::Fail {
                severity: s1,
                share: x1,
            },
            Outcome::Fail {
                severity: s2,
                share: x2,
            },
        ) => Outcome::Fail {
            severity: s1.min(s2),
            share: x1.max(x2),
        },
        (Outcome::Fail { .. }, _) => a,
        _ => b,
    }
}

/// The part of the important pages a finding is about. robots.txt failing is site-wide, and a
/// blocked probe is about the bot (a firewall rule), not the few pages probed, so both are 1; so
/// is a finding whose evidence names no pages.
fn finding_share(f: &Finding, important: usize) -> f64 {
    let total = f.evidence.urls.total as usize;
    match f.kind {
        FindingKind::RobotsUnavailable | FindingKind::FetchBlocked => 1.0,
        _ if total == 0 || important == 0 => 1.0,
        _ => (total as f64 / important as f64).min(1.0),
    }
}

/// `round(0.6 x SEO + 0.4 x GEO)`.
pub fn overall_score(seo: u8, geo: u8) -> u8 {
    (SEO_WEIGHT * f64::from(seo) + GEO_WEIGHT * f64::from(geo))
        .round()
        .clamp(0.0, 100.0) as u8
}

/// Overall `(checks passed, checks total)`: the SEO checks plus the applicable GEO checks. The
/// Recommended tier reads four SEO checks, so those count in both.
pub fn overall_checks(seo_passed: u16, seo_total: u16, geo: &GeoScore) -> (u16, u16) {
    (
        seo_passed + geo.checks_passed,
        seo_total + geo.checks_applicable,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use GeoCheck::*;
    use Outcome::{Fail, Pass, Skipped};
    use Severity::{Critical, Notice, Warning};

    type Outcomes = Vec<(GeoCheck, Outcome)>;

    /// Every check that is checked today, all passing.
    fn all_pass() -> Vec<(GeoCheck, Outcome)> {
        GeoCheck::ALL
            .into_iter()
            .filter(|c| !matches!(c, ContentSignals | MarkdownNegotiation))
            .map(|c| (c, Pass))
            .collect()
    }

    fn with(changes: &[(GeoCheck, Outcome)]) -> Vec<(GeoCheck, Outcome)> {
        let mut out: Vec<(GeoCheck, Outcome)> = changes.to_vec();
        out.extend(all_pass());
        out
    }

    fn score(outcomes: &[(GeoCheck, Outcome)]) -> u8 {
        geo_score(outcomes).expect("something applies").score
    }

    fn fail(severity: Severity, share: f64) -> Outcome {
        Fail { severity, share }
    }

    #[test]
    fn weights_add_up_per_tier() {
        let tier = |t: Tier| -> f64 {
            all_pass()
                .iter()
                .filter(|(c, _)| c.tier() == t)
                .map(|(c, _)| c.weight())
                .sum()
        };
        // 15 + 15 + 5 + 15 + 5 + 5 and 5 + 5 + 5 + 5 + 1.
        assert_eq!(tier(Tier::Essential), 60.0);
        assert_eq!(tier(Tier::Recommended), 21.0);
    }

    #[test]
    fn scores() {
        // Essential: 70 points over weight 60. Recommended: 30 over 21.
        let cases: &[(&str, Outcomes, u8)] = &[
            ("all pass", all_pass(), 100),
            (
                "robots.txt down: 70 x 15/60",
                with(&[(RobotsUnavailable, fail(Critical, 1.0))]),
                83,
            ),
            (
                "a Critical block on half the pages: 70 x 15 x 0.625/60",
                with(&[(BotsBlocked, fail(Critical, 0.5))]),
                89,
            ),
            (
                "a Notice-level block costs a quarter of a Notice, of a Critical weight",
                with(&[(BotsBlocked, fail(Notice, 0.0))]),
                100,
            ),
            (
                "titles missing everywhere: 30 x 5/21",
                with(&[(TitleMissing, fail(Warning, 1.0))]),
                93,
            ),
            (
                "no llms.txt: 30 x 1/21",
                with(&[(LlmsTxt, fail(Notice, 1.0))]),
                99,
            ),
            (
                "a severity above the nominal weight is capped at the weight",
                with(&[(LlmsTxt, fail(Critical, 1.0))]),
                99,
            ),
            (
                "everything failing everywhere",
                GeoCheck::ALL
                    .into_iter()
                    .map(|c| (c, fail(Critical, 1.0)))
                    .collect(),
                0,
            ),
        ];
        for (name, outcomes, want) in cases {
            assert_eq!(score(outcomes), *want, "{name}");
        }
    }

    #[test]
    fn essential_is_worth_70_and_recommended_30() {
        let failing = |t: Tier| -> Vec<(GeoCheck, Outcome)> {
            GeoCheck::ALL
                .into_iter()
                .map(|c| {
                    (
                        c,
                        if c.tier() == t {
                            fail(Critical, 1.0)
                        } else {
                            Pass
                        },
                    )
                })
                .collect()
        };
        assert_eq!(score(&failing(Tier::Essential)), 30);
        assert_eq!(score(&failing(Tier::Recommended)), 70);
        // The tiers split by points, the checks inside them by severity: an Essential robots.txt
        // failure (Critical) costs 17.5, a Recommended Warning everywhere 30 x 5/21 = 7.1, an
        // Essential Warning everywhere 70 x 5/60 = 5.8.
        assert_eq!(
            score(&with(&[(RobotsUnavailable, fail(Critical, 1.0))])),
            83
        );
        assert_eq!(score(&with(&[(TitleMissing, fail(Warning, 1.0))])), 93);
        assert_eq!(score(&with(&[(FetchBlocked, fail(Warning, 1.0))])), 94);
    }

    #[test]
    fn partial_share_costs_between_a_quarter_and_all_of_the_weight() {
        let lost = |share: f64| {
            geo_score(&with(&[(ContentMissing, fail(Warning, share))]))
                .unwrap()
                .checks
                .iter()
                .map(|c| c.points_lost)
                .sum::<f64>()
        };
        let full = 70.0 * 5.0 / 60.0;
        assert!((lost(0.0) - full * 0.25).abs() < 1e-9);
        assert!((lost(0.5) - full * 0.625).abs() < 1e-9);
        assert!((lost(1.0) - full).abs() < 1e-9);
        assert!((lost(7.0) - full).abs() < 1e-9, "share is clamped");
    }

    #[test]
    fn skipped_checks_are_left_out_of_both_sides() {
        // Without the probes and the content check, Essential is split over 50: a failing
        // robots.txt costs 70 x 15/50 = 21 instead of 17.5.
        let outcomes = with(&[
            (FetchBlocked, Skipped),
            (ContentMissing, Skipped),
            (RobotsUnavailable, fail(Critical, 1.0)),
        ]);
        let s = geo_score(&outcomes).unwrap();
        assert_eq!(s.score, 79);
        assert_eq!((s.checks_passed, s.checks_applicable), (8, 9));
        let skipped: Vec<GeoCheck> = s
            .checks
            .iter()
            .filter(|c| c.status == Status::Skipped)
            .map(|c| c.check)
            .collect();
        assert_eq!(
            skipped,
            [
                FetchBlocked,
                ContentMissing,
                ContentSignals,
                MarkdownNegotiation
            ]
        );
        // A skipped check never costs anything, whatever else is listed for it later.
        assert_eq!(score(&with(&[(LlmsTxt, Skipped)])), 100);
    }

    #[test]
    fn a_tier_with_nothing_applicable_gives_its_points_to_the_other() {
        let only_recommended: Vec<(GeoCheck, Outcome)> =
            vec![(TitleMissing, fail(Warning, 1.0)), (LlmsTxt, Pass)];
        // 100 x 5/6.
        assert_eq!(score(&only_recommended), 17);
        assert!(geo_score(&[]).is_none());
        assert!(geo_score(&[(ContentSignals, Skipped)]).is_none());
    }

    #[test]
    fn counts_and_points_regained() {
        let s = geo_score(&with(&[
            (BotsBlocked, fail(Critical, 1.0)),
            (Sitemap, fail(Notice, 1.0)),
        ]))
        .unwrap();
        assert_eq!((s.checks_passed, s.checks_applicable), (9, 11));
        let line = |c: GeoCheck| s.checks.iter().find(|l| l.check == c).unwrap();
        let bots = line(BotsBlocked);
        assert_eq!((bots.tier, bots.status), (Tier::Essential, Status::Fail));
        assert_eq!(bots.severity, Some(Critical));
        assert!((bots.points_regained - 17.5).abs() < 1e-9);
        let sitemap = line(Sitemap);
        assert!((sitemap.points_lost - 30.0 / 21.0).abs() < 1e-9);
        assert_eq!(line(TitleMissing).points_lost, 0.0);
        // Fixing both gives back exactly what they cost.
        let lost: f64 = s.checks.iter().map(|c| c.points_regained).sum();
        assert_eq!(s.score, (100.0 - lost).round() as u8);
    }

    #[test]
    fn overall_blends_seo_and_geo() {
        let cases = [
            (100, 100, 100),
            (0, 0, 0),
            (80, 50, 68),
            (91, 64, 80),
            (50, 51, 50),
            (100, 0, 60),
            (0, 100, 40),
        ];
        for (seo, geo, want) in cases {
            assert_eq!(overall_score(seo, geo), want, "{seo} / {geo}");
        }
        let geo = geo_score(&all_pass()).unwrap();
        assert_eq!(overall_checks(40, 44, &geo), (51, 55));
    }
}
