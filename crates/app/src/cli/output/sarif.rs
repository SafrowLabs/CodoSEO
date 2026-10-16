//! SARIF 2.1.0, for GitHub code scanning and any other SARIF reader.
//!
//! One run, one rule per check: the 44 SEO checks (`seo/<slug>`) and the 9 AI & GEO checks
//! (`geo/<kind>`), always all of them so rule indexes never move. A failing page check gives one
//! result per page (at most [`MAX_PER_RULE`]), a site-wide check one result at the start URL and
//! an AI access finding one result at the page it names first. Locations are the page URLs
//! themselves: a site has no source files to point at. Each `artifactLocation.uri` is the page's
//! path relative to the `SITE` base (`uriBaseId`), which `originalUriBaseIds` sets to the site's
//! origin, so the location resolves to the full URL. GitHub's upload rejects absolute URIs whose
//! scheme differs from its (file) source root, and a relative one keeps it accepted. The level comes
//! from the severity (critical `error`, warning `warning`, notice `note`); AI access rules carry
//! their evidence grade in `properties`. With a baseline each result has a `baselineState`.

use std::collections::BTreeMap;
use std::io::Write;

use codoseo_checks::{CHECKS, Category, CheckDef, Scope, def};
use codoseo_core::audit::Audit;
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::StopReason;
use codoseo_geo::findings::{Finding, FindingKind};
use codoseo_geo::ledger::for_kind;
use serde_json::{Value, json};
use xxhash_rust::xxh3::xxh3_64;

use super::super::gate::{Novelty, Verdict, audit_findings, finding_rule};
use super::CliError;

/// The schema the file names, as GitHub's documentation and SchemaStore publish it.
pub const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";
/// Results per page check; the rest are counted in the run's `properties.omitted`.
pub const MAX_PER_RULE: usize = 100;
const REPO: &str = "https://github.com/SafrowLabs/CodoSEO";

const KINDS: [FindingKind; 9] = FindingKind::ALL;

fn level(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "error",
        Severity::Warning => "warning",
        Severity::Notice => "note",
    }
}

fn category_label(c: Category) -> &'static str {
    match c {
        Category::Response => "Response",
        Category::Indexability => "Indexability",
        Category::Links => "Links",
        Category::OnPage => "On-page",
        Category::Content => "Content",
        Category::Technical => "Technical",
        Category::Sitemap => "Sitemap",
        Category::Social => "Social",
        Category::Schema => "Schema",
        Category::Policy => "Policy",
    }
}

fn severity_word(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "critical",
        Severity::Warning => "warning",
        Severity::Notice => "notice",
    }
}

fn seo_rule_id(id: CheckId) -> String {
    format!("seo/{}", id.slug())
}

fn geo_rule_id(kind: FindingKind) -> String {
    format!("geo/{}", kind.slug())
}

/// A `PascalCase` name from a slug, for the rule's `name`.
fn pascal(slug: &str) -> String {
    slug.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |f| {
                f.to_uppercase().chain(c).collect::<String>()
            })
        })
        .collect()
}

fn seo_rule(d: &CheckDef) -> Value {
    let scope = match d.scope {
        Scope::Page => "Checked on each page on its own.",
        Scope::Site => "Checked across the whole crawl and reported on each page it affects.",
        Scope::SiteWide => "A fact about the whole site, reported once.",
    };
    let policy = d.category == Category::Policy;
    let help = if policy {
        format!(
            "{}. Site policy check, severity {}. {scope} It runs when the policy file (codoseo.toml) declares its rule, and then counts toward the health score.",
            d.title,
            severity_word(d.severity)
        )
    } else {
        format!(
            "{}. {} check, severity {}. {scope} It counts toward the health score. Run `codoseo check <url>` to inspect one page.",
            d.title,
            category_label(d.category),
            severity_word(d.severity)
        )
    };
    json!({
        "id": seo_rule_id(d.id),
        "name": pascal(d.id.slug()),
        "shortDescription": { "text": d.title },
        "fullDescription": { "text": cap(&help) },
        "help": { "text": help, "markdown": format!("**{}**\n\n{help}", d.title) },
        "helpUri": if policy {
            format!("{REPO}/blob/main/docs/policy.md")
        } else {
            format!("{REPO}#the-44-seo-checks")
        },
        "defaultConfiguration": { "level": level(d.severity) },
        "properties": {
            "tags": ["seo", category_label(d.category).to_ascii_lowercase()],
            "category": category_label(d.category),
            "severity": severity_word(d.severity),
        },
    })
}

fn geo_title(kind: FindingKind) -> &'static str {
    match kind {
        FindingKind::RobotsUnavailable => "robots.txt answers 5xx or 429, so bots stay out",
        FindingKind::BotsBlocked => "AI search or answer bots you want are blocked by robots.txt",
        FindingKind::BotsNotBlocked => "Bots you want kept out can still crawl",
        FindingKind::AnswersRestricted => "Page markup keeps pages out of an engine's AI answers",
        FindingKind::FetchBlocked => {
            "A synthetic fetch with a bot's user agent is blocked, challenged or diverted"
        }
        FindingKind::ContentMissing => {
            "Important pages are an empty app shell in the HTML as served"
        }
        FindingKind::LlmsTxtUnavailable => "An adopted llms.txt stopped serving",
        FindingKind::ContentSignalsMissing => "robots.txt declares no Content Signals",
        FindingKind::MarkdownUnavailable => "The home page has no Markdown version for AI agents",
    }
}

fn geo_rule(kind: FindingKind) -> Value {
    let e = for_kind(kind);
    let title = geo_title(kind);
    let help = format!(
        "{title}. {} What it rests on: {} What it cannot tell: {} AI & GEO checks are judged under the default intent (AI search and user-triggered bots wanted) and never count toward the health score.",
        e.explain, e.rests_on, e.limits
    );
    let sources: Vec<String> = e.sources.iter().map(|s| format!("- {s}")).collect();
    let markdown = if sources.is_empty() {
        format!("**{title}**\n\n{help}")
    } else {
        format!("**{title}**\n\n{help}\n\nSources:\n{}", sources.join("\n"))
    };
    json!({
        "id": geo_rule_id(kind),
        "name": pascal(kind.slug()),
        "shortDescription": { "text": title },
        "fullDescription": { "text": cap(&help) },
        "help": { "text": help, "markdown": markdown },
        "helpUri": format!("{REPO}/blob/main/docs/ai-access.md"),
        // The severity of a finding follows the owner's intent; the rule only has a default.
        "defaultConfiguration": { "level": "warning" },
        "properties": {
            "tags": ["geo", "ai-access"],
            "evidenceGrade": e.grade.label(),
            "sources": e.sources,
        },
    })
}

/// The base every location is relative to.
const SITE_BASE: &str = "SITE";

/// A location for a URL: relative to [`SITE_BASE`] when it is on the site's origin (the root page
/// is `./`), absolute otherwise.
fn location(url: &str, origin: &str) -> Value {
    let base = format!("{}/", origin.trim_end_matches('/'));
    let artifact = match url.strip_prefix(&base) {
        Some("") => json!({ "uri": "./", "uriBaseId": SITE_BASE }),
        Some(rel) => json!({ "uri": rel, "uriBaseId": SITE_BASE }),
        None if url.trim_end_matches('/') == origin.trim_end_matches('/') => {
            json!({ "uri": "./", "uriBaseId": SITE_BASE })
        }
        None => json!({ "uri": url }),
    };
    json!([{ "physicalLocation": {
        "artifactLocation": artifact,
        "region": { "startLine": 1 },
    } }])
}

/// SARIF caps short and full descriptions at 1024 characters on GitHub.
fn cap(text: &str) -> String {
    const MAX: usize = 1000;
    if text.chars().count() <= MAX {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX - 1).collect();
    out.push('…');
    out
}

fn fingerprints(fp: &str) -> Value {
    // `primaryLocationLineHash` is the one GitHub reads; set, it also keeps upload-sarif from
    // trying to hash a source file that does not exist.
    json!({ "codoseo/v1": fp, "primaryLocationLineHash": fp })
}

fn fingerprint(parts: &[&str]) -> String {
    format!("{:016x}", xxh3_64(parts.join("\u{1f}").as_bytes()))
}

/// Where an AI access finding points: the page it names first, else the file it is about, else
/// the site.
fn finding_uri(f: &Finding, origin: &str) -> String {
    let base = origin.trim_end_matches('/');
    match f.kind {
        FindingKind::RobotsUnavailable | FindingKind::BotsBlocked | FindingKind::BotsNotBlocked
            if f.evidence.urls.sample.is_empty() =>
        {
            format!("{base}/robots.txt")
        }
        FindingKind::LlmsTxtUnavailable => format!("{base}/llms.txt"),
        FindingKind::ContentSignalsMissing => format!("{base}/robots.txt"),
        _ => f
            .evidence
            .urls
            .sample
            .first()
            .or_else(|| f.evidence.content.first().map(|c| &c.url))
            .or_else(|| f.evidence.probes.iter().find_map(|p| p.urls.first()))
            .cloned()
            .unwrap_or_else(|| format!("{base}/")),
    }
}

/// The first step of a finding's remedy, from the audit's `remedies`.
fn fix_of(audit: &Audit, f: &Finding) -> Option<String> {
    let remedies = audit.ai_access.as_ref()?.get("remedies")?.as_array()?;
    let kind = serde_json::to_value(f.kind).ok()?;
    remedies
        .iter()
        .find(|r| {
            r.get("kind") == Some(&kind)
                && r.get("subject").and_then(Value::as_str) == Some(&f.subject)
        })
        .and_then(|r| r.get("fix")?.as_str())
        .map(str::to_owned)
}

pub fn crawl(w: &mut impl Write, audit: &Audit, verdict: &Verdict) -> Result<(), CliError> {
    serde_json::to_writer_pretty(&mut *w, &document(audit, verdict))?;
    writeln!(w)?;
    Ok(())
}

/// The SARIF log for one audit.
pub fn document(audit: &Audit, verdict: &Verdict) -> Value {
    let mut rules: Vec<Value> = CHECKS.iter().map(seo_rule).collect();
    let mut index: BTreeMap<String, usize> = CHECKS
        .iter()
        .enumerate()
        .map(|(i, d)| (seo_rule_id(d.id), i))
        .collect();
    for kind in KINDS {
        index.insert(geo_rule_id(kind), rules.len());
        rules.push(geo_rule(kind));
    }

    let origin = audit.snapshot.origin.as_str();
    let start = audit.start_url.as_str();
    let mut results = Vec::new();
    let mut omitted: BTreeMap<String, usize> = BTreeMap::new();
    let state = |novelty: Novelty| verdict.baseline.then(|| novelty.sarif());

    for (id, _) in &audit.report.counts {
        let d = def(*id);
        let rule = seo_rule_id(*id);
        let mut push = |uri: &str, url: Option<&str>| {
            let mut r = json!({
                "ruleId": rule,
                "ruleIndex": index[&rule],
                "level": level(d.severity),
                "message": { "text": format!("{}: {uri}", d.title) },
                "locations": location(uri, origin),
                "partialFingerprints": fingerprints(&fingerprint(&[&rule, uri])),
            });
            if let Some(s) = state(verdict.issue(*id, url)) {
                r["baselineState"] = json!(s);
            }
            results.push(r);
        };
        if d.scope == Scope::SiteWide {
            push(start, None);
            continue;
        }
        let pages: Vec<&str> = audit
            .snapshot
            .pages
            .iter()
            .filter(|p| p.issues.has_check(*id))
            .map(|p| p.url.as_str())
            .collect();
        for url in pages.iter().take(MAX_PER_RULE) {
            push(url, Some(url));
        }
        if pages.len() > MAX_PER_RULE {
            omitted.insert(rule.clone(), pages.len() - MAX_PER_RULE);
        }
    }

    for f in audit_findings(audit) {
        let rule = geo_rule_id(f.kind);
        let uri = finding_uri(&f, origin);
        let fix = fix_of(audit, &f);
        let mut text = format!("{} {}", f.title, f.summary);
        if let Some(fix) = &fix {
            text.push_str(&format!("\nFix:\n{fix}"));
        }
        let mut r = json!({
            "ruleId": rule,
            "ruleIndex": index[&rule],
            "level": level(f.severity),
            "message": { "text": text },
            "locations": location(&uri, origin),
            "partialFingerprints": fingerprints(&fingerprint(&[&rule, &f.subject, origin])),
            "properties": {
                "subject": f.subject,
                "severity": severity_word(f.severity),
                "evidenceGrade": f.grade.label(),
                "sources": f.sources,
                "affectedUrls": f.evidence.urls.sample,
                "gate": finding_rule(&f),
            },
        });
        if let Some(s) = state(verdict.finding(&f)) {
            r["baselineState"] = json!(s);
        }
        results.push(r);
    }

    // A crawl that never got going (unreachable, blocked) did not do its job.
    let ran = matches!(
        audit.snapshot.stop,
        StopReason::Completed | StopReason::PageLimit | StopReason::TimeLimit
    );
    let mut properties = json!({
        "startUrl": start,
        "healthScore": audit.report.health_score,
        "checksPassed": audit.report.checks_passed,
        "checksTotal": audit.report.checks_total,
    });
    if !omitted.is_empty() {
        properties["omitted"] = json!(omitted);
    }
    json!({
        "$schema": SARIF_SCHEMA,
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": {
                "name": "CodoSEO",
                "version": audit.tool_version,
                "semanticVersion": audit.tool_version,
                "informationUri": "https://codoseo.com",
                "rules": rules,
            } },
            "originalUriBaseIds": {
                SITE_BASE: { "uri": format!("{}/", origin.trim_end_matches('/')) },
            },
            "automationDetails": {
                "id": format!("codoseo/{}/", audit.snapshot.origin.host_str().unwrap_or("site")),
            },
            "invocations": [{ "executionSuccessful": ran }],
            "results": results,
            "properties": properties,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_are_relative_to_the_site_base() {
        let o = "https://example.com/";
        let at = |u: &str| location(u, o)[0]["physicalLocation"]["artifactLocation"].clone();
        assert_eq!(
            at("https://example.com/"),
            json!({"uri": "./", "uriBaseId": "SITE"})
        );
        assert_eq!(
            at("https://example.com/a/b?x=1"),
            json!({"uri": "a/b?x=1", "uriBaseId": "SITE"})
        );
        assert_eq!(
            at("https://other.org/x"),
            json!({"uri": "https://other.org/x"})
        );
        assert_eq!(cap(&"x".repeat(2000)).chars().count(), 1000);
    }

    #[test]
    fn names_are_pascal_case() {
        assert_eq!(pascal("title_missing"), "TitleMissing");
        assert_eq!(pascal("http_4xx"), "Http4xx");
        assert_eq!(pascal("llms_txt_unavailable"), "LlmsTxtUnavailable");
    }
}
