//! The tile that stands for an AI operator next to its bots: its logo (`assets/logos/`, in brand
//! colours on a white tile) or, for an operator without one, its initials. The operator's name is
//! always written beside it, so the tile is decoration (`aria-hidden`, empty `alt`). Used by the
//! site audit's GEO section and the AI access screens (`partials/operator_logo.html`).

/// The hashed URL of an operator's logo, when there is one.
pub fn logo(operator: &str) -> Option<&'static str> {
    let name = match operator {
        "OpenAI" => "logo-openai.svg",
        "Anthropic" => "logo-anthropic.svg",
        "Google" => "logo-google.svg",
        "Perplexity" => "logo-perplexity.svg",
        "Microsoft" => "logo-microsoft.svg",
        "Apple" => "logo-apple.svg",
        "Meta" => "logo-meta.svg",
        "Amazon" => "logo-amazon.svg",
        "DuckDuckGo" => "logo-duckduckgo.svg",
        "Mistral" => "logo-mistral.svg",
        _ => return None,
    };
    Some(crate::assets::url(name))
}

/// `Common Crawl` -> `CC`, `Acme` -> `AC`: shown on the tile instead of a logo.
pub fn monogram(operator: &str) -> String {
    let words: Vec<&str> = operator.split_whitespace().collect();
    let letters: String = if words.len() > 1 {
        words
            .iter()
            .filter_map(|w| w.chars().next())
            .take(2)
            .collect()
    } else {
        operator.chars().take(2).collect()
    };
    letters.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use askama::Template;

    #[derive(Template)]
    #[template(
        source = r#"{% import "partials/operator_logo.html" as op %}{% call op::tile(name, "sm") %}{% endcall %}"#,
        ext = "html"
    )]
    struct Tile {
        name: String,
    }

    fn tile(name: &str) -> String {
        Tile {
            name: name.to_owned(),
        }
        .render()
        .unwrap()
    }

    #[test]
    fn a_known_operator_gets_its_logo_as_decoration() {
        assert_eq!(
            tile("Anthropic"),
            format!(
                r#"<span class="op-logo op-logo-sm" aria-hidden="true"><img src="{}" alt="" width="14" height="14" loading="lazy" decoding="async"></span>"#,
                logo("Anthropic").unwrap()
            )
        );
    }

    #[test]
    fn an_unknown_operator_gets_its_initials() {
        assert_eq!(
            tile("Acme Robots"),
            r#"<span class="op-logo op-logo-sm" aria-hidden="true"><b>AR</b></span>"#
        );
        assert_eq!(
            tile("Common Crawl"),
            r#"<span class="op-logo op-logo-sm" aria-hidden="true"><b>CC</b></span>"#
        );
    }

    /// The AI access animations run only for people who haven't asked for less motion.
    #[test]
    fn ai_access_motion_is_off_under_reduced_motion() {
        let css = include_str!("../assets/app.css");
        let start = css
            .find("@keyframes aia-pulse")
            .expect("the AI access animations");
        let rest = &css[start..];
        let moving = &rest[..rest
            .find("@media (prefers-reduced-motion: reduce) {")
            .unwrap()];
        assert!(moving.contains("@media (prefers-reduced-motion: no-preference) {"));
        let reduce = &rest[rest
            .find("@media (prefers-reduced-motion: reduce) {")
            .unwrap()..];
        let reduce = &reduce[..reduce.find("\n}\n").unwrap()];
        for sel in [".aia-row", ".aia-issue.is-changed", ".aia-progress .icon"] {
            assert!(reduce.contains(sel), "{sel} keeps moving: {reduce}");
        }
        assert!(reduce.contains("{ animation: none; }"), "{reduce}");
    }

    #[test]
    fn monograms_take_initials() {
        assert_eq!(monogram("Common Crawl"), "CC");
        assert_eq!(monogram("acme"), "AC");
        assert_eq!(monogram(""), "");
    }

    #[test]
    fn every_registry_operator_but_common_crawl_has_a_logo() {
        for b in &codoseo_geo::registry().bots {
            assert_eq!(
                logo(&b.operator).is_none(),
                b.operator == "Common Crawl",
                "{}",
                b.operator
            );
        }
        assert!(logo("OpenAI").unwrap().starts_with("/assets/logo-openai."));
    }
}
