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
use std::rc::Rc;

use codoseo_core::page::{JsonLdStatus, OgTags, PageFields};
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
            if pending.len() >= 3 {
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
        } else {
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
    );
    let settings = Settings::new()
        .with_encoding(encoding)
        .with_adjust_charset_on_meta_tag(adjust_on_meta)
        .append_element_content_handler(element!("*", move |_el| {
            s_any.borrow_mut().words.break_word();
            Ok(())
        }))
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
            s_head.borrow_mut().heading = Some((level, String::new()));
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
    jsonld_valid: u16,
    jsonld_invalid: bool,
    mixed_content: u32,
    words: WordCounter,
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
            jsonld_valid: 0,
            jsonld_invalid: false,
            mixed_content: 0,
            words: WordCounter {
                count: 0,
                in_word: false,
                space_pending: false,
                hashed_any: false,
                hasher: Xxh3::new(),
            },
        }
    }

    fn on_text(&mut self, raw: &str) {
        if let Some(buf) = &mut self.jsonld_buf {
            buf.push_str(raw);
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
    }

    fn on_meta(&mut self, el: &Element<'_, '_>) {
        let content = || {
            el.get_attribute("content")
                .map(|c| clean(&c, MAX_TITLE_CHARS))
        };
        if let Some(name) = el.get_attribute("name") {
            match name.trim().to_ascii_lowercase().as_str() {
                "description" if self.meta_description.is_none() => {
                    self.meta_description = content()
                }
                "robots" => self.meta_robots.extend(content()),
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
            if serde_json::from_str::<serde_json::Value>(buf.trim()).is_ok() {
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
            .and_then(|b| normalize(page_url, &unescape(b)))
            .unwrap_or_else(|| page_url.clone());
        let resolve = |raw: &str| normalize(&base, &unescape(raw));

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

/// Decodes entities, collapses whitespace and caps the length in characters.
fn clean(raw: &str, max_chars: usize) -> String {
    let decoded = unescape(raw);
    let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.char_indices().nth(max_chars) {
        Some((cut, _)) => collapsed[..cut].to_owned(),
        None => collapsed,
    }
}
