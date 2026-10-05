//! Slack incoming-webhook JSON: a plain-text fallback plus blocks.

use codoseo_core::check::Severity;
use serde_json::{Value, json};

use crate::message::{AlertItem, AlertMessage, fit_lines, one_line, short_url, truncate};

/// Slack's limit for a section's text.
const SECTION_BUDGET: usize = 2800;

fn emoji(s: Severity) -> &'static str {
    match s {
        Severity::Critical => ":red_circle:",
        Severity::Warning => ":large_orange_circle:",
        Severity::Notice => ":large_blue_circle:",
    }
}

/// Slack treats `&`, `<` and `>` as markup (`<!channel>` pings everyone), so page content is
/// escaped before it goes in.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn line(item: &AlertItem) -> String {
    let mut out = format!("{} *{}*", emoji(item.severity), escape(&item.kind_label));
    if let Some(url) = &item.url {
        out.push_str(&format!(
            " <{}|{}>",
            escape(url),
            escape(&short_url(url, 70))
        ));
    }
    let change = item.change_text();
    if !change.is_empty() {
        out.push_str(&format!(": {}", escape(&truncate(&one_line(&change), 80))));
    }
    out
}

pub fn payload(msg: &AlertMessage) -> Value {
    let mut blocks = vec![json!({
        "type": "header",
        "text": {"type": "plain_text", "text": truncate(&msg.headline, 150)},
    })];
    let mut more = msg.more as usize;
    if !msg.items.is_empty() {
        let lines: Vec<String> = msg.items.iter().map(line).collect();
        let (text, left_out) = fit_lines(&lines, SECTION_BUDGET);
        more += left_out;
        blocks.push(json!({"type": "section", "text": {"type": "mrkdwn", "text": text}}));
    }
    if more > 0 {
        blocks.push(json!({
            "type": "context",
            "elements": [{"type": "mrkdwn", "text": format!("and {more} more")}],
        }));
    }
    blocks.push(json!({
        "type": "actions",
        "elements": [{
            "type": "button",
            "text": {"type": "plain_text", "text": "Open in CodoSEO"},
            "url": msg.site_url,
        }],
    }));
    json!({"text": escape(&msg.headline), "blocks": blocks})
}
