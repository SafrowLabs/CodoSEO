//! The SEO tab of `/s/{site}/policy`: the preferred address, what search engines should index, the
//! URL parameters that don't change the page, the pages that must always work, the check
//! thresholds and the exceptions, each with its compare line against the latest crawl.
//!
//! The form posts flat fields: `a.host` and `a.slash`; `ix.pattern` and `ix.stance` once per
//! index rule, in order (a rule with a blank pattern is dropped), and `ix.default`; `params` and
//! `pagination` as text; `must`, one path per line; `t.{key}` per threshold (blank or the default
//! is the default; `t.slow` in seconds); `ex.check`, `ex.scope` and `ex.note` once per exception
//! (a blank scope is dropped), with `ex.form` saying the exceptions were on the form. A section
//! missing from the form keeps what the policy has.

use codoseo_checks::{Scope, def, duration_words};
use codoseo_core::check::CheckId;
use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_policy::seo::{EVERYTHING_ELSE, Preferred};
use codoseo_policy::{
    Address, DEFAULT_THRESHOLDS, Exception, HostPref, IndexRule, IndexStance, PathPattern,
    PolicyError, PolicyReport, RuleId, RulePart, SeoRules, Slash, Status, Thresholds,
};

use crate::routes::ai_access::plural;
use crate::routes::policy::{Cmp, SelectOpt, cmp};

/// One option of a segmented control.
pub struct Opt {
    pub id: String,
    pub value: &'static str,
    pub label: String,
    pub checked: bool,
}

/// One index rule's row.
pub struct IndexRowView {
    pub pattern: String,
    /// `6 pages`, `4 URLs linked`, or empty for a new rule.
    pub note: String,
    pub note_id: String,
    pub options: Vec<SelectOpt>,
    pub cmp: Cmp,
    /// The row's place, for "Remove".
    pub index: usize,
}

/// One must-work page's row.
pub struct MustRowView {
    pub path: String,
    /// `Home`, `Listed`, `Starred`.
    pub why: String,
    pub cmp: Cmp,
}

/// One check threshold's row.
pub struct ThresholdRowView {
    /// The form field: `t.title_max`.
    pub name: String,
    pub label: &'static str,
    /// `Default 60 characters`.
    pub default: String,
    /// `over`, `under`, `more than`.
    pub pre: &'static str,
    pub value: String,
    pub unit: &'static str,
    /// `0.1` for seconds, `1` otherwise.
    pub step: &'static str,
    pub min: String,
    pub max: String,
    /// The image alt row is a flag/off choice, not a number.
    pub options: Vec<SelectOpt>,
    pub cmp: Cmp,
}

/// One exception's row.
pub struct ExceptionRowView {
    pub index: usize,
    pub options: Vec<SelectOpt>,
    pub scope: String,
    pub note: String,
    pub cmp: Cmp,
}

/// What the threshold and exception rows count on the latest crawl.
#[derive(Default)]
pub struct SeoCounts {
    /// Per threshold check: pages flagged under the policy's thresholds and under the defaults.
    pub flags: Vec<(CheckId, u32, u32)>,
    /// Per exception, in order: the failing pages it leaves out of the score.
    pub matches: Vec<u32>,
    /// There is a crawl to count on.
    pub crawled: bool,
}

/// Blank rows to show after "Add rule" or "Add exception".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Blank {
    pub index: usize,
    pub exceptions: usize,
}

/// The SEO sections.
pub struct SeoView {
    pub hosts: Vec<Opt>,
    pub slashes: Vec<Opt>,
    pub address_cmp: Cmp,
    pub index_rows: Vec<IndexRowView>,
    pub default_options: Vec<SelectOpt>,
    pub default_note: String,
    pub default_cmp: Cmp,
    pub params: String,
    pub pagination: String,
    pub params_cmp: Cmp,
    pub must: String,
    pub must_rows: Vec<MustRowView>,
    pub must_cmp: Cmp,
    /// `Your plan checks up to 5 listed pages; the home page and starred pages are added.`
    pub must_cap: String,
    pub thresholds: Vec<ThresholdRowView>,
    pub exception_rows: Vec<ExceptionRowView>,
    /// Re-render the exceptions (after "Add exception" or "Remove"); the rest goes out of band.
    pub exceptions_table: bool,
    /// Every reason the rules can't be saved, one line each.
    pub errors: Vec<String>,
    pub address_errors: Vec<String>,
    pub index_errors: Vec<String>,
    pub params_errors: Vec<String>,
    pub must_errors: Vec<String>,
    pub threshold_errors: Vec<String>,
    pub exception_errors: Vec<String>,
}

fn stance_slug(s: IndexStance) -> &'static str {
    match s {
        IndexStance::Index => "index",
        IndexStance::KeepOut => "keep_out",
        IndexStance::None => "none",
    }
}

fn stance_options(current: IndexStance) -> Vec<SelectOpt> {
    [
        (IndexStance::Index, "Index"),
        (IndexStance::KeepOut, "Keep out of search"),
        (IndexStance::None, "No rule"),
    ]
    .into_iter()
    .map(|(s, label)| SelectOpt {
        value: stance_slug(s),
        label,
        selected: s == current,
    })
    .collect()
}

fn parse_stance(v: &str) -> Result<IndexStance, String> {
    match v {
        "index" => Ok(IndexStance::Index),
        "keep_out" => Ok(IndexStance::KeepOut),
        "none" => Ok(IndexStance::None),
        _ => Err(format!("unknown index stance \"{v}\"")),
    }
}

/// A must-work line as a path: a full URL of the site is cut down to its path and query.
fn must_path(line: &str) -> String {
    let line = line.trim();
    match url::Url::parse(line) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        },
        _ => line.to_owned(),
    }
}

/// The checks an exception may name: page checks, not the policy's own.
pub fn exceptable() -> impl Iterator<Item = CheckId> {
    CheckId::ALL
        .into_iter()
        .filter(|id| !id.is_policy() && def(*id).scope != Scope::SiteWide)
}

fn number<T: std::str::FromStr>(key: &str, raw: &str) -> Result<Option<T>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    raw.parse()
        .map(Some)
        .map_err(|_| format!("{key}: \"{raw}\" is not a whole number"))
}

/// The thresholds the form sets, on top of `current`: a field missing from the form keeps its
/// value; a blank one, or one set to the default, is the default.
fn thresholds_from(
    current: &Thresholds,
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<Thresholds, String> {
    let d = DEFAULT_THRESHOLDS;
    let mut t = current.clone();
    fn keep<T: PartialEq>(v: Option<T>, default: T) -> Option<T> {
        v.filter(|v| *v != default)
    }
    if let Some(v) = get("t.title_max") {
        t.title_max = keep(number("title_max", &v)?, d.title_max);
    }
    if let Some(v) = get("t.title_min") {
        t.title_min = keep(number("title_min", &v)?, d.title_min);
    }
    if let Some(v) = get("t.description_max") {
        t.description_max = keep(number("description_max", &v)?, d.description_max);
    }
    if let Some(v) = get("t.description_min") {
        t.description_min = keep(number("description_min", &v)?, d.description_min);
    }
    if let Some(v) = get("t.thin_words") {
        t.thin_words = keep(number("thin_words", &v)?, d.thin_words);
    }
    if let Some(v) = get("t.slow") {
        let v = v.trim();
        t.slow_ms = if v.is_empty() {
            None
        } else {
            let secs: f64 = v
                .parse()
                .ok()
                .filter(|s: &f64| s.is_finite() && *s >= 0.0)
                .ok_or_else(|| format!("slow page: \"{v}\" is not a number of seconds"))?;
            keep(
                Some((secs * 1000.0).round().min(f64::from(u32::MAX)) as u32),
                d.slow_ms,
            )
        };
    }
    if let Some(v) = get("t.max_redirect_hops") {
        t.max_redirect_hops = keep(number("max_redirect_hops", &v)?, d.max_redirect_hops);
    }
    if let Some(v) = get("t.image_alt") {
        t.image_alt = match v.as_str() {
            "flag" => None,
            "off" => Some(false),
            v => return Err(format!("unknown image alt setting \"{v}\"")),
        };
    }
    Ok(t)
}

/// The SEO rules the form describes: `current` with the sections the form sets. `op` adds a
/// blank index rule (`add`) or exception (`ex-add`), or drops one (`remove:2`, `ex-remove:1`), on
/// top.
pub fn from_form(
    current: &SeoRules,
    fields: &[(String, String)],
    op: Option<&str>,
) -> Result<(SeoRules, Blank), String> {
    let mut seo = current.clone();
    let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let all = |k: &str| -> Vec<&str> {
        fields
            .iter()
            .filter(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
            .collect()
    };
    let host = match get("a.host").unwrap_or("none") {
        "none" => None,
        "apex" => Some(HostPref::Apex),
        "www" => Some(HostPref::Www),
        v => return Err(format!("unknown host \"{v}\"")),
    };
    let slash = match get("a.slash").unwrap_or("without") {
        "without" => Slash::Without,
        "with" => Slash::With,
        "either" => Slash::Either,
        v => return Err(format!("unknown ending \"{v}\"")),
    };
    seo.address = host.map(|host| Address {
        host,
        trailing_slash: slash,
    });

    let patterns: Vec<&str> = fields
        .iter()
        .filter(|(k, _)| k == "ix.pattern")
        .map(|(_, v)| v.as_str())
        .collect();
    let stances: Vec<&str> = fields
        .iter()
        .filter(|(k, _)| k == "ix.stance")
        .map(|(_, v)| v.as_str())
        .collect();
    let mut rules = Vec::new();
    for (i, pattern) in patterns.iter().enumerate() {
        let stance = parse_stance(stances.get(i).copied().unwrap_or("none"))?;
        if pattern.trim().is_empty() || op == Some(&format!("remove:{i}")) {
            continue;
        }
        rules.push(IndexRule {
            pattern: PathPattern(pattern.trim().to_owned()),
            stance,
        });
    }
    seo.index = rules;
    seo.index_default = parse_stance(get("ix.default").unwrap_or("none"))?;

    seo.ignore_params = get("params")
        .unwrap_or_default()
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    seo.pagination_param =
        Some(get("pagination").unwrap_or_default().trim().to_owned()).filter(|p| !p.is_empty());
    seo.must_work = get("must")
        .unwrap_or_default()
        .lines()
        .map(must_path)
        .filter(|l| !l.is_empty())
        .collect();
    seo.thresholds = thresholds_from(&current.thresholds, &|k| get(k).map(str::to_owned))?;
    if get("ex.form").is_some() {
        let checks = all("ex.check");
        let scopes = all("ex.scope");
        let notes = all("ex.note");
        let mut exceptions = Vec::new();
        for (i, scope) in scopes.iter().enumerate() {
            if scope.trim().is_empty() || op == Some(&format!("ex-remove:{i}")) {
                continue;
            }
            let slug = checks.get(i).copied().unwrap_or_default();
            let check =
                CheckId::from_slug(slug).ok_or_else(|| format!("unknown check \"{slug}\""))?;
            exceptions.push(Exception {
                check,
                scope: PathPattern(scope.trim().to_owned()),
                note: notes.get(i).copied().unwrap_or_default().trim().to_owned(),
            });
        }
        seo.exceptions = exceptions;
    }
    Ok((
        seo,
        Blank {
            index: usize::from(op == Some("add")),
            exceptions: usize::from(op == Some("ex-add")),
        },
    ))
}

fn pages_cmp(id: String, n: u32, default: Option<u32>, crawled: bool, what: &str) -> Cmp {
    if !crawled {
        return Cmp {
            id,
            class: "is-off",
            head: "No crawl yet".to_owned(),
            text: "Counted after the first crawl.".to_owned(),
        };
    }
    let text = match default {
        Some(d) if d != n => format!("{what} (default flags {d})"),
        Some(_) => format!("{what}, as the default"),
        None => what.to_owned(),
    };
    Cmp {
        id,
        class: if n > 0 { "is-warn" } else { "is-ok" },
        head: plural(n as usize, "page", "pages"),
        text,
    }
}

/// The threshold rows, with what each flags on the latest crawl.
fn threshold_rows(t: &Thresholds, counts: &SeoCounts) -> Vec<ThresholdRowView> {
    let e = t.effective();
    let d = DEFAULT_THRESHOLDS;
    let flag = |id: CheckId| {
        counts
            .flags
            .iter()
            .find(|(c, ..)| *c == id)
            .map_or((0, 0), |&(_, n, d)| (n, d))
    };
    let row = |key: &str,
               label: &'static str,
               default: String,
               pre: &'static str,
               value: String,
               unit: &'static str,
               (min, max): (u32, u32),
               id: CheckId,
               what: &str| {
        let (n, dn) = flag(id);
        ThresholdRowView {
            name: format!("t.{key}"),
            label,
            default,
            pre,
            value,
            unit,
            step: "1",
            min: min.to_string(),
            max: max.to_string(),
            options: Vec::new(),
            cmp: pages_cmp(format!("cmp-t-{key}"), n, Some(dn), counts.crawled, what),
        }
    };
    let secs = |ms: u32| {
        if ms.is_multiple_of(1000) {
            (ms / 1000).to_string()
        } else {
            format!("{}", f64::from(ms) / 1000.0)
        }
    };
    let mut rows = vec![
        row(
            "title_max",
            "Title, longest",
            format!("Default {} characters", d.title_max),
            "over",
            e.title_max.to_string(),
            "chars",
            (10, 300),
            CheckId::TitleTooLong,
            &format!("over {}", e.title_max),
        ),
        row(
            "title_min",
            "Title, shortest",
            format!("Default {} characters", d.title_min),
            "under",
            e.title_min.to_string(),
            "chars",
            (1, 300),
            CheckId::TitleTooShort,
            &format!("under {}", e.title_min),
        ),
        row(
            "description_max",
            "Meta description, longest",
            format!("Default {} characters", d.description_max),
            "over",
            e.description_max.to_string(),
            "chars",
            (10, 1000),
            CheckId::DescriptionTooLong,
            &format!("over {}", e.description_max),
        ),
        row(
            "description_min",
            "Meta description, shortest",
            format!("Default {} characters", d.description_min),
            "under",
            e.description_min.to_string(),
            "chars",
            (1, 1000),
            CheckId::DescriptionTooShort,
            &format!("under {}", e.description_min),
        ),
        row(
            "thin_words",
            "Thin page",
            format!("Default under {} words", d.thin_words),
            "under",
            e.thin_words.to_string(),
            "words",
            (1, 10_000),
            CheckId::ThinContent,
            &format!("under {} words", e.thin_words),
        ),
        row(
            "slow",
            "Slow page",
            format!("Default over {} to first byte", duration_words(d.slow_ms)),
            "over",
            secs(e.slow_ms),
            "s",
            (0, 60),
            CheckId::SlowResponse,
            &format!("over {}", duration_words(e.slow_ms)),
        ),
        row(
            "max_redirect_hops",
            "Redirect hops",
            format!("Default more than {}", d.max_redirect_hops),
            "more than",
            e.max_redirect_hops.to_string(),
            "hops",
            (1, 10),
            CheckId::RedirectChain,
            &format!("more than {} hops", e.max_redirect_hops),
        ),
        row(
            "image_alt",
            "Images without alt",
            "Default flag".to_owned(),
            "",
            String::new(),
            "",
            (0, 0),
            CheckId::ImagesMissingAlt,
            if e.image_alt {
                "with images missing alt text"
            } else {
                "not checked"
            },
        ),
    ];
    if let Some(r) = rows.iter_mut().find(|r| r.name == "t.slow") {
        r.step = "0.1";
        r.min = "0.1".to_owned();
    }
    if let Some(r) = rows.last_mut() {
        r.options = vec![
            SelectOpt {
                value: "flag",
                label: "Flag",
                selected: e.image_alt,
            },
            SelectOpt {
                value: "off",
                label: "Don't flag",
                selected: !e.image_alt,
            },
        ];
        if !e.image_alt {
            r.cmp = Cmp {
                id: r.cmp.id.clone(),
                class: "is-off",
                head: "Off".to_owned(),
                text: "Images without alt text are not flagged.".to_owned(),
            };
        }
    }
    rows
}

fn check_options(current: CheckId) -> Vec<SelectOpt> {
    exceptable()
        .map(|id| SelectOpt {
            value: id.slug(),
            label: def(id).title,
            selected: id == current,
        })
        .collect()
}

/// The exception rows, with the pages each leaves out of the score on the latest crawl.
fn exception_rows(seo: &SeoRules, counts: &SeoCounts, blank: usize) -> Vec<ExceptionRowView> {
    let mut rows: Vec<ExceptionRowView> = seo
        .exceptions
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let n = counts.matches.get(i).copied().unwrap_or(0);
            let mut cmp = pages_cmp(
                format!("cmp-ex-{i}"),
                n,
                None,
                counts.crawled,
                "excluded from the health score",
            );
            if counts.crawled {
                cmp.class = "is-off";
            }
            ExceptionRowView {
                index: i,
                options: check_options(e.check),
                scope: e.scope.as_str().to_owned(),
                note: e.note.clone(),
                cmp,
            }
        })
        .collect();
    for k in 0..blank {
        let i = seo.exceptions.len() + k;
        rows.push(ExceptionRowView {
            index: i,
            options: check_options(CheckId::ThinContent),
            scope: String::new(),
            note: String::new(),
            cmp: Cmp {
                id: format!("cmp-ex-{i}"),
                class: "is-off",
                head: "New exception".to_owned(),
                text: "Type a path pattern such as /stores/*.".to_owned(),
            },
        });
    }
    rows
}

fn part_cmp(id: String, part: Option<&RulePart>, must: bool) -> Cmp {
    let Some(p) = part else {
        return Cmp {
            id,
            class: "is-off",
            head: "Not checked".to_owned(),
            text: String::new(),
        };
    };
    let (class, head) = match p.status {
        Status::Met => ("is-ok", "Met".to_owned()),
        Status::Gaps if must => ("is-bad", "Gap".to_owned()),
        Status::Gaps => ("is-bad", plural(p.gaps as usize, "gap", "gaps")),
        Status::Partly => ("is-warn", "Partly".to_owned()),
        Status::NotChecked => ("is-off", "Not checked".to_owned()),
        Status::LimitedEffect => ("is-warn", "Limited effect".to_owned()),
    };
    // A must-work line reads "200, indexable." or "answers 500.": capitalised as a sentence.
    let mut text = p.text.clone();
    if let Some(first) = text.get(..1)
        && must
    {
        text = first.to_uppercase() + &text[1..];
    }
    Cmp {
        id,
        class,
        head,
        text,
    }
}

/// The plan's cap on listed must-work pages, in words.
pub fn must_cap(plan: Plan) -> String {
    match PlanLimits::for_plan(plan).max_must_work {
        Some(n) => format!(
            "Your plan checks up to {} you list; the home page and starred pages are added on top.",
            plural(usize::from(n), "page", "pages")
        ),
        None => {
            "List as many as you need; the home page and starred pages are added on top.".to_owned()
        }
    }
}

/// Which section an error is about, by its path.
fn errors_for(errors: &[PolicyError], prefixes: &[&str]) -> Vec<String> {
    errors
        .iter()
        .filter(|e| prefixes.iter().any(|p| e.path.starts_with(p)))
        .map(|e| e.message.clone())
        .collect()
}

/// The tab for `seo`, compared in `rep`. `domain` names the hosts; `blank_rows` adds empty index
/// rules to fill in.
#[allow(clippy::too_many_arguments)] // one flat call per render, as both callers read it
pub fn view(
    seo: &SeoRules,
    rep: &PolicyReport,
    domain: &str,
    plan: Plan,
    errors: &[PolicyError],
    blank: Blank,
    counts: &SeoCounts,
    exceptions_table: bool,
) -> SeoView {
    let blank_rows = blank.index;
    let apex = domain.trim_start_matches("www.").to_ascii_lowercase();
    let origin = url::Url::parse(&format!("https://{apex}/")).ok();
    let host_now = seo.address.as_ref().map(|a| a.host);
    let slash_now = seo
        .address
        .as_ref()
        .map_or(Slash::Without, |a| a.trailing_slash);
    let label = |h: HostPref| {
        origin.as_ref().map_or_else(
            || apex.clone(),
            |o| Preferred::new(o, h, Slash::Either).base(),
        )
    };
    let hosts = [
        ("apex", Some(HostPref::Apex)),
        ("www", Some(HostPref::Www)),
        ("none", None),
    ]
    .into_iter()
    .map(|(value, h)| Opt {
        id: format!("a-host-{value}"),
        value,
        label: h.map_or_else(|| "Not checked".to_owned(), label),
        checked: h == host_now,
    })
    .collect();
    let slashes = [
        ("without", Slash::Without, "No trailing slash"),
        ("with", Slash::With, "Trailing slash"),
        ("either", Slash::Either, "Either"),
    ]
    .into_iter()
    .map(|(value, s, label)| Opt {
        id: format!("a-slash-{value}"),
        value,
        label: label.to_owned(),
        checked: s == slash_now,
    })
    .collect();

    let index = rep.rule(RuleId::Index);
    let parts: &[RulePart] = index.map_or(&[], |r| r.parts.as_slice());
    let mut index_rows: Vec<IndexRowView> = seo
        .index
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let part = parts.get(i);
            IndexRowView {
                pattern: r.pattern.as_str().to_owned(),
                note: part.map(|p| p.note.clone()).unwrap_or_default(),
                note_id: format!("note-ix-{i}"),
                options: stance_options(r.stance),
                cmp: part_cmp(format!("cmp-ix-{i}"), part, false),
                index: i,
            }
        })
        .collect();
    for k in 0..blank_rows {
        let i = seo.index.len() + k;
        index_rows.push(IndexRowView {
            pattern: String::new(),
            note: String::new(),
            note_id: format!("note-ix-{i}"),
            options: stance_options(IndexStance::KeepOut),
            cmp: Cmp {
                id: format!("cmp-ix-{i}"),
                class: "is-off",
                head: "New rule".to_owned(),
                text: "Type a path pattern such as /checkout/*.".to_owned(),
            },
            index: i,
        });
    }
    let default_part = parts.iter().find(|p| p.subject == EVERYTHING_ELSE);

    let must = rep.rule(RuleId::MustWork);
    let must_rows = must
        .map(|r| r.parts.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|p| MustRowView {
            path: p.subject.clone(),
            why: p.note.clone(),
            cmp: part_cmp(String::new(), Some(p), true),
        })
        .collect();

    SeoView {
        hosts,
        slashes,
        address_cmp: cmp("cmp-address".to_owned(), rep, RuleId::Address),
        index_rows,
        default_options: stance_options(seo.index_default),
        default_note: default_part.map(|p| p.note.clone()).unwrap_or_default(),
        default_cmp: part_cmp("cmp-ix-else".to_owned(), default_part, false),
        params: seo.ignore_params.join(", "),
        pagination: seo.pagination_param.clone().unwrap_or_default(),
        params_cmp: cmp("cmp-params".to_owned(), rep, RuleId::Params),
        must: seo.must_work.join("\n"),
        must_rows,
        must_cmp: cmp("cmp-must".to_owned(), rep, RuleId::MustWork),
        must_cap: must_cap(plan),
        thresholds: threshold_rows(&seo.thresholds, counts),
        exception_rows: exception_rows(seo, counts, blank.exceptions),
        exceptions_table,
        errors: errors.iter().map(ToString::to_string).collect(),
        address_errors: errors_for(errors, &["seo.address"]),
        index_errors: errors_for(errors, &["seo.index"]),
        params_errors: errors_for(errors, &["seo.ignore_params", "seo.pagination_param"]),
        must_errors: errors_for(errors, &["seo.must_work"]),
        threshold_errors: errors_for(errors, &["seo.thresholds"]),
        exception_errors: errors_for(errors, &["seo.exceptions"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn the_form_sets_the_four_sections_and_keeps_the_rest() {
        let current = SeoRules {
            exceptions: Vec::new(),
            thresholds: codoseo_policy::Thresholds {
                title_max: Some(65),
                ..Default::default()
            },
            ..SeoRules::default()
        };
        let f = fields(&[
            ("a.host", "www"),
            ("a.slash", "either"),
            ("ix.pattern", "/checkout/*"),
            ("ix.stance", "keep_out"),
            ("ix.pattern", "  "),
            ("ix.stance", "index"),
            ("ix.pattern", "/search*"),
            ("ix.stance", "none"),
            ("ix.default", "index"),
            ("params", "utm_*, gclid sort"),
            ("pagination", " page "),
            (
                "must",
                "/\n\n https://shop.example.com/pricing?x=1 \n/checkout/cart",
            ),
        ]);
        let (seo, blank) = from_form(&current, &f, None).unwrap();
        assert_eq!(blank, Blank::default());
        assert_eq!(
            seo.address,
            Some(Address {
                host: HostPref::Www,
                trailing_slash: Slash::Either
            })
        );
        let rules: Vec<(&str, IndexStance)> = seo
            .index
            .iter()
            .map(|r| (r.pattern.as_str(), r.stance))
            .collect();
        assert_eq!(
            rules,
            [
                ("/checkout/*", IndexStance::KeepOut),
                ("/search*", IndexStance::None)
            ]
        );
        assert_eq!(seo.index_default, IndexStance::Index);
        assert_eq!(seo.ignore_params, ["utm_*", "gclid", "sort"]);
        assert_eq!(seo.pagination_param.as_deref(), Some("page"));
        assert_eq!(seo.must_work, ["/", "/pricing?x=1", "/checkout/cart"]);
        assert_eq!(seo.thresholds.title_max, Some(65), "kept");

        let (seo, _) = from_form(&current, &f, Some("remove:0")).unwrap();
        assert_eq!(seo.index.len(), 1);
        let (_, blank) = from_form(&current, &f, Some("add")).unwrap();
        assert_eq!(blank.index, 1);
        let none = from_form(&current, &fields(&[("a.host", "none")]), None).unwrap();
        assert_eq!(none.0.address, None);
        assert!(from_form(&current, &fields(&[("ix.default", "bogus")]), None).is_err());
    }

    #[test]
    fn the_form_sets_thresholds_and_exceptions() {
        let current = SeoRules {
            thresholds: codoseo_policy::Thresholds {
                thin_words: Some(150),
                ..Default::default()
            },
            ..SeoRules::default()
        };
        let f = fields(&[
            ("t.title_max", "65"),
            ("t.title_min", "30"),
            ("t.slow", "1.5"),
            ("t.image_alt", "off"),
            ("ex.form", "1"),
            ("ex.check", "thin_content"),
            ("ex.scope", " /stores/* "),
            ("ex.note", "Short by design"),
            ("ex.check", "title_too_long"),
            ("ex.scope", ""),
            ("ex.note", ""),
        ]);
        let (seo, _) = from_form(&current, &f, None).unwrap();
        let t = &seo.thresholds;
        assert_eq!(t.title_max, Some(65));
        assert_eq!(t.title_min, None, "the default is stored as the default");
        assert_eq!(t.slow_ms, Some(1500));
        assert_eq!(t.image_alt, Some(false));
        assert_eq!(t.thin_words, Some(150), "a field not on the form is kept");
        assert_eq!(seo.exceptions.len(), 1);
        assert_eq!(seo.exceptions[0].check, CheckId::ThinContent);
        assert_eq!(seo.exceptions[0].scope.as_str(), "/stores/*");
        assert_eq!(seo.exceptions[0].note, "Short by design");
        let (seo, blank) = from_form(&current, &f, Some("ex-remove:0")).unwrap();
        assert!(seo.exceptions.is_empty());
        assert_eq!(blank.exceptions, 0);
        let (_, blank) = from_form(&current, &f, Some("ex-add")).unwrap();
        assert_eq!(blank.exceptions, 1);
        assert!(from_form(&current, &fields(&[("t.title_max", "abc")]), None).is_err());
        // No `ex.form`: the exceptions are kept.
        let kept = SeoRules {
            exceptions: seo.exceptions.clone(),
            ..SeoRules::default()
        };
        assert_eq!(
            from_form(&kept, &[], None).unwrap().0.exceptions,
            kept.exceptions
        );
    }

    #[test]
    fn threshold_rows_count_the_pages_flagged() {
        let counts = SeoCounts {
            flags: vec![(CheckId::TitleTooLong, 9, 21)],
            matches: Vec::new(),
            crawled: true,
        };
        let t = codoseo_policy::Thresholds {
            title_max: Some(65),
            ..Default::default()
        };
        let rows = threshold_rows(&t, &counts);
        let title = &rows[0];
        assert_eq!(title.value, "65");
        assert_eq!(title.cmp.head, "9 pages");
        assert_eq!(title.cmp.text, "over 65 (default flags 21)");
        assert_eq!(rows.iter().find(|r| r.name == "t.slow").unwrap().value, "1");
    }

    #[test]
    fn the_must_work_cap_follows_the_plan() {
        assert!(must_cap(Plan::Free).contains("up to 5 pages"));
        assert!(must_cap(Plan::Pro).contains("up to 50 pages"));
        assert!(must_cap(Plan::SelfHosted).starts_with("List as many"));
    }
}
