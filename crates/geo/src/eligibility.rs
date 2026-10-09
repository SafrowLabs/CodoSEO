//! Whether one page can be shown in an engine's AI answers, from the page-level controls that
//! engine documents (robots meta, `X-Robots-Tag`, `data-nosnippet`).
//!
//! Only controls the operator documents count, and only the ones that engine honours: the same
//! `noarchive` removes a page from Copilot's answers but does nothing to Google's. Engines with
//! no documented page-level control are always [`Effect::Eligible`] here; whether their crawler
//! may fetch at all is a robots.txt question, answered elsewhere.

use codoseo_core::page::{PageFields, PageRecord, directives_for};
use serde::{Deserialize, Serialize};

/// Share of a page's words under `data-nosnippet` at which an engine counts the page as limited.
pub const NOSNIPPET_SHARE_PERCENT: u64 = 25;

/// Shown with the engines that document no page-level control.
pub const NO_PAGE_CONTROLS_NOTE: &str = "no documented page-level controls";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineId {
    Google,
    Bing,
    Apple,
    Amazon,
    ChatGpt,
    Claude,
    Perplexity,
    MetaAi,
    DuckDuckGo,
    Mistral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Engine {
    pub id: EngineId,
    pub name: &'static str,
    /// The registry token of the crawler whose robots.txt group governs this engine.
    pub crawler: &'static str,
    /// Lower-case names whose `agent:` prefix, or `<meta name>`, addresses this engine.
    pub meta_scopes: &'static [&'static str],
    /// The operator documents at least one page-level control.
    pub page_controls: bool,
    /// The operator's documentation of those controls (or of the crawler, when there are none).
    pub source_url: &'static str,
}

impl Engine {
    /// Why the page effect is always [`Effect::Eligible`], for engines without page controls.
    pub fn note(&self) -> Option<&'static str> {
        (!self.page_controls).then_some(NO_PAGE_CONTROLS_NOTE)
    }
}

static ENGINES: [Engine; 10] = [
    Engine {
        id: EngineId::Google,
        name: "Google Search, AI Overviews & AI Mode",
        crawler: "Googlebot",
        meta_scopes: &["robots", "googlebot"],
        page_controls: true,
        source_url: "https://developers.google.com/search/docs/appearance/ai-features",
    },
    Engine {
        id: EngineId::Bing,
        name: "Bing & Microsoft Copilot",
        crawler: "Bingbot",
        meta_scopes: &["robots", "bingbot", "msnbot"],
        page_controls: true,
        source_url: "https://blogs.bing.com/webmaster/september-2023/Announcing-new-options-for-webmasters-to-control-usage-of-their-content-in-Bing-Chat",
    },
    Engine {
        id: EngineId::Apple,
        name: "Apple (Siri, Spotlight, Safari)",
        crawler: "Applebot",
        meta_scopes: &["robots", "applebot"],
        page_controls: true,
        source_url: "https://support.apple.com/en-us/119829",
    },
    Engine {
        id: EngineId::Amazon,
        name: "Amazon search (Alexa)",
        crawler: "Amzn-SearchBot",
        meta_scopes: &["robots", "amzn-searchbot"],
        page_controls: true,
        source_url: "https://developer.amazon.com/amazonbot",
    },
    Engine {
        id: EngineId::ChatGpt,
        name: "ChatGPT search",
        crawler: "OAI-SearchBot",
        meta_scopes: &["robots", "oai-searchbot"],
        page_controls: false,
        source_url: "https://developers.openai.com/api/docs/bots",
    },
    Engine {
        id: EngineId::Claude,
        name: "Claude",
        crawler: "Claude-SearchBot",
        meta_scopes: &["robots", "claude-searchbot"],
        page_controls: false,
        source_url: "https://support.claude.com/en/articles/8896518-does-anthropic-crawl-data-from-the-web-and-how-can-site-owners-block-the-crawler",
    },
    Engine {
        id: EngineId::Perplexity,
        name: "Perplexity",
        crawler: "PerplexityBot",
        meta_scopes: &["robots", "perplexitybot"],
        page_controls: false,
        source_url: "https://docs.perplexity.ai/guides/bots",
    },
    Engine {
        id: EngineId::MetaAi,
        name: "Meta AI",
        crawler: "meta-webindexer",
        meta_scopes: &["robots", "meta-webindexer"],
        page_controls: false,
        source_url: "https://developers.facebook.com/docs/sharing/webmasters/web-crawlers",
    },
    Engine {
        id: EngineId::DuckDuckGo,
        name: "DuckDuckGo AI answers",
        crawler: "DuckAssistBot",
        meta_scopes: &["robots", "duckassistbot"],
        page_controls: false,
        source_url: "https://duckduckgo.com/duckduckgo-help-pages/results/duckassistbot",
    },
    Engine {
        id: EngineId::Mistral,
        name: "Mistral Le Chat",
        crawler: "MistralAI-Index",
        meta_scopes: &["robots", "mistralai-index"],
        page_controls: false,
        source_url: "https://docs.mistral.ai/robots",
    },
];

pub fn engines() -> &'static [Engine] {
    &ENGINES
}

/// What the page's own controls do to its place in an engine's answers. `Excluded` beats
/// `Limited` beats `Eligible`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Eligible,
    Limited,
    Excluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectiveSlug {
    Noindex,
    Nosnippet,
    MaxSnippet,
    Noarchive,
    Nocache,
    DataNosnippet,
}

/// Where a directive was found.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CauseSource {
    /// `<meta name="…">`, `robots` or a bot name.
    Meta { name: String },
    /// `X-Robots-Tag`; `scope` is the `agent:` prefix, or `all` when there is none.
    Header { scope: String },
    /// A `data-nosnippet` attribute.
    Attribute,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cause {
    pub directive: DirectiveSlug,
    /// The directive as written (`max-snippet:50`, `none`) or a measure (`62% of words`).
    pub detail: String,
    pub source: CauseSource,
    /// What this directive alone does to the page.
    pub effect: Effect,
}

/// What an engine honours: directive words per effect, and the two parameterised controls.
struct Rules {
    excluded: &'static [&'static str],
    limited: &'static [&'static str],
    /// `max-snippet:0` excludes, `max-snippet:N` limits (Google).
    max_snippet: bool,
    /// `data-nosnippet` over [`NOSNIPPET_SHARE_PERCENT`] of the words limits.
    data_nosnippet: bool,
}

fn rules(id: EngineId) -> Rules {
    match id {
        // https://developers.google.com/search/docs/appearance/ai-features names `nosnippet`,
        // `data-nosnippet`, `max-snippet` and `noindex` as the controls over what Search shows
        // from a page, AI features included; it doesn't spell out the effect per feature. The
        // directive meanings are from
        // https://developers.google.com/search/docs/crawling-indexing/robots-meta-tag:
        // `none` = `noindex, nofollow`, `max-snippet:0` = no snippet, `noarchive` is ignored
        // (so it isn't listed), and `data-nosnippet` is valid on `span`, `div` and `section`.
        // Treating no-snippet as Excluded is our reading: with no text allowed there is nothing
        // to quote.
        EngineId::Google => Rules {
            excluded: &["noindex", "none", "nosnippet"],
            limited: &[],
            max_snippet: true,
            data_nosnippet: true,
        },
        // Bing's announcement (the engine's source_url): NOARCHIVE keeps the page out of Bing
        // Chat answers (it stays in search results), NOCACHE allows only URL, title and snippet,
        // both together count as NOCACHE. It says nothing about Copilot by name, nor about
        // `nosnippet` / `data-nosnippet`, which come from Bing's robots meta tag documentation;
        // those are our reading of "snippet only".
        EngineId::Bing => Rules {
            excluded: &["noindex", "none", "noarchive"],
            limited: &["nocache", "nosnippet"],
            max_snippet: false,
            data_nosnippet: true,
        },
        // https://support.apple.com/en-us/119829 documents noindex, nosnippet, nofollow, none and
        // all, under `robots` or `applebot`; it doesn't mention noarchive.
        EngineId::Apple => Rules {
            excluded: &["noindex", "none"],
            limited: &["nosnippet"],
            max_snippet: false,
            data_nosnippet: false,
        },
        // https://developer.amazon.com/amazonbot: noindex and none mean "do not index"; noarchive
        // means "do not use the page for model training", which says nothing about search
        // answers, so it isn't a control here. The page doesn't name the meta `name`.
        EngineId::Amazon => Rules {
            excluded: &["noindex", "none"],
            limited: &[],
            max_snippet: false,
            data_nosnippet: false,
        },
        _ => Rules {
            excluded: &[],
            limited: &[],
            max_snippet: false,
            data_nosnippet: false,
        },
    }
}

fn slug_of(directive: &str) -> Option<DirectiveSlug> {
    Some(match directive {
        "noindex" | "none" => DirectiveSlug::Noindex,
        "nosnippet" => DirectiveSlug::Nosnippet,
        "noarchive" => DirectiveSlug::Noarchive,
        "nocache" => DirectiveSlug::Nocache,
        _ => return None,
    })
}

/// The effect of a page's own markup on one engine, and every directive that causes it.
/// `Eligible` comes back with no causes. A page that can't be read as HTML has no answer
/// eligibility to speak of: use [`record_effect`] for stored records.
pub fn page_effect(engine: &Engine, fields: &PageFields) -> (Effect, Vec<Cause>) {
    if !engine.page_controls {
        return (Effect::Eligible, Vec::new());
    }
    let rules = rules(engine.id);
    let mut causes: Vec<Cause> = Vec::new();
    let mut add = |cause: Cause| {
        if !causes.contains(&cause) {
            causes.push(cause);
        }
    };

    let mut consider = |directive: &str, source: &CauseSource| {
        if let Some(effect) = word_effect(&rules, directive)
            && let Some(slug) = slug_of(directive)
        {
            add(Cause {
                directive: slug,
                detail: directive.to_owned(),
                source: source.clone(),
                effect,
            });
        } else if rules.max_snippet
            && let Some(limit) = directive.strip_prefix("max-snippet:")
            && let Ok(n) = limit.parse::<i64>()
            && n >= 0
        {
            add(Cause {
                directive: DirectiveSlug::MaxSnippet,
                detail: directive.to_owned(),
                source: source.clone(),
                effect: if n == 0 {
                    Effect::Excluded
                } else {
                    Effect::Limited
                },
            });
        }
    };

    let scopes = engine.meta_scopes;
    if let Some(value) = fields.meta_robots.as_deref() {
        let source = CauseSource::Meta {
            name: "robots".to_owned(),
        };
        for d in directives_for(Some(value), scopes) {
            consider(&d, &source);
        }
    }
    if let Some(value) = fields.x_robots_tag.as_deref() {
        // Unprefixed directives apply to every bot; a prefix names the scope that addressed us.
        let unscoped = directives_for(Some(value), &[]);
        for d in directives_for(Some(value), scopes) {
            let scope = if unscoped.contains(&d) {
                "all".to_owned()
            } else {
                scopes
                    .iter()
                    .find(|s| directives_for(Some(value), &[s]).contains(&d))
                    .map_or_else(|| "all".to_owned(), |s| (*s).to_owned())
            };
            consider(&d, &CauseSource::Header { scope });
        }
    }
    for (name, content) in &fields.ai.bot_meta {
        if !scopes.contains(&name.as_str()) {
            continue;
        }
        let source = CauseSource::Meta { name: name.clone() };
        for d in directives_for(Some(content), scopes) {
            consider(&d, &source);
        }
    }

    if rules.data_nosnippet {
        let hidden = u64::from(fields.ai.nosnippet_words);
        let total = u64::from(fields.word_count).max(hidden);
        if hidden > 0 && hidden * 100 >= NOSNIPPET_SHARE_PERCENT * total {
            add(Cause {
                directive: DirectiveSlug::DataNosnippet,
                detail: format!("{}% of words", hidden * 100 / total),
                source: CauseSource::Attribute,
                effect: Effect::Limited,
            });
        }
    }

    let effect = causes
        .iter()
        .map(|c| c.effect)
        .max()
        .unwrap_or(Effect::Eligible);
    (effect, causes)
}

fn word_effect(rules: &Rules, directive: &str) -> Option<Effect> {
    if rules.excluded.contains(&directive) {
        Some(Effect::Excluded)
    } else if rules.limited.contains(&directive) {
        Some(Effect::Limited)
    } else {
        None
    }
}

/// [`page_effect`] for a stored record; `None` when the page isn't a successful HTML response,
/// where the question doesn't apply.
pub fn record_effect(engine: &Engine, record: &PageRecord) -> Option<(Effect, Vec<Cause>)> {
    record
        .is_html_ok()
        .then(|| page_effect(engine, &record.fields))
}
