//! Discord webhook JSON: a short `content` line and one embed, inside Discord's limits
//! (2000-character content, 256-character title, 4096-character description, 25 fields).

use codoseo_core::check::Severity;
use serde_json::{Value, json};

use crate::message::{AlertItem, AlertMessage, fit_lines, one_line, truncate};

const DESCRIPTION_BUDGET: usize = 3900;

fn emoji(s: Severity) -> &'static str {
    match s {
        Severity::Critical => ":red_circle:",
        Severity::Warning => ":orange_circle:",
        Severity::Notice => ":blue_circle:",
    }
}

fn color(s: Severity) -> u32 {
    match s {
        Severity::Critical => 0xE5484D,
        Severity::Warning => 0xF5A524,
        Severity::Notice => 0x3E63DD,
    }
}

fn line(item: &AlertItem) -> String {
    let mut out = format!("{} **{}**", emoji(item.severity), item.kind_label);
    if let Some(url) = &item.url {
        // Angle brackets keep Discord from unfurling every link.
        out.push_str(&format!(" <{}>", url.replace('>', "%3E")));
    }
    let change = item.change_text();
    if !change.is_empty() {
        out.push_str(&format!(": {}", truncate(&one_line(&change), 80)));
    }
    truncate(&out, 300)
}

pub fn payload(msg: &AlertMessage) -> Value {
    let lines: Vec<String> = msg.items.iter().map(line).collect();
    let (mut description, left_out) = fit_lines(&lines, DESCRIPTION_BUDGET);
    let more = msg.more as usize + left_out;
    if more > 0 {
        if !description.is_empty() {
            description.push_str("\n\n");
        }
        description.push_str(&format!("and {more} more"));
    }
    let mut embed = json!({
        "title": truncate(&msg.site_domain, 256),
        "url": msg.site_url,
        "color": color(msg.severity()),
    });
    if !description.is_empty() {
        embed["description"] = Value::String(description);
    }
    json!({
        "content": truncate(&msg.headline, 2000),
        // Page content must never ping anyone.
        "allowed_mentions": {"parse": []},
        "embeds": [embed],
    })
}
