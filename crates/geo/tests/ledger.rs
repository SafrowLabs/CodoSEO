use std::collections::{BTreeSet, HashSet};

use codoseo_geo::eligibility::engines;
use codoseo_geo::findings::{BotEvidence, Evidence, FindingKind, Grade, ROBOTS_DOC};
use codoseo_geo::ledger::{Lag, OPERATOR_LAGS, finding_sources, for_kind, lag, ledger};
use codoseo_geo::registry;
use codoseo_geo::robots::GroupMatch;

const ALL_KINDS: [FindingKind; 7] = [
    FindingKind::RobotsUnavailable,
    FindingKind::BotsBlocked,
    FindingKind::BotsNotBlocked,
    FindingKind::AnswersRestricted,
    FindingKind::FetchBlocked,
    FindingKind::ContentMissing,
    FindingKind::LlmsTxtUnavailable,
];

/// The checks that never open an incident but still show a grade.
const CHECKS: [&str; 7] = [
    "robots_verdict",
    "eligibility",
    "declared_preferences",
    "synthetic_probe",
    "content_verdict",
    "llms_format",
    "llms_size_and_type",
];

#[test]
fn every_finding_kind_has_an_entry_keyed_by_its_slug() {
    for kind in ALL_KINDS {
        assert_eq!(for_kind(kind).key, kind.slug());
    }
}

#[test]
fn the_non_incident_checks_have_entries() {
    let keys: BTreeSet<&str> = ledger().iter().map(|e| e.key).collect();
    for key in CHECKS {
        assert!(keys.contains(key), "{key} is missing from the ledger");
    }
    assert_eq!(keys.len(), ledger().len(), "ledger keys are unique");
    assert_eq!(ledger().len(), ALL_KINDS.len() + CHECKS.len());
}

#[test]
fn todays_grades_are_unchanged() {
    use FindingKind::*;
    for (kind, grade) in [
        (RobotsUnavailable, Grade::A),
        (BotsBlocked, Grade::A),
        (BotsNotBlocked, Grade::A),
        (AnswersRestricted, Grade::A),
        (FetchBlocked, Grade::B),
        (ContentMissing, Grade::AMinus),
        (LlmsTxtUnavailable, Grade::A),
    ] {
        assert_eq!(for_kind(kind).grade, grade, "{kind:?}");
    }
}

#[test]
fn every_entry_says_what_it_rests_on_and_its_limits() {
    for e in ledger() {
        assert!(!e.rests_on.is_empty(), "{}", e.key);
        assert!(!e.limits.is_empty(), "{}", e.key);
        assert!(
            e.explain
                .starts_with(&format!("Evidence grade {}:", e.grade.label())),
            "{}: {}",
            e.key,
            e.explain
        );
    }
}

#[test]
fn every_source_is_https_and_unique_within_its_entry() {
    for e in ledger() {
        let mut seen = HashSet::new();
        for s in e.sources {
            assert!(s.starts_with("https://"), "{}: {s}", e.key);
            assert!(seen.insert(*s), "{}: {s} twice", e.key);
        }
    }
    for (_, url) in codoseo_geo::ledger::VENDOR_DOCS {
        assert!(url.starts_with("https://"), "{url}");
    }
    for l in OPERATOR_LAGS {
        assert!(l.source.starts_with("https://"), "{}", l.source);
    }
}

#[test]
fn the_eligibility_entry_cites_every_engine() {
    let cited: BTreeSet<&str> = ledger()
        .iter()
        .find(|e| e.key == "eligibility")
        .unwrap()
        .sources
        .iter()
        .copied()
        .collect();
    let engines: BTreeSet<&str> = engines().iter().map(|e| e.source_url).collect();
    assert_eq!(cited, engines);
}

#[test]
fn grades_serialise_with_their_labels() {
    for (g, s) in [
        (Grade::A, "A"),
        (Grade::AMinus, "A-"),
        (Grade::B, "B"),
        (Grade::C, "C"),
        (Grade::X, "X"),
    ] {
        assert_eq!(g.label(), s);
        assert_eq!(serde_json::to_string(&g).unwrap(), format!("\"{s}\""));
        assert_eq!(
            serde_json::from_str::<Grade>(&format!("\"{s}\"")).unwrap(),
            g
        );
    }
}

#[test]
fn fixed_sources_lead_and_evidence_adds_its_bots() {
    assert_eq!(
        finding_sources(FindingKind::RobotsUnavailable, &Evidence::default()),
        vec![ROBOTS_DOC.to_owned()]
    );
    let ev = bots(&["GPTBot", "OAI-SearchBot", "ClaudeBot"]);
    let gpt = registry().bot("GPTBot").unwrap().source_url.clone();
    let claude = registry().bot("ClaudeBot").unwrap().source_url.clone();
    // GPTBot and OAI-SearchBot share one page, cited once.
    assert_eq!(
        finding_sources(FindingKind::BotsBlocked, &ev),
        vec![gpt, claude]
    );
}

// ---- lag -------------------------------------------------------------------------------------

fn bots(tokens: &[&str]) -> Evidence {
    Evidence {
        bots: tokens
            .iter()
            .map(|t| {
                let bot = registry().bot(t).expect(t);
                BotEvidence {
                    token: bot.token.clone(),
                    operator: bot.operator.clone(),
                    purpose: bot.purpose,
                    honours: bot.honours_robots,
                    urls_blocked: 1,
                    urls_total: 1,
                    home_blocked: true,
                    rule: None,
                    group: GroupMatch::Named,
                }
            })
            .collect(),
        ..Evidence::default()
    }
}

const OPENAI: &str = "https://developers.openai.com/api/docs/bots";
const DUCKDUCKGO: &str = "https://duckduckgo.com/duckduckgo-help-pages/results/duckassistbot";

#[test]
fn openai_documents_a_day() {
    assert_eq!(
        lag(FindingKind::BotsBlocked, &bots(&["OAI-SearchBot"])),
        Lag {
            hours: 24,
            source: Some(OPENAI),
            grade: Grade::A
        }
    );
}

#[test]
fn duckduckgo_documents_three_days() {
    assert_eq!(
        lag(FindingKind::BotsNotBlocked, &bots(&["DuckAssistBot"])),
        Lag {
            hours: 72,
            source: Some(DUCKDUCKGO),
            grade: Grade::A
        }
    );
}

#[test]
fn google_caches_robots_txt_for_a_day() {
    assert_eq!(
        lag(FindingKind::BotsBlocked, &bots(&["Google-Extended"])),
        Lag {
            hours: 24,
            source: Some(ROBOTS_DOC),
            grade: Grade::A
        }
    );
}

#[test]
fn an_operator_that_publishes_nothing_is_an_unverified_day() {
    assert_eq!(
        lag(FindingKind::BotsBlocked, &bots(&["ClaudeBot"])),
        Lag {
            hours: 24,
            source: None,
            grade: Grade::X
        }
    );
}

#[test]
fn a_mixed_set_takes_the_longest() {
    assert_eq!(
        lag(
            FindingKind::BotsBlocked,
            &bots(&["OAI-SearchBot", "DuckAssistBot", "ClaudeBot"])
        ),
        Lag {
            hours: 72,
            source: Some(DUCKDUCKGO),
            grade: Grade::A
        }
    );
    // A tie rests on its weakest part: OpenAI's day is documented, Anthropic's is not.
    assert_eq!(
        lag(FindingKind::BotsBlocked, &bots(&["GPTBot", "ClaudeBot"])),
        Lag {
            hours: 24,
            source: None,
            grade: Grade::X
        }
    );
}

#[test]
fn kinds_without_a_documented_lag_use_codoseos_own_debounce() {
    for kind in [
        FindingKind::AnswersRestricted,
        FindingKind::FetchBlocked,
        FindingKind::ContentMissing,
        FindingKind::LlmsTxtUnavailable,
    ] {
        assert_eq!(
            lag(kind, &bots(&["DuckAssistBot"])),
            Lag {
                hours: 1,
                source: None,
                grade: Grade::X
            },
            "{kind:?}"
        );
    }
}

#[test]
fn a_broken_robots_txt_waits_for_googles_cache() {
    assert_eq!(
        lag(FindingKind::RobotsUnavailable, &Evidence::default()),
        Lag {
            hours: 24,
            source: Some(ROBOTS_DOC),
            grade: Grade::A
        }
    );
}

#[test]
fn every_operator_with_a_documented_lag_is_in_the_registry() {
    for l in OPERATOR_LAGS {
        assert!(
            registry().bots.iter().any(|b| b.operator == l.operator),
            "{}",
            l.operator
        );
        assert_eq!(l.grade, Grade::A);
    }
}

// ---- docs ------------------------------------------------------------------------------------

#[test]
fn the_docs_grades_table_matches_the_ledger() {
    let doc = include_str!("../../../docs/ai-access.md");
    let section = doc
        .split("## Evidence grades")
        .nth(1)
        .expect("docs/ai-access.md has an Evidence grades section");
    let section = section.split("\n## ").next().unwrap();
    // `| **A** | means | `key`, `key` |`: the ledger column is the last cell.
    let row = |label: &str| -> (String, String) {
        let line = section
            .lines()
            .find(|l| l.starts_with(&format!("| **{label}** |")))
            .unwrap_or_else(|| panic!("no row for grade {label}"));
        let cells: Vec<&str> = line.trim_matches('|').split('|').collect();
        assert_eq!(cells.len(), 3, "{line}");
        (cells[1].to_owned(), cells[2].to_owned())
    };
    let labels = ["A", "A-", "B", "C", "X"];
    for label in labels {
        let (means, _) = row(label);
        assert!(!means.trim().is_empty(), "grade {label} is not explained");
    }
    for e in ledger() {
        let code = format!("`{}`", e.key);
        for label in labels {
            let (_, keys) = row(label);
            assert_eq!(
                keys.contains(&code),
                label == e.grade.label(),
                "{} is grade {} in the ledger; check the {label} row",
                e.key,
                e.grade.label()
            );
        }
    }
}
