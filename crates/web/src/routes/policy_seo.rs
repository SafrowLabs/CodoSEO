//! The SEO tab of `/s/{site}/policy`: the preferred address, what search engines should index, the
//! URL parameters that don't change the page and the pages that must always work, each with its
//! compare line against the latest crawl. Thresholds and exceptions come next.
//!
//! The form posts flat fields: `a.host` and `a.slash`; `ix.pattern` and `ix.stance` once per
//! index rule, in order (a rule with a blank pattern is dropped), and `ix.default`; `params` and
//! `pagination` as text; `must`, one path per line.

use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_policy::seo::{EVERYTHING_ELSE, Preferred};
use codoseo_policy::{
    Address, HostPref, IndexRule, IndexStance, PathPattern, PolicyError, PolicyReport, RuleId,
    RulePart, SeoRules, Slash, Status,
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

/// The four SEO sections.
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
    /// Every reason the rules can't be saved, one line each.
    pub errors: Vec<String>,
    pub address_errors: Vec<String>,
    pub index_errors: Vec<String>,
    pub params_errors: Vec<String>,
    pub must_errors: Vec<String>,
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

/// The SEO rules the form describes: `current` (thresholds and exceptions, which the tab doesn't
/// edit yet, are kept) with the four sections the form sets. `op` adds a blank index rule
/// (`add`) or drops one (`remove:2`) on top.
pub fn from_form(
    current: &SeoRules,
    fields: &[(String, String)],
    op: Option<&str>,
) -> Result<(SeoRules, usize), String> {
    let mut seo = current.clone();
    let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
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
    let blank_rows = usize::from(op == Some("add"));
    Ok((seo, blank_rows))
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
pub fn view(
    seo: &SeoRules,
    rep: &PolicyReport,
    domain: &str,
    plan: Plan,
    errors: &[PolicyError],
    blank_rows: usize,
) -> SeoView {
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
        errors: errors.iter().map(ToString::to_string).collect(),
        address_errors: errors_for(errors, &["seo.address"]),
        index_errors: errors_for(errors, &["seo.index"]),
        params_errors: errors_for(errors, &["seo.ignore_params", "seo.pagination_param"]),
        must_errors: errors_for(errors, &["seo.must_work"]),
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
        assert_eq!(blank, 0);
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
        assert_eq!(blank, 1);
        let none = from_form(&current, &fields(&[("a.host", "none")]), None).unwrap();
        assert_eq!(none.0.address, None);
        assert!(from_form(&current, &fields(&[("ix.default", "bogus")]), None).is_err());
    }

    #[test]
    fn the_must_work_cap_follows_the_plan() {
        assert!(must_cap(Plan::Free).contains("up to 5 pages"));
        assert!(must_cap(Plan::Pro).contains("up to 50 pages"));
        assert!(must_cap(Plan::SelfHosted).starts_with("List as many"));
    }
}
