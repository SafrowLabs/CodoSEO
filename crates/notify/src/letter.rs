//! The one shape every CodoSEO email takes: the logo, a white card with a title, a few
//! paragraphs, an optional list or set of sections, one button, and a quiet footer that says
//! why the person got it. Senders fill a [`Letter`]; [`Letter::email`] renders the HTML (a
//! table layout with inline styles, for mail clients) and the plain-text part.

use askama::Template;
use codoseo_core::check::Severity;

use crate::email::Email;

/// The colour of a list item's dot and a group label.
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

    /// The word in the plain-text part: `[Critical]`.
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
            Tone::Notice => "#6B736E",
            Tone::Good => "#137333",
        }
    }
}

/// One line of a list: a dot, a title, and a smaller detail under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub tone: Tone,
    pub title: String,
    pub detail: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Letter {
    /// The logo image, absolute. Empty renders the name as text.
    pub logo_url: String,
    /// A small uppercase line above the title: "Instant alert · example.com".
    pub eyebrow: Option<String>,
    pub title: String,
    /// Paragraphs under the title.
    pub intro: Vec<String>,
    pub items: Vec<Line>,
    /// "and 380 more", after the items.
    pub more: Option<String>,
    pub sections: Vec<Section>,
    pub button: Option<Button>,
    /// Small paragraphs after the button.
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
        push_items(&mut out, &self.items);
        if let Some(m) = &self.more {
            out.push_str(m);
            out.push_str(".\n");
        }
        if !self.items.is_empty() || self.more.is_some() {
            out.push('\n');
        }
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
        out.push('[');
        out.push_str(item.tone.label());
        out.push_str("] ");
        out.push_str(&item.title);
        out.push('\n');
        if let Some(d) = &item.detail {
            out.push_str("  ");
            out.push_str(d);
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter() -> Letter {
        Letter {
            logo_url: "https://codoseo.com/email-logo.png".into(),
            eyebrow: Some("Instant alert · example.com".into()),
            title: "2 changes on example.com".into(),
            intro: vec!["Found on this morning's crawl.".into()],
            items: vec![
                Line {
                    tone: Tone::Critical,
                    title: "Key page became noindex".into(),
                    detail: Some("https://example.com/pricing · index → noindex".into()),
                },
                Line {
                    tone: Tone::Warning,
                    title: "Title <b>changed</b> & more".into(),
                    detail: None,
                },
            ],
            more: Some("and 3 more".into()),
            sections: Vec::new(),
            button: Some(Button {
                label: "See the changes".into(),
                url: "https://codoseo.com/s/1/changes".into(),
            }),
            after: vec!["Scores update when the crawl is done.".into()],
            footer: vec!["Sent to owner@example.com, an alert channel on CodoSEO.".into()],
            links: vec![Link {
                label: "Unsubscribe".into(),
                url: "https://codoseo.com/email/unsubscribe/t".into(),
            }],
            unsubscribe: Some("https://codoseo.com/email/unsubscribe/t".into()),
        }
    }

    #[test]
    fn the_text_part_keeps_every_link_on_its_own_line_and_labels_items() {
        let text = letter().text();
        let expected = "Instant alert · example.com\n2 changes on example.com\n\nFound on this \
                        morning's crawl.\n\n[Critical] Key page became noindex\n  \
                        https://example.com/pricing · index → noindex\n[Warning] Title \
                        <b>changed</b> & more\nand 3 more.\n\nSee the changes:\n\
                        https://codoseo.com/s/1/changes\n\nScores update when the crawl is \
                        done.\n\nSent to owner@example.com, an alert channel on CodoSEO.\n\
                        Unsubscribe: https://codoseo.com/email/unsubscribe/t\n\nCodoSEO by \
                        SafrowLabs · https://codoseo.com\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn the_html_part_escapes_content_and_carries_the_logo_button_and_footer() {
        let e = letter()
            .email("owner@example.com", "CodoSEO: 2 changes")
            .unwrap();
        let html = e.html.unwrap();
        assert!(
            html.contains(r#"<img src="https://codoseo.com/email-logo.png""#),
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
            html.contains("background-color:#C5221F"),
            "the critical dot: {html}"
        );
        assert!(html.contains(r#"<a href="https://codoseo.com/email/unsubscribe/t""#));
        assert!(
            html.contains("Found on this morning&#39;s crawl."),
            "preheader and intro: {html}"
        );
        assert_eq!(
            e.unsubscribe.as_deref(),
            Some("https://codoseo.com/email/unsubscribe/t")
        );
        assert_eq!(e.text, letter().text());
    }

    #[test]
    fn without_a_logo_the_name_is_text_and_sections_render() {
        let l = Letter {
            title: "Your week".into(),
            sections: vec![Section {
                heading: "example.com".into(),
                aside: Some("92 (+3)".into()),
                lines: vec!["Your site passed 37/40 checks.".into()],
                groups: vec![Group {
                    label: "Resolved".into(),
                    tone: Tone::Good,
                    items: vec![Line {
                        tone: Tone::Good,
                        title: "Missing title".into(),
                        detail: Some("2 pages".into()),
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
        assert!(text.contains("== example.com: 92 (+3) ==\nYour site passed 37/40 checks.\n\nResolved:\n[Resolved] Missing title\n  2 pages\n  and 1 more\n\nOpen the dashboard:\nhttps://codoseo.com/s/1/audit\nTurn these pages into rankings with RankOrg:\nhttps://rankorg.com/?x\n"), "{text}");
        let html = l.email("a@b.co", "s").unwrap().html.unwrap();
        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains(">CodoSEO</span>"), "{html}");
        assert!(
            html.contains("92 (+3)")
                && html.contains("Open the dashboard")
                && html.contains("rankorg.com")
        );
    }
}
