//! Pulls SEO fields out of an HTML page as it streams in, without building a DOM.
//!
//! Encoding: a BOM wins, then the `Content-Type` charset, then `<meta charset>`
//! (which lol_html switches to mid-stream), then UTF-8. Encodings that aren't
//! ASCII-compatible (UTF-16) are transcoded to UTF-8 before parsing.
//!
//! Text comes from one document-wide handler and is routed by state that the
//! element handlers keep (inside `<title>`, a heading, a link, a skipped element).
//! Malformed input never panics: a parser error just ends extraction early.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::OnceLock;

use codoseo_core::page::{AiMeta, JsonLdStatus, OgTags, PageFields};
use codoseo_core::url::normalize;
use encoding_rs::{Decoder, Encoding, UTF_8, UTF_16BE, UTF_16LE};
use lol_html::html_content::{Element, EndTag};
use lol_html::{AsciiCompatibleEncoding, HandlerResult, HtmlRewriter, Settings, doc_text, element};
use serde::Serialize;
use url::Url;
use xxhash_rust::xxh3::Xxh3;

const MAX_TITLE_CHARS: usize = 1_000;
const MAX_HEADING_CHARS: usize = 300;
const MAX_ANCHOR_CHARS: usize = 200;
const MAX_HEADINGS: usize = 50;
const MAX_LINKS: usize = 5_000;
/// Bot-named robots metas kept per page.
const MAX_BOT_META: usize = 16;
/// Robots-style meta names read besides every registry token (lower-cased).
const BOT_META_NAMES: [&str; 8] = [
    "googlebot",
    "googlebot-news",
    "bingbot",
    "msnbot",
    "applebot",
    "amazonbot",
    "amzn-searchbot",
    "codoseobot",
];
/// JSON-LD blocks bigger than this are reported as too large instead of parsed.
const MAX_JSONLD_BYTES: usize = 1024 * 1024;
/// How far to look for `<meta charset>` before parsing, as browsers do.
const PRESCAN_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Link {
    pub url: Url,
    pub anchor: String,
    pub nofollow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Extracted {
    pub fields: PageFields,
    pub links: Vec<Link>,
}

/// Extracts everything from a complete body in one call.
pub fn extract(page_url: &Url, content_type: Option<&str>, body: &[u8]) -> Extracted {
    let mut e = Extractor::new(page_url, content_type);
    e.write(body);
    e.finish()
}

type Rewriter = HtmlRewriter<'static, fn(&[u8])>;

enum Stage {
    /// Holding the first bytes until we can check for a BOM.
    Sniffing(Vec<u8>),
    Direct(Rewriter),
    Transcoding(Decoder, Rewriter),
    Stopped,
}

pub struct Extractor {
    page_url: Url,
    header_encoding: Option<&'static Encoding>,
    state: Rc<RefCell<State>>,
    stage: Stage,
}

impl Extractor {
    pub fn new(page_url: &Url, content_type: Option<&str>) -> Extractor {
        Extractor {
            page_url: page_url.clone(),
            header_encoding: content_type.and_then(charset_from_content_type),
            state: Rc::new(RefCell::new(State::new(page_url.scheme() == "https"))),
            stage: Stage::Sniffing(Vec::new()),
        }
    }

    pub fn write(&mut self, chunk: &[u8]) {
        if let Stage::Sniffing(pending) = &mut self.stage {
            pending.extend_from_slice(chunk);
            if pending.len() >= PRESCAN_BYTES {
                let first = std::mem::take(pending);
                self.begin(&first);
            }
            return;
        }
        self.feed(chunk);
    }

    pub fn finish(mut self) -> Extracted {
        if let Stage::Sniffing(pending) = &mut self.stage {
            let first = std::mem::take(pending);
            self.begin(&first);
        }
        match std::mem::replace(&mut self.stage, Stage::Stopped) {
            Stage::Direct(rw) => {
                let _ = rw.end();
            }
            Stage::Transcoding(mut decoder, mut rw) => {
                let tail = decode(&mut decoder, &[], true);
                if rw.write(tail.as_bytes()).is_ok() {
                    let _ = rw.end();
                }
            }
            Stage::Sniffing(_) | Stage::Stopped => {}
        }
        let mut state = self.state.borrow_mut();
        state.close_open_elements();
        state.build(&self.page_url)
    }

    fn begin(&mut self, first: &[u8]) {
        let (encoding, skip, adjust_on_meta) = if first.starts_with(&[0xEF, 0xBB, 0xBF]) {
            (UTF_8, 3, false)
        } else if first.starts_with(&[0xFF, 0xFE]) {
            (UTF_16LE, 2, false)
        } else if first.starts_with(&[0xFE, 0xFF]) {
            (UTF_16BE, 2, false)
        } else if let Some(enc) = self.header_encoding {
            (enc, 0, false)
        } else if let Some(enc) = prescan_meta_charset(first) {
            (enc, 0, false)
        } else {
            // A meta tag past the pre-scan window can still switch encoding mid-stream.
            (UTF_8, 0, true)
        };
        self.stage = match AsciiCompatibleEncoding::new(encoding) {
            Some(ascii) => Stage::Direct(build_rewriter(&self.state, ascii, adjust_on_meta)),
            None => {
                let utf8 = AsciiCompatibleEncoding::new(UTF_8).expect("UTF-8 is ASCII-compatible");
                Stage::Transcoding(
                    encoding.new_decoder_without_bom_handling(),
                    build_rewriter(&self.state, utf8, false),
                )
            }
        };
        self.feed(&first[skip.min(first.len())..]);
    }

    fn feed(&mut self, chunk: &[u8]) {
        let ok = match &mut self.stage {
            Stage::Direct(rw) => rw.write(chunk).is_ok(),
            Stage::Transcoding(decoder, rw) => {
                let text = decode(decoder, chunk, false);
                rw.write(text.as_bytes()).is_ok()
            }
            Stage::Sniffing(_) | Stage::Stopped => true,
        };
        if !ok {
            self.stage = Stage::Stopped;
        }
    }
}

fn decode(decoder: &mut Decoder, src: &[u8], last: bool) -> String {
    let mut out = String::with_capacity(
        decoder
            .max_utf8_buffer_length(src.len())
            .unwrap_or(src.len() * 3),
    );
    let _ = decoder.decode_to_string(src, &mut out, last);
    out
}

fn charset_from_content_type(ct: &str) -> Option<&'static Encoding> {
    ct.split(';').skip(1).find_map(|param| {
        let (key, value) = param.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("charset") {
            return None;
        }
        Encoding::for_label(value.trim().trim_matches(['"', '\'']).as_bytes())
    })
}

fn discard(_: &[u8]) {}

fn build_rewriter(
    state: &Rc<RefCell<State>>,
    encoding: AsciiCompatibleEncoding,
    adjust_on_meta: bool,
) -> Rewriter {
    let s = |state: &Rc<RefCell<State>>| Rc::clone(state);
    let (
        s_any,
        s_svg,
        s_title,
        s_meta,
        s_link,
        s_base,
        s_head,
        s_a,
        s_img,
        s_script,
        s_skip,
        s_media,
        s_text,
        s_nosnippet,
    ) = (
        s(state),
        s(state),
        s(state),
        s(state),
        s(state),
        s(state),
        s(state),
        s(state),
        s(state),
        s(state),
        s(state),
        s(state),
        s(state),
        s(state),
    );
    let settings = Settings::new()
        .with_encoding(encoding)
        .with_adjust_charset_on_meta_tag(adjust_on_meta)
        .append_element_content_handler(element!("*", move |el| {
            let mut st = s_any.borrow_mut();
            st.words.break_word();
            st.nosnippet.in_word = false;
            if is_block_or_break(&el.tag_name()) {
                st.separate_open_text();
            }
            Ok(())
        }))
        .append_element_content_handler(
            // Only the elements Google documents (`span`, `div`, `section`): they need an end tag, so an
            // unclosed `<p data-nosnippet>` (lol_html never fires its end) can't leave the counter open
            // for the rest of the page, and void elements have no text to count.
            element!(
                "span[data-nosnippet], div[data-nosnippet], section[data-nosnippet]",
                move |el| {
                    s_nosnippet.borrow_mut().nosnippet_depth += 1;
                    let st = Rc::clone(&s_nosnippet);
                    on_end(el, move || {
                        let mut st = st.borrow_mut();
                        st.nosnippet_depth = st.nosnippet_depth.saturating_sub(1);
                        st.nosnippet.in_word = false;
                    })
                }
            ),
        )
        .append_element_content_handler(element!("svg", move |el| {
            s_svg.borrow_mut().svg_depth += 1;
            let st = Rc::clone(&s_svg);
            on_end(el, move || st.borrow_mut().svg_depth -= 1)
        }))
        .append_element_content_handler(element!("title", move |el| {
            let mut st = s_title.borrow_mut();
            if st.svg_depth > 0 {
                return Ok(());
            }
            st.title_count = st.title_count.saturating_add(1);
            st.title_buf = Some(String::new());
            drop(st);
            let st = Rc::clone(&s_title);
            on_end(el, move || st.borrow_mut().close_title())
        }))
        .append_element_content_handler(element!("meta", move |el| {
            s_meta.borrow_mut().on_meta(el);
            Ok(())
        }))
        .append_element_content_handler(element!("link[href]", move |el| {
            s_link.borrow_mut().on_link(el);
            Ok(())
        }))
        .append_element_content_handler(element!("base[href]", move |el| {
            let mut st = s_base.borrow_mut();
            if st.base_raw.is_none() {
                st.base_raw = el.get_attribute("href");
            }
            Ok(())
        }))
        .append_element_content_handler(element!("h1, h2", move |el| {
            let level = if el.tag_name().eq_ignore_ascii_case("h1") {
                1
            } else {
                2
            };
            let mut st = s_head.borrow_mut();
            st.close_heading(); // an unclosed heading ends where the next one starts
            st.heading = Some((level, String::new()));
            drop(st);
            let st = Rc::clone(&s_head);
            on_end(el, move || st.borrow_mut().close_heading())
        }))
        .append_element_content_handler(element!("a[href]", move |el| {
            let mut st = s_a.borrow_mut();
            if st.links_raw.len() >= MAX_LINKS {
                return Ok(());
            }
            let rel = el
                .get_attribute("rel")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let nofollow = rel.split_ascii_whitespace().any(|r| r == "nofollow");
            st.links_raw.push(RawLink {
                href: el.get_attribute("href").unwrap_or_default(),
                anchor: String::new(),
                nofollow,
            });
            st.anchor_open = true;
            drop(st);
            let st = Rc::clone(&s_a);
            on_end(el, move || st.borrow_mut().anchor_open = false)
        }))
        .append_element_content_handler(element!("img", move |el| {
            let mut st = s_img.borrow_mut();
            if !el.has_attribute("alt") {
                st.images_missing_alt += 1;
            }
            st.check_mixed(el.get_attribute("src").as_deref());
            Ok(())
        }))
        .append_element_content_handler(element!("script", move |el| {
            let mut st = s_script.borrow_mut();
            st.check_mixed(el.get_attribute("src").as_deref());
            let is_jsonld = el
                .get_attribute("type")
                .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/ld+json"));
            if is_jsonld {
                st.jsonld_buf = Some(String::new());
            }
            st.skip_depth += 1;
            drop(st);
            let st = Rc::clone(&s_script);
            on_end(el, move || st.borrow_mut().close_script())
        }))
        .append_element_content_handler(element!("style, noscript, template", move |el| {
            s_skip.borrow_mut().skip_depth += 1;
            let st = Rc::clone(&s_skip);
            on_end(el, move || st.borrow_mut().skip_depth -= 1)
        }))
        .append_element_content_handler(element!(
            "iframe[src], video[src], audio[src], source[src], embed[src]",
            move |el| {
                s_media
                    .borrow_mut()
                    .check_mixed(el.get_attribute("src").as_deref());
                Ok(())
            }
        ))
        .append_document_content_handler(doc_text!(move |t| {
            s_text.borrow_mut().on_text(t.as_str());
            Ok(())
        }));
    HtmlRewriter::new(settings, discard as fn(&[u8]))
}

/// Runs `f` at the element's end tag, or right away if it can't have one.
fn on_end(el: &mut Element<'_, '_>, f: impl FnOnce() + 'static) -> HandlerResult {
    if el.can_have_content() && !el.is_self_closing() {
        el.on_end_tag(Box::new(move |_: &mut EndTag<'_>| {
            f();
            Ok(())
        }))?;
    } else {
        f();
    }
    Ok(())
}

struct RawLink {
    href: String,
    anchor: String,
    nofollow: bool,
}

/// Counts words and hashes the visible text with whitespace collapsed.
struct WordCounter {
    count: u32,
    in_word: bool,
    space_pending: bool,
    hashed_any: bool,
    hasher: Xxh3,
}

impl WordCounter {
    fn feed(&mut self, text: &str) {
        let mut buf = [0u8; 4];
        for ch in text.chars() {
            if ch.is_whitespace() {
                self.break_word();
                continue;
            }
            if !self.in_word {
                self.count = self.count.saturating_add(1);
                self.in_word = true;
                if self.space_pending && self.hashed_any {
                    self.hasher.update(b" ");
                }
                self.space_pending = false;
            }
            self.hasher.update(ch.encode_utf8(&mut buf).as_bytes());
            self.hashed_any = true;
        }
    }

    fn break_word(&mut self) {
        if self.in_word {
            self.in_word = false;
            self.space_pending = true;
        }
    }
}

/// Counts words like [`WordCounter`] does (whitespace-separated, broken at every element
/// start) so its total is comparable with the page's `word_count`, without hashing.
#[derive(Default)]
struct SnippetWords {
    count: u32,
    in_word: bool,
}

impl SnippetWords {
    fn feed(&mut self, text: &str) {
        for ch in text.chars() {
            if ch.is_whitespace() {
                self.in_word = false;
            } else if !self.in_word {
                self.in_word = true;
                self.count = self.count.saturating_add(1);
            }
        }
    }
}

/// Lower-cased names of the `<meta name>` tags that address one crawler.
fn bot_meta_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        BOT_META_NAMES
            .iter()
            .map(|n| (*n).to_owned())
            .chain(
                codoseo_geo::registry::registry()
                    .bots
                    .iter()
                    .map(|b| b.token.to_ascii_lowercase()),
            )
            .collect()
    })
}

struct State {
    page_is_https: bool,
    svg_depth: u32,
    skip_depth: u32,
    title: Option<String>,
    title_count: u8,
    title_buf: Option<String>,
    meta_description: Option<String>,
    meta_robots: Vec<String>,
    canonical_raw: Option<String>,
    hreflang_raw: Vec<(String, String)>,
    base_raw: Option<String>,
    h1: Vec<String>,
    h2: Vec<String>,
    heading: Option<(u8, String)>,
    links_raw: Vec<RawLink>,
    anchor_open: bool,
    images_missing_alt: u32,
    og: OgTags,
    jsonld_buf: Option<String>,
    /// The open JSON-LD block went over [`MAX_JSONLD_BYTES`]; its text is dropped.
    jsonld_overflow: bool,
    jsonld_valid: u16,
    jsonld_invalid: bool,
    jsonld_too_large: bool,
    mixed_content: u32,
    words: WordCounter,
    ai_bot_meta: Vec<(String, String)>,
    tdm_reservation: Option<String>,
    tdm_policy: Option<String>,
    /// Open elements that carry `data-nosnippet`; their text is counted once however deep.
    nosnippet_depth: u32,
    nosnippet: SnippetWords,
}

impl State {
    fn new(page_is_https: bool) -> State {
        State {
            page_is_https,
            svg_depth: 0,
            skip_depth: 0,
            title: None,
            title_count: 0,
            title_buf: None,
            meta_description: None,
            meta_robots: Vec::new(),
            canonical_raw: None,
            hreflang_raw: Vec::new(),
            base_raw: None,
            h1: Vec::new(),
            h2: Vec::new(),
            heading: None,
            links_raw: Vec::new(),
            anchor_open: false,
            images_missing_alt: 0,
            og: OgTags::default(),
            jsonld_buf: None,
            jsonld_overflow: false,
            jsonld_valid: 0,
            jsonld_invalid: false,
            jsonld_too_large: false,
            mixed_content: 0,
            words: WordCounter {
                count: 0,
                in_word: false,
                space_pending: false,
                hashed_any: false,
                hasher: Xxh3::new(),
            },
            ai_bot_meta: Vec::new(),
            tdm_reservation: None,
            tdm_policy: None,
            nosnippet_depth: 0,
            nosnippet: SnippetWords::default(),
        }
    }

    fn on_text(&mut self, raw: &str) {
        if let Some(buf) = &mut self.jsonld_buf {
            if !self.jsonld_overflow {
                if buf.len() + raw.len() > MAX_JSONLD_BYTES {
                    self.jsonld_overflow = true;
                    *buf = String::new();
                } else {
                    buf.push_str(raw);
                }
            }
            return;
        }
        if let Some(buf) = &mut self.title_buf {
            push_capped(buf, raw, MAX_TITLE_CHARS * 4);
            return;
        }
        if self.skip_depth > 0 {
            return;
        }
        if let Some((_, buf)) = &mut self.heading {
            push_capped(buf, raw, MAX_HEADING_CHARS * 4);
        }
        if self.anchor_open
            && let Some(link) = self.links_raw.last_mut()
        {
            push_capped(&mut link.anchor, raw, MAX_ANCHOR_CHARS * 4);
        }
        self.words.feed(raw);
        if self.nosnippet_depth > 0 {
            self.nosnippet.feed(raw);
        }
    }

    /// A block element or `<br>` starts: keep words apart in open heading and link text.
    fn separate_open_text(&mut self) {
        if let Some((_, buf)) = &mut self.heading {
            push_capped(buf, " ", MAX_HEADING_CHARS * 4);
        }
        if self.anchor_open
            && let Some(link) = self.links_raw.last_mut()
        {
            push_capped(&mut link.anchor, " ", MAX_ANCHOR_CHARS * 4);
        }
    }

    fn on_meta(&mut self, el: &Element<'_, '_>) {
        let content = || {
            el.get_attribute("content")
                .map(|c| clean_attribute(&c, MAX_TITLE_CHARS))
        };
        if let Some(name) = el.get_attribute("name") {
            let name = name.trim().to_ascii_lowercase();
            match name.as_str() {
                "description" if self.meta_description.is_none() => {
                    self.meta_description = content()
                }
                "robots" => self.meta_robots.extend(content()),
                "tdm-reservation" if self.tdm_reservation.is_none() => {
                    self.tdm_reservation = content().filter(|c| !c.is_empty())
                }
                "tdm-policy" if self.tdm_policy.is_none() => {
                    self.tdm_policy = content().filter(|c| !c.is_empty())
                }
                _ if self.ai_bot_meta.len() < MAX_BOT_META && bot_meta_names().contains(&name) => {
                    if let Some(c) = content() {
                        self.ai_bot_meta.push((name, c));
                    }
                }
                _ => {}
            }
        }
        if let Some(property) = el.get_attribute("property") {
            let slot = match property.trim().to_ascii_lowercase().as_str() {
                "og:title" => &mut self.og.title,
                "og:description" => &mut self.og.description,
                "og:image" => &mut self.og.image,
                _ => return,
            };
            if slot.is_none() {
                *slot = content();
            }
        }
    }

    fn on_link(&mut self, el: &Element<'_, '_>) {
        let rel = el
            .get_attribute("rel")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let href = el.get_attribute("href").unwrap_or_default();
        for r in rel.split_ascii_whitespace() {
            match r {
                "canonical" if self.canonical_raw.is_none() => {
                    self.canonical_raw = Some(href.clone())
                }
                "alternate" => {
                    if let Some(lang) = el.get_attribute("hreflang") {
                        self.hreflang_raw
                            .push((lang.trim().to_owned(), href.clone()));
                    }
                }
                "stylesheet" => self.check_mixed(Some(&href)),
                _ => {}
            }
        }
    }

    fn check_mixed(&mut self, src: Option<&str>) {
        let is_http = src
            .map(str::trim_start)
            .and_then(|s| s.get(..5))
            .is_some_and(|p| p.eq_ignore_ascii_case("http:"));
        if self.page_is_https && is_http {
            self.mixed_content += 1;
        }
    }

    fn close_title(&mut self) {
        if let Some(buf) = self.title_buf.take() {
            let text = clean(&buf, MAX_TITLE_CHARS);
            if self.title.is_none() && !text.is_empty() {
                self.title = Some(text);
            }
        }
    }

    fn close_heading(&mut self) {
        if let Some((level, buf)) = self.heading.take() {
            let text = clean(&buf, MAX_HEADING_CHARS);
            let list = if level == 1 {
                &mut self.h1
            } else {
                &mut self.h2
            };
            if !text.is_empty() && list.len() < MAX_HEADINGS {
                list.push(text);
            }
        }
    }

    fn close_script(&mut self) {
        self.skip_depth = self.skip_depth.saturating_sub(1);
        if let Some(buf) = self.jsonld_buf.take() {
            if std::mem::take(&mut self.jsonld_overflow) {
                self.jsonld_too_large = true;
            } else if serde_json::from_str::<serde::de::IgnoredAny>(buf.trim()).is_ok() {
                self.jsonld_valid = self.jsonld_valid.saturating_add(1);
            } else {
                self.jsonld_invalid = true;
            }
        }
    }

    fn close_open_elements(&mut self) {
        self.close_title();
        self.close_heading();
        if self.jsonld_buf.is_some() {
            self.close_script();
        }
    }

    fn build(&mut self, page_url: &Url) -> Extracted {
        let base = self
            .base_raw
            .as_deref()
            .and_then(|b| normalize(page_url, &unescape_attribute(b)))
            .unwrap_or_else(|| page_url.clone());
        let resolve = |raw: &str| normalize(&base, &unescape_attribute(raw));

        let links = self
            .links_raw
            .drain(..)
            .filter_map(|l| {
                Some(Link {
                    url: resolve(&l.href)?,
                    anchor: clean(&l.anchor, MAX_ANCHOR_CHARS),
                    nofollow: l.nofollow,
                })
            })
            .collect();

        let jsonld = if self.jsonld_invalid {
            JsonLdStatus::Invalid
        } else if self.jsonld_too_large {
            JsonLdStatus::TooLarge
        } else if self.jsonld_valid > 0 {
            JsonLdStatus::Valid(self.jsonld_valid)
        } else {
            JsonLdStatus::Absent
        };

        let fields = PageFields {
            title: self.title.take(),
            title_count: self.title_count,
            meta_description: self.meta_description.take(),
            meta_robots: (!self.meta_robots.is_empty()).then(|| self.meta_robots.join(", ")),
            x_robots_tag: None,
            canonical: self.canonical_raw.as_deref().and_then(resolve),
            hreflang: self
                .hreflang_raw
                .drain(..)
                .filter_map(|(lang, href)| Some((lang, resolve(&href)?)))
                .collect(),
            h1: std::mem::take(&mut self.h1),
            h2: std::mem::take(&mut self.h2),
            word_count: self.words.count,
            content_hash: self.words.hasher.digest(),
            images_missing_alt: self.images_missing_alt,
            og: std::mem::take(&mut self.og),
            jsonld,
            mixed_content: self.mixed_content,
            ai: AiMeta {
                bot_meta: std::mem::take(&mut self.ai_bot_meta),
                nosnippet_words: self.nosnippet.count,
                tdm_reservation: self.tdm_reservation.take(),
                tdm_policy: self.tdm_policy.take(),
            },
        };
        Extracted { fields, links }
    }
}

fn push_capped(buf: &mut String, text: &str, max_bytes: usize) {
    if buf.len() < max_bytes {
        buf.push_str(text);
    }
}

fn unescape(raw: &str) -> String {
    htmlize::unescape(raw).into_owned()
}

/// Attribute values follow different rules: a legacy entity without `;` followed by
/// `=` or a letter stays as written, so `?a=1&region=us` is not turned into `®ion`.
fn unescape_attribute(raw: &str) -> String {
    htmlize::unescape_attribute(raw).into_owned()
}

fn clean_attribute(raw: &str, max_chars: usize) -> String {
    let decoded = unescape_attribute(raw);
    let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.char_indices().nth(max_chars) {
        Some((cut, _)) => collapsed[..cut].to_owned(),
        None => collapsed,
    }
}

/// Decodes entities, collapses whitespace and caps the length in characters.
fn clean(raw: &str, max_chars: usize) -> String {
    let decoded = unescape(raw);
    let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.char_indices().nth(max_chars) {
        Some((cut, _)) => collapsed[..cut].to_owned(),
        None => collapsed,
    }
}

fn is_block_or_break(tag: &str) -> bool {
    matches!(
        tag,
        "br" | "p"
            | "div"
            | "li"
            | "ul"
            | "ol"
            | "dl"
            | "dt"
            | "dd"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "main"
            | "table"
            | "tr"
            | "td"
            | "th"
            | "thead"
            | "tbody"
            | "blockquote"
            | "figure"
            | "figcaption"
            | "hr"
            | "pre"
            | "address"
            | "details"
            | "summary"
            | "form"
            | "fieldset"
            | "legend"
            | "option"
            | "button"
    )
}

/// The HTML spec's encoding pre-scan, simplified: the first `<meta>` in the first
/// 1024 bytes that declares a charset (via `charset` or `http-equiv` + `content`).
fn prescan_meta_charset(bytes: &[u8]) -> Option<&'static Encoding> {
    let s = &bytes[..bytes.len().min(PRESCAN_BYTES)];
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(b"<!--") {
            i = find(s, i + 4, b"-->")? + 3;
            continue;
        }
        let is_meta = s[i] == b'<'
            && s.len() > i + 5
            && s[i + 1..i + 5].eq_ignore_ascii_case(b"meta")
            && matches!(s[i + 5], b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/');
        if is_meta {
            let (attrs, end) = meta_attributes(s, i + 5);
            if let Some(enc) = charset_from_meta(&attrs) {
                return Some(match enc.name() {
                    "UTF-16LE" | "UTF-16BE" => UTF_8,
                    "x-user-defined" => encoding_rs::WINDOWS_1252,
                    _ => enc,
                });
            }
            i = end;
            continue;
        }
        i += 1;
    }
    None
}

fn find(s: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    s.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn meta_attributes(s: &[u8], mut i: usize) -> (Vec<(String, String)>, usize) {
    let mut attrs = Vec::new();
    loop {
        while i < s.len() && (s[i].is_ascii_whitespace() || s[i] == b'/') {
            i += 1;
        }
        if i >= s.len() || s[i] == b'>' {
            return (attrs, i + 1);
        }
        let start = i;
        while i < s.len() && !s[i].is_ascii_whitespace() && !matches!(s[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        let name = String::from_utf8_lossy(&s[start..i]).to_ascii_lowercase();
        while i < s.len() && s[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < s.len() && s[i] == b'=' {
            i += 1;
            while i < s.len() && s[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < s.len() && matches!(s[i], b'"' | b'\'') {
                let quote = s[i];
                let vstart = i + 1;
                i = vstart;
                while i < s.len() && s[i] != quote {
                    i += 1;
                }
                value = String::from_utf8_lossy(&s[vstart..i.min(s.len())]).into_owned();
                i += 1;
            } else {
                let vstart = i;
                while i < s.len() && !s[i].is_ascii_whitespace() && s[i] != b'>' {
                    i += 1;
                }
                value = String::from_utf8_lossy(&s[vstart..i]).into_owned();
            }
        }
        attrs.push((name, value));
    }
}

fn charset_from_meta(attrs: &[(String, String)]) -> Option<&'static Encoding> {
    let get = |name: &str| {
        attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    if let Some(charset) = get("charset") {
        return Encoding::for_label(charset.trim().as_bytes());
    }
    if get("http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("content-type")) {
        let content = get("content")?;
        let lower = content.to_ascii_lowercase();
        let at = lower.find("charset")? + "charset".len();
        let rest = content[at..].trim_start().strip_prefix('=')?.trim_start();
        let label: String = rest
            .trim_start_matches(['"', '\''])
            .chars()
            .take_while(|c| !matches!(c, ';' | '"' | '\'' | ' '))
            .collect();
        return Encoding::for_label(label.as_bytes());
    }
    None
}
