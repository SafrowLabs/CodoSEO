//! The overall, SEO and GEO scores as one bar chart, shared by the site audit, the live audit
//! and the free audit (`partials/score_bars.html`).

/// One bar: a score out of 100 with what moved it.
pub struct ScoreBar {
    /// `overall`, `seo` or `geo`: picks the bar's colour.
    pub key: &'static str,
    pub label: &'static str,
    /// `None` while there is no score yet; the bar stays empty.
    pub value: Option<i16>,
    /// `up` / `down` / empty, for `change`.
    pub change_class: &'static str,
    pub change: String,
    pub detail: String,
    /// Still moving: a live dot by the label.
    pub live: bool,
    /// The `?` popover id next to the label, when the page has one.
    pub help: Option<&'static str>,
}

impl ScoreBar {
    pub fn new(key: &'static str, value: Option<i16>) -> ScoreBar {
        ScoreBar {
            key,
            label: match key {
                "seo" => "SEO",
                "geo" => "GEO",
                _ => "Overall",
            },
            value,
            change_class: "",
            change: String::new(),
            detail: String::new(),
            live: false,
            help: None,
        }
    }

    /// The bar's width in percent.
    pub fn width(&self) -> i16 {
        self.value.unwrap_or(0).clamp(0, 100)
    }

    pub fn shown(&self) -> String {
        self.value.map_or_else(|| "–".to_owned(), |v| v.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_is_the_score_kept_within_0_to_100() {
        assert_eq!(ScoreBar::new("seo", Some(78)).width(), 78);
        assert_eq!(ScoreBar::new("seo", Some(140)).width(), 100);
        assert_eq!(ScoreBar::new("geo", None).width(), 0);
        assert_eq!(ScoreBar::new("geo", None).shown(), "–");
        assert_eq!(ScoreBar::new("overall", Some(5)).label, "Overall");
    }
}
