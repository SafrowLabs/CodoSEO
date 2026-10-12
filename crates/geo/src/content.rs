//! Whether an important page's content is in its HTML as served, or only appears once its
//! JavaScript runs.
//!
//! Most AI crawlers fetch the HTML and read it as it is: they don't run scripts, so a client-side
//! app's empty shell is all they get. [`classify`] reads the raw-HTML signals the extractor keeps
//! ([`RawSignals`]) and never claims what a page looks like after rendering:
//!
//! - [`ContentVerdict::Shell`]: under [`SHELL_MAX_WORDS`] words and the app's mount element is
//!   empty, or a `<noscript>` notice asks for JavaScript.
//! - [`ContentVerdict::LikelyShell`]: under [`SHELL_MAX_WORDS`] words, no H1, no internal links,
//!   and scripts make up the page: inline script is at least [`SCRIPT_SHARE_PERCENT`] of its bytes,
//!   or it loads at least [`MIN_EXTERNAL_SCRIPTS`] external scripts. Script share alone never flags
//!   a page: a server-rendered Next.js page carries a big `__NEXT_DATA__` and its text too.
//! - [`ContentVerdict::Thin`]: few words but no shell signal (a contact or coming-soon page).
//!   Informational, never a finding.
//! - [`ContentVerdict::Present`]: enough text in the HTML as served.

use std::collections::HashMap;

use codoseo_core::page::{JsonLdStatus, MountPoint, PageRecord};
use serde::{Deserialize, Serialize};

use crate::report::ImportantUrl;

/// A page with fewer words than this in its HTML as served is thin, or a shell when a shell
/// signal is present.
pub const SHELL_MAX_WORDS: u32 = 50;
/// Inline script share of the page's bytes, in percent, from which a short page with no H1 and
/// no internal links is likely a shell.
pub const SCRIPT_SHARE_PERCENT: u8 = 60;
/// External scripts from which a short page with no H1 and no internal links is likely a shell.
pub const MIN_EXTERNAL_SCRIPTS: u16 = 3;

/// AI crawlers that read the HTML as served without running its JavaScript, as measured by
/// Vercel and MERJ ([`NON_RENDERING_SOURCE`]) for OpenAI, Anthropic and Perplexity; the
/// search and user-triggered fetchers of the same operators are listed with them.
pub const NON_RENDERING_BOTS: [&str; 8] = [
    "GPTBot",
    "OAI-SearchBot",
    "ChatGPT-User",
    "ClaudeBot",
    "Claude-SearchBot",
    "Claude-User",
    "PerplexityBot",
    "Perplexity-User",
];
/// Vercel and MERJ, "The rise of the AI crawler" (2024): none of the major AI crawlers measured
/// render JavaScript.
pub const NON_RENDERING_SOURCE: &str = "https://vercel.com/blog/the-rise-of-the-ai-crawler";

/// What the HTML as served holds, before any JavaScript runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentVerdict {
    Present,
    Thin,
    LikelyShell,
    Shell,
}

impl ContentVerdict {
    /// An empty app shell, certain or likely.
    pub fn is_shell(self) -> bool {
        matches!(self, ContentVerdict::Shell | ContentVerdict::LikelyShell)
    }
}

/// A raw-HTML marker of a client-side app shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellSignal {
    /// The app's mount element holds nothing.
    MountEmpty,
    /// A `<noscript>` notice asks for JavaScript.
    NoscriptJs,
    /// Inline script is most of the page's bytes.
    ScriptHeavy,
    /// The page loads several external scripts.
    ManyScripts,
}

/// The verdict for one page. Pages that aren't a successful HTML response are
/// [`ContentVerdict::Present`]: there is no app shell to speak of.
pub fn classify(page: &PageRecord) -> ContentVerdict {
    if !page.is_html_ok() || page.fields.word_count >= SHELL_MAX_WORDS {
        return ContentVerdict::Present;
    }
    let raw = &page.fields.raw;
    if raw.mount_empty || raw.noscript_js {
        return ContentVerdict::Shell;
    }
    let bare = page.fields.h1.is_empty() && page.outlinks_internal == 0;
    let scripted =
        script_share(page) >= SCRIPT_SHARE_PERCENT || raw.scripts_external >= MIN_EXTERNAL_SCRIPTS;
    if bare && scripted {
        ContentVerdict::LikelyShell
    } else {
        ContentVerdict::Thin
    }
}

/// The shell markers `page` carries, whatever its verdict.
pub fn shell_signals(page: &PageRecord) -> Vec<ShellSignal> {
    let raw = &page.fields.raw;
    let mut out = Vec::new();
    if raw.mount_empty {
        out.push(ShellSignal::MountEmpty);
    }
    if raw.noscript_js {
        out.push(ShellSignal::NoscriptJs);
    }
    if script_share(page) >= SCRIPT_SHARE_PERCENT {
        out.push(ShellSignal::ScriptHeavy);
    }
    if raw.scripts_external >= MIN_EXTERNAL_SCRIPTS {
        out.push(ShellSignal::ManyScripts);
    }
    out
}

/// Inline script bytes as a percentage of the page's size, at most 100; 0 without a size.
pub fn script_share(page: &PageRecord) -> u8 {
    if page.size_bytes == 0 {
        return 0;
    }
    let share = u64::from(page.fields.raw.script_bytes) * 100 / page.size_bytes;
    share.min(100) as u8
}

/// What one important HTML page holds as served. Kept small for the stored report: false flags,
/// zero counts and an absent JSON-LD are left out of the JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawContent {
    /// Index into `AccessReport::important`.
    pub url: u16,
    pub verdict: ContentVerdict,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<ShellSignal>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub words: u32,
    /// Words inside `main`, `article` and `[role=main]`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub main_words: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub title: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub h1: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub description: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub canonical: bool,
    /// A meta robots tag is present.
    #[serde(default, skip_serializing_if = "is_false")]
    pub robots: bool,
    #[serde(default, skip_serializing_if = "is_absent")]
    pub jsonld: JsonLdStatus,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub links_internal: u32,
    /// Inline script bytes as a percentage of the page's bytes.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub script_share: u8,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub scripts_external: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<MountPoint>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mount_empty: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub noscript_js: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero<T: Default + PartialEq>(n: &T) -> bool {
    *n == T::default()
}

fn is_absent(j: &JsonLdStatus) -> bool {
    *j == JsonLdStatus::Absent
}

/// The raw content of the report's important HTML pages, in the report's order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentReport {
    pub pages: Vec<RawContent>,
}

impl ContentReport {
    /// The pages that are, or are likely, an empty app shell.
    pub fn shells(&self) -> impl Iterator<Item = &RawContent> {
        self.pages.iter().filter(|p| p.verdict.is_shell())
    }
}

/// The facts for one page; `None` unless it is a successful HTML response.
pub fn raw_content(url: u16, page: &PageRecord) -> Option<RawContent> {
    if !page.is_html_ok() {
        return None;
    }
    let f = &page.fields;
    Some(RawContent {
        url,
        verdict: classify(page),
        signals: shell_signals(page),
        words: f.word_count,
        main_words: f.raw.main_words,
        title: f.title.is_some(),
        h1: !f.h1.is_empty(),
        description: f.meta_description.is_some(),
        canonical: f.canonical.is_some(),
        robots: f.meta_robots.is_some(),
        jsonld: f.jsonld,
        links_internal: page.outlinks_internal,
        script_share: script_share(page),
        scripts_external: f.raw.scripts_external,
        mount: f.raw.mount,
        mount_empty: f.raw.mount_empty,
        noscript_js: f.raw.noscript_js,
    })
}

/// The content report over the `important` pages; `None` when none of them is an HTML page.
pub fn content_report(
    important: &[ImportantUrl],
    by_url: &HashMap<&str, &PageRecord>,
) -> Option<ContentReport> {
    let pages: Vec<RawContent> = important
        .iter()
        .enumerate()
        .filter_map(|(i, imp)| raw_content(i as u16, by_url.get(imp.url.as_str())?))
        .collect();
    (!pages.is_empty()).then_some(ContentReport { pages })
}
