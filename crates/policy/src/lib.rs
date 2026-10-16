//! The site policy: how the owner means a site to work for search engines and AI bots, so a crawl
//! can tell a mistake from a choice.
//!
//! One [`SitePolicy`] holds the SEO rules ([`SeoRules`]: preferred address, what to index, URL
//! parameters, pages that must work, check thresholds and exceptions) and the AI rules
//! ([`AiRules`]: the owner's [`Intent`] per kind of AI bot plus the fetch check switch). It is
//! stored as JSON on the site (`sites.policy`) and written by hand as the `[policy]` table of a
//! `codoseo.toml` file; both are the same serde shape, so a file exports and imports losslessly.
//!
//! The SEO types are all defined so the format is stable, but only the AI rules are evaluated so
//! far ([`evaluate`]); the SEO evaluators arrive with their checks.

use std::collections::{BTreeMap, BTreeSet};

use codoseo_core::check::CheckId;
use codoseo_geo::eligibility::DirectiveSlug;
use codoseo_geo::intent::{Intent, Stance};
use codoseo_geo::registry::Purpose;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod diff;
pub mod evaluate;
pub mod robots;
pub mod template;

pub use evaluate::{Gap, GapKind, PolicyInput, PolicyReport, RuleId, RuleResult, Status, evaluate};

/// The policy format's version. A file or stored value with another version is refused (a file)
/// or read as far as it goes (the database).
pub const POLICY_VERSION: u32 = 1;
/// The policy file the CLI reads when it is given none.
pub const DEFAULT_FILE: &str = "codoseo.toml";

/// Most index rules a policy may hold.
pub const MAX_INDEX_RULES: usize = 50;
/// Most ignored URL parameters.
pub const MAX_IGNORE_PARAMS: usize = 30;
/// Most exceptions.
pub const MAX_EXCEPTIONS: usize = 100;
/// Most must-work paths (the plan may allow fewer).
pub const MAX_MUST_WORK: usize = 100;
/// Most per-bot overrides of the AI rules.
pub const MAX_BOT_OVERRIDES: usize = 200;
/// Longest bot token the AI rules may hold.
pub const MAX_TOKEN_LEN: usize = 100;
/// Longest path pattern, parameter name or exception note.
pub const MAX_TEXT_LEN: usize = 200;

fn version_one() -> u32 {
    POLICY_VERSION
}

fn yes() -> bool {
    true
}

/// The whole policy of one site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SitePolicy {
    #[serde(default = "version_one")]
    pub version: u32,
    #[serde(default)]
    pub seo: SeoRules,
    #[serde(default)]
    pub ai: AiRules,
}

impl Default for SitePolicy {
    fn default() -> SitePolicy {
        SitePolicy {
            version: POLICY_VERSION,
            seo: SeoRules::default(),
            ai: AiRules::default(),
        }
    }
}

/// What the owner wants from AI bots: the [`Intent`] (per-purpose stances, per-bot overrides and
/// the page directives used on purpose, under the same keys as before) and whether the synthetic
/// fetch checks run after each crawl.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "AiRepr", into = "AiRepr")]
pub struct AiRules {
    pub intent: Intent,
    pub probes: bool,
}

impl Default for AiRules {
    fn default() -> AiRules {
        AiRules {
            intent: Intent::default(),
            probes: true,
        }
    }
}

/// [`AiRules`] as written: the intent's keys flattened next to `probes`. A plain struct rather
/// than `#[serde(flatten)]` so that unknown keys in a file are reported.
#[derive(Serialize, Deserialize)]
struct AiRepr {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    purposes: BTreeMap<Purpose, Stance>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    bots: BTreeMap<String, Stance>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    accepted_directives: BTreeSet<DirectiveSlug>,
    #[serde(default = "yes")]
    probes: bool,
}

impl From<AiRepr> for AiRules {
    fn from(r: AiRepr) -> AiRules {
        AiRules {
            intent: Intent {
                purposes: r.purposes,
                bots: r.bots,
                accepted_directives: r.accepted_directives,
            },
            probes: r.probes,
        }
    }
}

impl From<AiRules> for AiRepr {
    fn from(a: AiRules) -> AiRepr {
        AiRepr {
            purposes: a.intent.purposes,
            bots: a.intent.bots,
            accepted_directives: a.intent.accepted_directives,
            probes: a.probes,
        }
    }
}

/// The SEO rules. Every part is optional: a missing part is "not checked" (or the built-in
/// default, for thresholds).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeoRules {
    /// The one form every page should use; `None` is not checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Address>,
    /// Path rules, top to bottom; the first match wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index: Vec<IndexRule>,
    /// What the pages no rule matches should be ("everything else").
    #[serde(default, skip_serializing_if = "IndexStance::is_none")]
    pub index_default: IndexStance,
    /// Query parameters that don't change the page (`utm_*`, `gclid`); URLs differing only by them
    /// should canonicalise to the clean URL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_params: Vec<String>,
    /// The pagination parameter, whose pages canonicalise to themselves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination_param: Option<String>,
    /// Paths that must answer 200 and be indexable on every crawl. Starred pages and the home
    /// page are added when the policy is evaluated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_work: Vec<String>,
    #[serde(default, skip_serializing_if = "Thresholds::is_default")]
    pub thresholds: Thresholds,
    /// Checks that don't apply to part of the site.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exceptions: Vec<Exception>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub host: HostPref,
    #[serde(default)]
    pub trailing_slash: Slash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPref {
    /// `https://example.com`
    Apex,
    /// `https://www.example.com`
    Www,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slash {
    #[default]
    Without,
    With,
    Either,
}

/// One path rule of "what search engines should index".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRule {
    pub pattern: PathPattern,
    pub stance: IndexStance,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexStance {
    /// The pages should be indexable.
    Index,
    /// The pages should carry `noindex` (robots.txt alone stops crawling, not indexing).
    KeepOut,
    /// No rule: not checked.
    #[default]
    None,
}

impl IndexStance {
    fn is_none(&self) -> bool {
        *self == IndexStance::None
    }
}

/// A path pattern: `*` matches any run of characters, `/` included. Without a `?` it is matched
/// against the path; with one, against the path and the query (parameters sorted, as the crawler
/// normalises them): `/collections/*?page=*`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PathPattern(pub String);

impl PathPattern {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `path_and_query` (`/a/b?x=1`) matches.
    pub fn matches(&self, path_and_query: &str) -> bool {
        let subject = if self.0.contains('?') {
            path_and_query
        } else {
            path_and_query.split('?').next().unwrap_or_default()
        };
        glob(self.0.as_bytes(), subject.as_bytes())
    }
}

/// `*`-only glob matching, iterative with one backtrack point.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

/// Check thresholds; `None` is the built-in default ([`Thresholds::effective`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thresholds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_max: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_min: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_max: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_min: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thin_words: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slow_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redirect_hops: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_alt: Option<bool>,
}

/// [`Thresholds`] with every default filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EffectiveThresholds {
    pub title_max: u16,
    pub title_min: u16,
    pub description_max: u16,
    pub description_min: u16,
    pub thin_words: u32,
    pub slow_ms: u32,
    pub max_redirect_hops: u8,
    pub image_alt: bool,
}

/// The built-in thresholds, as the checks use them today.
pub const DEFAULT_THRESHOLDS: EffectiveThresholds = EffectiveThresholds {
    title_max: 60,
    title_min: 30,
    description_max: 160,
    description_min: 70,
    thin_words: 200,
    slow_ms: 1000,
    max_redirect_hops: 1,
    image_alt: true,
};

impl Thresholds {
    fn is_default(&self) -> bool {
        *self == Thresholds::default()
    }

    pub fn effective(&self) -> EffectiveThresholds {
        let d = DEFAULT_THRESHOLDS;
        EffectiveThresholds {
            title_max: self.title_max.unwrap_or(d.title_max),
            title_min: self.title_min.unwrap_or(d.title_min),
            description_max: self.description_max.unwrap_or(d.description_max),
            description_min: self.description_min.unwrap_or(d.description_min),
            thin_words: self.thin_words.unwrap_or(d.thin_words),
            slow_ms: self.slow_ms.unwrap_or(d.slow_ms),
            max_redirect_hops: self.max_redirect_hops.unwrap_or(d.max_redirect_hops),
            image_alt: self.image_alt.unwrap_or(d.image_alt),
        }
    }
}

/// A check that doesn't apply to part of the site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exception {
    pub check: CheckId,
    pub scope: PathPattern,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

// ---- validation ------------------------------------------------------------------------------

/// One reason a policy is refused: where (`seo.index[2].pattern`) and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyError {
    pub path: String,
    pub message: String,
}

impl PolicyError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> PolicyError {
        PolicyError {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// The errors as one line each.
pub fn error_lines(errors: &[PolicyError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn sane_text(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= MAX_TEXT_LEN && !s.chars().any(char::is_control)
}

fn check_pattern(errors: &mut Vec<PolicyError>, path: String, pattern: &str) {
    if !pattern.starts_with('/') {
        errors.push(PolicyError::new(path, "a pattern starts with /"));
    } else if !sane_text(pattern) || pattern.contains(char::is_whitespace) {
        errors.push(PolicyError::new(
            path,
            format!("\"{pattern}\" is not a path pattern"),
        ));
    }
}

/// Checks the AI rules: sane sizes, bot names that are names, and no accepted `noindex` (it never
/// opens an AI answers incident: it is the SEO checks' business).
pub fn validate_ai(ai: &AiRules) -> Result<(), Vec<PolicyError>> {
    let mut errors = Vec::new();
    let intent = &ai.intent;
    if intent.bots.len() > MAX_BOT_OVERRIDES {
        errors.push(PolicyError::new(
            "ai.bots",
            format!("At most {MAX_BOT_OVERRIDES} bots can be set."),
        ));
    }
    for token in intent.bots.keys() {
        let t = token.trim();
        if t.is_empty() || t.len() > MAX_TOKEN_LEN || t.chars().any(char::is_control) {
            errors.push(PolicyError::new(
                "ai.bots",
                format!("\"{token}\" is not a bot name."),
            ));
        }
    }
    if intent.accepted_directives.contains(&DirectiveSlug::Noindex) {
        errors.push(PolicyError::new(
            "ai.accepted_directives",
            "noindex can't be accepted here: the SEO checks report it.",
        ));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

impl SitePolicy {
    /// Every reason the policy can't be saved, or `Ok`.
    pub fn validate(&self) -> Result<(), Vec<PolicyError>> {
        let mut errors = Vec::new();
        if self.version != POLICY_VERSION {
            errors.push(PolicyError::new(
                "version",
                format!(
                    "version {} is not supported (expected {POLICY_VERSION})",
                    self.version
                ),
            ));
        }
        self.seo.validate_into(&mut errors);
        if let Err(e) = validate_ai(&self.ai) {
            errors.extend(e);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Reads a stored policy leniently, for forward compatibility: a section that doesn't parse
    /// (a value written by a newer release) reads as its defaults instead of failing the whole.
    pub fn from_json_lenient(value: serde_json::Value) -> SitePolicy {
        if let Ok(p) = serde_json::from_value::<SitePolicy>(value.clone()) {
            return p;
        }
        let part = |key: &str| value.get(key).cloned();
        SitePolicy {
            version: part("version")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or(POLICY_VERSION),
            seo: part("seo")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            ai: part("ai")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
        }
    }

    /// As stored: JSON.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("a policy always serializes")
    }

    /// As a `codoseo.toml` file: the `[policy]` table.
    pub fn to_toml(&self) -> String {
        #[derive(Serialize)]
        struct File<'a> {
            policy: &'a SitePolicy,
        }
        toml::to_string_pretty(&File { policy: self }).expect("a policy always serializes")
    }

    /// The policy as one AI-only view: the intent the findings are drawn under.
    pub fn intent(&self) -> &Intent {
        &self.ai.intent
    }
}

impl SeoRules {
    fn validate_into(&self, errors: &mut Vec<PolicyError>) {
        if self.index.len() > MAX_INDEX_RULES {
            errors.push(PolicyError::new(
                "seo.index",
                format!("at most {MAX_INDEX_RULES} index rules"),
            ));
        }
        let mut seen = BTreeSet::new();
        for (i, rule) in self.index.iter().enumerate() {
            let path = format!("seo.index[{i}].pattern");
            check_pattern(errors, path.clone(), rule.pattern.as_str());
            if !seen.insert(rule.pattern.as_str()) {
                errors.push(PolicyError::new(
                    path,
                    format!("\"{}\" is listed twice", rule.pattern.as_str()),
                ));
            }
        }
        if self.ignore_params.len() > MAX_IGNORE_PARAMS {
            errors.push(PolicyError::new(
                "seo.ignore_params",
                format!("at most {MAX_IGNORE_PARAMS} parameters"),
            ));
        }
        let mut params = BTreeSet::new();
        for (i, p) in self.ignore_params.iter().enumerate() {
            let path = format!("seo.ignore_params[{i}]");
            if !sane_text(p) || p.contains(['=', '&', '?', ' ']) {
                errors.push(PolicyError::new(
                    path,
                    format!("\"{p}\" is not a parameter name"),
                ));
            } else if !params.insert(p.as_str()) {
                errors.push(PolicyError::new(path, format!("\"{p}\" is listed twice")));
            }
        }
        if let Some(p) = &self.pagination_param {
            if !sane_text(p) || p.contains(['=', '&', '?', ' ', '*']) {
                errors.push(PolicyError::new(
                    "seo.pagination_param",
                    format!("\"{p}\" is not a parameter name"),
                ));
            } else if params.contains(p.as_str()) {
                errors.push(PolicyError::new(
                    "seo.pagination_param",
                    format!("\"{p}\" is also an ignored parameter"),
                ));
            }
        }
        if self.must_work.len() > MAX_MUST_WORK {
            errors.push(PolicyError::new(
                "seo.must_work",
                format!("at most {MAX_MUST_WORK} pages"),
            ));
        }
        let mut must = BTreeSet::new();
        for (i, p) in self.must_work.iter().enumerate() {
            let path = format!("seo.must_work[{i}]");
            if !p.starts_with('/') || !sane_text(p) || p.contains(char::is_whitespace) {
                errors.push(PolicyError::new(path, format!("\"{p}\" is not a path")));
            } else if p.contains('*') {
                errors.push(PolicyError::new(path, "a must-work page is one path, no *"));
            } else if !must.insert(p.as_str()) {
                errors.push(PolicyError::new(path, format!("\"{p}\" is listed twice")));
            }
        }
        self.thresholds.validate_into(errors);
        if self.exceptions.len() > MAX_EXCEPTIONS {
            errors.push(PolicyError::new(
                "seo.exceptions",
                format!("at most {MAX_EXCEPTIONS} exceptions"),
            ));
        }
        let mut exc = BTreeSet::new();
        for (i, e) in self.exceptions.iter().enumerate() {
            check_pattern(
                errors,
                format!("seo.exceptions[{i}].scope"),
                e.scope.as_str(),
            );
            if e.note.len() > MAX_TEXT_LEN || e.note.chars().any(char::is_control) {
                errors.push(PolicyError::new(
                    format!("seo.exceptions[{i}].note"),
                    format!("a note is at most {MAX_TEXT_LEN} characters on one line"),
                ));
            }
            if !exc.insert((e.check, e.scope.as_str())) {
                errors.push(PolicyError::new(
                    format!("seo.exceptions[{i}]"),
                    "the same check and scope are listed twice",
                ));
            }
        }
    }
}

impl Thresholds {
    fn validate_into(&self, errors: &mut Vec<PolicyError>) {
        fn range<T: PartialOrd + Copy + std::fmt::Display>(
            errors: &mut Vec<PolicyError>,
            key: &str,
            value: Option<T>,
            lo: T,
            hi: T,
        ) {
            if let Some(v) = value
                && (v < lo || v > hi)
            {
                errors.push(PolicyError::new(
                    format!("seo.thresholds.{key}"),
                    format!("{v} is out of range ({lo} to {hi})"),
                ));
            }
        }
        range(errors, "title_max", self.title_max, 10, 300);
        range(errors, "title_min", self.title_min, 1, 300);
        range(errors, "description_max", self.description_max, 10, 1000);
        range(errors, "description_min", self.description_min, 1, 1000);
        range(errors, "thin_words", self.thin_words, 1, 10_000);
        range(errors, "slow_ms", self.slow_ms, 100, 60_000);
        range(errors, "max_redirect_hops", self.max_redirect_hops, 1, 10);
        let e = self.effective();
        if e.title_min >= e.title_max {
            errors.push(PolicyError::new(
                "seo.thresholds.title_min",
                format!(
                    "title_min ({}) must be below title_max ({})",
                    e.title_min, e.title_max
                ),
            ));
        }
        if e.description_min >= e.description_max {
            errors.push(PolicyError::new(
                "seo.thresholds.description_min",
                format!(
                    "description_min ({}) must be below description_max ({})",
                    e.description_min, e.description_max
                ),
            ));
        }
    }
}

// ---- the policy file -------------------------------------------------------------------------

/// A `codoseo.toml` read: the policy, and the keys it has that the policy doesn't know (as
/// `policy.seo.tresholds`), which are ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFile {
    pub policy: SitePolicy,
    pub warnings: Vec<String>,
}

/// Reads a `codoseo.toml`: its `[policy]` table, validated. A syntax or type error is one
/// [`PolicyError`] with the parser's message (line and column included); unknown keys are
/// warnings, not errors, so an older release reads a newer file.
pub fn parse_file(text: &str) -> Result<ParsedFile, Vec<PolicyError>> {
    #[derive(Deserialize)]
    struct File {
        policy: Option<SitePolicy>,
    }
    let de = toml::Deserializer::parse(text)
        .map_err(|e| vec![PolicyError::new("", e.to_string().trim_end())])?;
    let mut warnings = Vec::new();
    let file: File = serde_ignored::deserialize(de, |path| {
        // `?` is serde_ignored's step into an `Option`.
        let path = path.to_string().replace(".?", "");
        // Other tables of codoseo.toml belong to other features.
        if path.starts_with("policy.") {
            warnings.push(path);
        }
    })
    .map_err(|e| vec![PolicyError::new("", e.to_string().trim_end())])?;
    let policy = file.policy.ok_or_else(|| {
        vec![PolicyError::new(
            "",
            "no [policy] table: a policy file starts with [policy]",
        )]
    })?;
    policy.validate()?;
    Ok(ParsedFile { policy, warnings })
}

/// A policy file read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedFile {
    pub path: std::path::PathBuf,
    pub policy: SitePolicy,
    /// Unknown keys, ignored ([`ParsedFile::warnings`]).
    pub warnings: Vec<String>,
    /// [`file_hash`] of its bytes.
    pub hash: String,
}

/// Reads and validates the policy file at `path`. The error is what to tell the user: the
/// file's name, then one line per problem.
pub fn load_file(path: &std::path::Path) -> Result<LoadedFile, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let text =
        std::str::from_utf8(&bytes).map_err(|_| format!("{} is not UTF-8 text", path.display()))?;
    let parsed = parse_file(text).map_err(|errors| {
        format!(
            "{} is not a valid policy:\n{}",
            path.display(),
            error_lines(&errors)
        )
    })?;
    Ok(LoadedFile {
        path: path.to_owned(),
        policy: parsed.policy,
        warnings: parsed.warnings,
        hash: file_hash(&bytes),
    })
}

impl LoadedFile {
    /// What a saved audit keeps about the policy it was judged under: `{ "file", "hash",
    /// "policy", "report" }`.
    pub fn audit_section(&self, report: &PolicyReport) -> serde_json::Value {
        serde_json::json!({
            "file": self.path.display().to_string(),
            "hash": self.hash,
            "policy": self.policy,
            "report": report,
        })
    }
}

/// A short, stable fingerprint of a policy file's bytes: `sha256:` and the first 16 hex digits.
pub fn file_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_any_run_including_slashes() {
        let p = PathPattern("/checkout/*".into());
        assert!(p.matches("/checkout/cart"));
        assert!(p.matches("/checkout/a/b?x=1"));
        assert!(!p.matches("/checkout"));
        assert!(PathPattern("/search*".into()).matches("/search"));
        assert!(PathPattern("/search*".into()).matches("/search/x"));
        let q = PathPattern("/collections/*?page=*".into());
        assert!(q.matches("/collections/winter?page=2"));
        assert!(!q.matches("/collections/winter"));
        assert!(PathPattern("/a*b*c".into()).matches("/aXbYbZc"));
    }

    #[test]
    fn default_thresholds_are_the_checks_numbers() {
        let e = Thresholds::default().effective();
        assert_eq!((e.title_min, e.title_max), (30, 60));
        assert_eq!((e.description_min, e.description_max), (70, 160));
        assert_eq!(
            (e.thin_words, e.slow_ms, e.max_redirect_hops),
            (200, 1000, 1)
        );
        let t = Thresholds {
            title_max: Some(65),
            ..Thresholds::default()
        };
        assert_eq!(t.effective().title_max, 65);
    }
}
