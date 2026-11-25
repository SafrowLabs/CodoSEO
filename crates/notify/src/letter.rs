//! The one shape every CodoSEO email takes: the logo, a white card that opens with the mascot
//! and a title, then whatever that email is about (scores, changes, what to check, steps),
//! one button, a few facts, and a quiet footer that says why the person got it. Senders fill
//! a [`Letter`]; [`Letter::email`] renders the HTML (a table layout with inline styles, for
//! mail clients) and the plain-text part.
//!
//! Images (the logo, the mascot in each mood, the icons) are PNGs the web app serves under
//! `/email/`, so they show in every client; [`Letter::images`] is that base URL.

use askama::Template;
use codoseo_core::check::Severity;

use crate::email::Email;

/// The colour of a change's tag or a group label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Critical,
    Warning,
    Notice,
    Good,
}

impl Tone {
    pub fn from_severity(s: Severity) -> Tone {
        match s {
            Severity::Critical => Tone::Critical,
            Severity::Warning => Tone::Warning,
            Severity::Notice => Tone::Notice,
        }
    }

    /// The word on the tag and in the plain-text part: `[Critical]`.
    pub fn label(self) -> &'static str {
        match self {
            Tone::Critical => "Critical",
            Tone::Warning => "Warning",
            Tone::Notice => "Notice",
            Tone::Good => "Resolved",
        }
    }

    pub fn color(self) -> &'static str {
        match self {
            Tone::Critical => "#C5221F",
            Tone::Warning => "#9A5B00",
            Tone::Notice => "#4C544F",
            Tone::Good => "#137333",
        }
    }

    /// The tag's background.
    pub fn tint(self) -> &'static str {
        match self {
            Tone::Critical => "#FCE8E6",
            Tone::Warning => "#FDF1DC",
            Tone::Notice => "#EEEFEA",
            Tone::Good => "#E6F4EA",
        }
    }
}

/// The mascot's mood at the top of the card: the same moods the app shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Rest,
    Crawling,
    Warning,
    Error,
    Ok,
    Idle,
}

impl Mood {
    /// The image file under `/email/`: `mascot-rest.png`.
    pub fn file(self) -> &'static str {
        match self {
            Mood::Rest => "mascot-rest.png",
            Mood::Crawling => "mascot-crawling.png",
            Mood::Warning => "mascot-warning.png",
            Mood::Error => "mascot-error.png",
            Mood::Ok => "mascot-ok.png",
            Mood::Idle => "mascot-idle.png",
        }
    }
}

/// A small icon in a pale tile, before a row or above a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Pages,
    Fix,
    Week,
    Key,
    Mail,
    Crawl,
    Clock,
    Once,
    Retry,
    Wall,
    Pause,
    Safe,
    Settings,
    Chat,
}

impl Icon {
    /// The image file under `/email/`: `icon-clock.png`.
    pub fn file(self) -> &'static str {
        match self {
            Icon::Pages => "icon-pages.png",
            Icon::Fix => "icon-fix.png",
            Icon::Week => "icon-week.png",
            Icon::Key => "icon-key.png",
            Icon::Mail => "icon-mail.png",
            Icon::Crawl => "icon-crawl.png",
            Icon::Clock => "icon-clock.png",
            Icon::Once => "icon-once.png",
            Icon::Retry => "icon-retry.png",
            Icon::Wall => "icon-wall.png",
            Icon::Pause => "icon-pause.png",
            Icon::Safe => "icon-safe.png",
            Icon::Settings => "icon-settings.png",
            Icon::Chat => "icon-chat.png",
        }
    }
}

/// One score in the strip: a label, the number, a bar, and the week's change when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Score {
    pub label: String,
    /// 0 to 100.
    pub value: i16,
    pub delta: Option<i32>,
}

impl Score {
    /// The bar's width, in percent of the strip's cell.
    pub fn width(&self) -> i16 {
        self.value.clamp(0, 100)
    }

    /// `+3`, `-2`.
    pub fn delta_text(&self) -> String {
        self.delta.map(|d| format!("{d:+}")).unwrap_or_default()
    }

    pub fn delta_color(&self) -> &'static str {
        match self.delta {
            Some(d) if d < 0 => "#C5221F",
            _ => "#137333",
        }
    }
}

/// One change, issue or item: a tag or dot, a title, a smaller detail (the page), and the
/// old and new values when the thing changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line {
    pub tone: Option<Tone>,
    pub title: String,
    pub detail: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
}

impl Line {
    /// `index → noindex`, `was 200`, or the new value alone; empty when neither side is set.
    pub fn change_text(&self) -> String {
        match (self.before.as_deref(), self.after.as_deref()) {
            (Some(b), Some(a)) => format!("{b} → {a}"),
            (Some(b), None) => format!("was {b}"),
            (None, Some(a)) => a.to_owned(),
            (None, None) => String::new(),
        }
    }
}

/// A row with an icon: a step, a thing to check, a feature of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub icon: Icon,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub label: String,
    pub url: String,
}

/// A labelled list inside a section ("New issues", "Resolved").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub label: String,
    pub tone: Tone,
    pub items: Vec<Line>,
}

/// A block of its own inside the card, for emails about several things (the digest's sites).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Section {
    pub heading: String,
    /// Set to the right of the heading: a score.
    pub aside: Option<String>,
    pub lines: Vec<String>,
    pub groups: Vec<Group>,
    /// "and 4 more", after the groups.
    pub more: Option<String>,
    pub button: Option<Button>,
    /// A small line after the button, with a link in it.
    pub link: Option<Link>,
}

/// The parts render in this order, each only when set: mascot and title, intro, scores,
/// code, items, more, rows, sections, button, strip, facts, after, then the footer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Letter {
    /// Where the images are: `https://codoseo.com/email/`. Empty renders the name as text
    /// and no pictures at all.
    pub images: String,
    pub mascot: Option<Mood>,
    /// A small uppercase line above the title: "Instant alert · example.com".
    pub eyebrow: Option<String>,
    pub title: String,
    /// Paragraphs under the title.
    pub intro: Vec<String>,
    pub scores: Vec<Score>,
    /// A line of monospace text in a box: an error, a URL.
    pub code: Option<String>,
    /// Changes, with tags.
    pub items: Vec<Line>,
    /// "and 380 more", after the items.
    pub more: Option<String>,
    /// A small uppercase label above the rows: "In the full report".
    pub rows_label: Option<String>,
    pub rows: Vec<Row>,
    pub sections: Vec<Section>,
    pub button: Option<Button>,
    /// Small label and value pairs under the button: ("Crawl 212", "this morning").
    pub strip: Vec<(String, String)>,
    /// Up to three facts with icons, in columns under a rule.
    pub facts: Vec<Row>,
    /// Small paragraphs at the end of the card.
    pub after: Vec<String>,
    /// Why the person got this, one or two sentences.
    pub footer: Vec<String>,
    /// Footer links after those sentences: "Unsubscribe", "Change what you hear about".
    pub links: Vec<Link>,
    /// The one-click unsubscribe link for the headers; usually also one of `links`.
    pub unsubscribe: Option<String>,
}

#[derive(Template)]
#[template(path = "email/letter.html")]
struct Html<'a> {
    letter: &'a Letter,
}

impl Letter {
    /// The text mail clients show as a preview next to the subject.
    pub fn preheader(&self) -> &str {
        self.intro.first().map_or("", String::as_str)
    }

    /// `https://codoseo.com/email/mascot-rest.png`.
    pub fn image(&self, file: &str) -> String {
        format!("{}{file}", self.images)
    }

    /// The email, both parts.
    pub fn email(&self, to: &str, subject: &str) -> Result<Email, askama::Error> {
        Ok(Email {
            to: to.to_owned(),
            subject: subject.to_owned(),
            text: self.text(),
            html: Some(Html { letter: self }.render()?),
            unsubscribe: self.unsubscribe.clone(),
        })
    }

    /// The plain-text part. Every link is on a line of its own, so it stays clickable
    /// wherever the text is read.
    pub fn text(&self) -> String {
        let mut out = String::new();
        if let Some(e) = &self.eyebrow {
            out.push_str(e);
            out.push('\n');
        }
        out.push_str(&self.title);
        out.push_str("\n\n");
        for p in &self.intro {
            out.push_str(p);
            out.push_str("\n\n");
        }
        if !self.scores.is_empty() {
            let line: Vec<String> = self
                .scores
                .iter()
                .map(|s| {
                    let delta = s.delta.map(|d| format!(" ({d:+})")).unwrap_or_default();
                    format!("{} {}{delta}", s.label, s.value)
                })
                .collect();
            out.push_str(&line.join(" · "));
            out.push_str("\n\n");
        }
        if let Some(code) = &self.code {
            out.push_str("    ");
            out.push_str(code);
            out.push_str("\n\n");
        }
        push_items(&mut out, &self.items);
        if let Some(m) = &self.more {
            out.push_str(m);
            out.push_str(".\n");
        }
        if !self.items.is_empty() || self.more.is_some() {
            out.push('\n');
        }
        if let Some(l) = &self.rows_label {
            out.push_str(l);
            out.push_str(":\n");
        }
        push_rows(&mut out, &self.rows);
        for s in &self.sections {
            out.push_str("== ");
            out.push_str(&s.heading);
            if let Some(a) = &s.aside {
                out.push_str(": ");
                out.push_str(a);
            }
            out.push_str(" ==\n");
            for l in &s.lines {
                out.push_str(l);
                out.push('\n');
            }
            for g in &s.groups {
                out.push('\n');
                out.push_str(&g.label);
                out.push_str(":\n");
                push_items(&mut out, &g.items);
            }
            if let Some(m) = &s.more {
                out.push_str("  ");
                out.push_str(m);
                out.push('\n');
            }
            if let Some(b) = &s.button {
                out.push('\n');
                out.push_str(&b.label);
                out.push_str(":\n");
                out.push_str(&b.url);
                out.push('\n');
            }
            if let Some(l) = &s.link {
                out.push_str(&l.label);
                out.push_str(":\n");
                out.push_str(&l.url);
                out.push('\n');
            }
            out.push('\n');
        }
        if let Some(b) = &self.button {
            out.push_str(&b.label);
            out.push_str(":\n");
            out.push_str(&b.url);
            out.push_str("\n\n");
        }
        if !self.strip.is_empty() {
            let parts: Vec<String> = self.strip.iter().map(|(a, b)| format!("{a} {b}")).collect();
            out.push_str(&parts.join(" · "));
            out.push_str("\n\n");
        }
        push_rows(&mut out, &self.facts);
        for p in &self.after {
            out.push_str(p);
            out.push_str("\n\n");
        }
        for f in &self.footer {
            out.push_str(f);
            out.push('\n');
        }
        for l in &self.links {
            out.push_str(&l.label);
            out.push_str(": ");
            out.push_str(&l.url);
            out.push('\n');
        }
        out.push_str("\nCodoSEO by SafrowLabs · https://codoseo.com\n");
        out
    }
}

fn push_items(out: &mut String, items: &[Line]) {
    for item in items {
        if let Some(tone) = item.tone {
            out.push('[');
            out.push_str(tone.label());
            out.push_str("] ");
        }
        out.push_str(&item.title);
        out.push('\n');
        if let Some(d) = &item.detail {
            out.push_str("  ");
            out.push_str(d);
            out.push('\n');
        }
        let change = item.change_text();
        if !change.is_empty() {
            out.push_str("  ");
            out.push_str(&change);
            out.push('\n');
        }
    }
}

fn push_rows(out: &mut String, rows: &[Row]) {
    if rows.is_empty() {
        return;
    }
    for r in rows {
        out.push_str("- ");
        out.push_str(&r.title);
        out.push_str(": ");
        out.push_str(&r.detail);
        out.push('\n');
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter() -> Letter {
        Letter {
            images: "https://codoseo.com/email/".into(),
            mascot: Some(Mood::Warning),
            eyebrow: Some("Instant alert · example.com".into()),
            title: "2 changes on example.com".into(),
            intro: vec!["Found on this morning's crawl.".into()],
            items: vec![
                Line {
                    tone: Some(Tone::Critical),
                    title: "Key page became noindex".into(),
                    detail: Some("https://example.com/pricing".into()),
                    before: Some("index".into()),
                    after: Some("noindex".into()),
                },
                Line {
                    tone: Some(Tone::Warning),
                    title: "Title <b>changed</b> & more".into(),
                    ..Line::default()
                },
            ],
            more: Some("and 3 more".into()),
            button: Some(Button {
                label: "See the changes".into(),
                url: "https://codoseo.com/s/1/changes".into(),
            }),
            strip: vec![("Crawl 212".into(), "this morning".into())],
            after: vec!["Scores update when the crawl is done.".into()],
            footer: vec!["Sent to owner@example.com, an alert channel on CodoSEO.".into()],
            links: vec![Link {
                label: "Unsubscribe".into(),
                url: "https://codoseo.com/email/unsubscribe/t".into(),
            }],
            unsubscribe: Some("https://codoseo.com/email/unsubscribe/t".into()),
            ..Letter::default()
        }
    }

    #[test]
    fn the_text_part_keeps_every_link_on_its_own_line_and_labels_items() {
        let text = letter().text();
        let expected = "Instant alert · example.com\n2 changes on example.com\n\nFound on this \
                        morning's crawl.\n\n[Critical] Key page became noindex\n  \
                        https://example.com/pricing\n  index → noindex\n[Warning] Title \
                        <b>changed</b> & more\nand 3 more.\n\nSee the changes:\n\
                        https://codoseo.com/s/1/changes\n\nCrawl 212 this morning\n\nScores \
                        update when the crawl is done.\n\nSent to owner@example.com, an alert \
                        channel on CodoSEO.\nUnsubscribe: \
                        https://codoseo.com/email/unsubscribe/t\n\nCodoSEO by SafrowLabs · \
                        https://codoseo.com\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn the_html_part_escapes_content_and_carries_the_pictures_button_and_footer() {
        let e = letter()
            .email("owner@example.com", "CodoSEO: 2 changes")
            .unwrap();
        let html = e.html.unwrap();
        assert!(
            html.contains(r#"<img src="https://codoseo.com/email/logo.png""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<img src="https://codoseo.com/email/mascot-warning.png""#),
            "{html}"
        );
        assert!(
            html.contains("Title &#60;b&#62;changed&#60;/b&#62; &#38; more"),
            "{html}"
        );
        assert!(!html.contains("<b>changed</b>"));
        assert!(
            html.contains(r#"<a href="https://codoseo.com/s/1/changes""#),
            "{html}"
        );
        assert!(html.contains("See the changes"), "{html}");
        assert!(
            html.contains("background-color:#FCE8E6"),
            "the critical tag: {html}"
        );
        assert!(
            html.contains(">index<") && html.contains(">noindex<"),
            "{html}"
        );
        assert!(html.contains(r#"<a href="https://codoseo.com/email/unsubscribe/t""#));
        assert!(
            html.contains("Found on this morning&#39;s crawl."),
            "preheader and intro: {html}"
        );
        assert!(html.contains("Crawl 212"), "{html}");
        assert_eq!(
            e.unsubscribe.as_deref(),
            Some("https://codoseo.com/email/unsubscribe/t")
        );
        assert_eq!(e.text, letter().text());
    }

    #[test]
    fn without_images_the_name_is_text_and_every_part_renders() {
        let l = Letter {
            title: "Your week".into(),
            scores: vec![
                Score {
                    label: "Overall".into(),
                    value: 92,
                    delta: Some(3),
                },
                Score {
                    label: "GEO".into(),
                    value: 140,
                    delta: Some(-2),
                },
            ],
            code: Some("GET / timed out".into()),
            rows_label: Some("In the full report".into()),
            rows: vec![Row {
                icon: Icon::Pages,
                title: "Up to 500 pages".into(),
                detail: "Every page's checks.".into(),
            }],
            facts: vec![Row {
                icon: Icon::Clock,
                title: "15 minutes".into(),
                detail: "Then it stops working.".into(),
            }],
            sections: vec![Section {
                heading: "example.com".into(),
                aside: Some("92 (+3)".into()),
                lines: vec!["Your site passed 37/40 checks.".into()],
                groups: vec![Group {
                    label: "Resolved".into(),
                    tone: Tone::Good,
                    items: vec![Line {
                        tone: Some(Tone::Good),
                        title: "Missing title".into(),
                        detail: Some("2 pages".into()),
                        ..Line::default()
                    }],
                }],
                more: Some("and 1 more".into()),
                button: Some(Button {
                    label: "Open the dashboard".into(),
                    url: "https://codoseo.com/s/1/audit".into(),
                }),
                link: Some(Link {
                    label: "Turn these pages into rankings with RankOrg".into(),
                    url: "https://rankorg.com/?x".into(),
                }),
            }],
            ..Letter::default()
        };
        let text = l.text();
        assert!(
            text.contains("Overall 92 (+3) · GEO 140 (-2)\n\n    GET / timed out\n\n"),
            "{text}"
        );
        assert!(
            text.contains("In the full report:\n- Up to 500 pages: Every page's checks.\n\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "== example.com: 92 (+3) ==\nYour site passed 37/40 checks.\n\nResolved:\n\
                 [Resolved] Missing title\n  2 pages\n  and 1 more\n\nOpen the dashboard:\n\
                 https://codoseo.com/s/1/audit\nTurn these pages into rankings with RankOrg:\n\
                 https://rankorg.com/?x\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("- 15 minutes: Then it stops working.\n"),
            "{text}"
        );
        let html = l.email("a@b.co", "s").unwrap().html.unwrap();
        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains(">CodoSEO</span>"), "{html}");
        assert!(
            html.contains("width:100%;height:6px"),
            "the bar is clamped: {html}"
        );
        assert!(
            html.contains("GET / timed out")
                && html.contains("Up to 500 pages")
                && html.contains("15 minutes")
        );
        assert!(
            html.contains("92 (+3)")
                && html.contains("Open the dashboard")
                && html.contains("rankorg.com")
        );
    }
}
