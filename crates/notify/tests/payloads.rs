//! The channel renderers: what Slack, Discord, a webhook and an email get for the same alert.

use codoseo_core::check::Severity;
use codoseo_notify::message::{AlertItem, AlertMessage, MAX_ITEMS};
use codoseo_notify::{discord, message, slack, webhook};
use uuid::Uuid;

const CRAWL: Uuid = Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0);

fn item(i: usize) -> AlertItem {
    let (severity, kind, label) = match i % 3 {
        0 => (Severity::Critical, "became_noindex", "Became noindex"),
        1 => (Severity::Warning, "status_changed", "Status changed"),
        _ => (Severity::Notice, "title_changed", "Title changed"),
    };
    AlertItem {
        severity,
        kind: kind.into(),
        kind_label: label.into(),
        url: Some(format!("https://example.com/page-{i}")),
        before: if i % 3 == 1 {
            "200".into()
        } else {
            "index".into()
        },
        after: if i % 3 == 1 {
            "500".into()
        } else {
            "noindex".into()
        },
    }
}

fn changes(n: usize) -> AlertMessage {
    AlertMessage::changes(
        "example.com",
        "https://codoseo.com/sites/abc",
        CRAWL,
        format!("{n} changes on example.com"),
        (0..n).map(item).collect(),
    )
}

fn json(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).unwrap()
}

fn email_snapshot(msg: &AlertMessage) -> String {
    let e = message::email(msg, "owner@example.com");
    format!(
        "to: {}\nsubject: {}\n--- text ---\n{}\n--- html ---\n{}\n",
        e.to,
        e.subject,
        e.text,
        e.html.unwrap_or_default()
    )
}

#[test]
fn listing_is_capped_at_twenty_and_the_rest_counted() {
    let msg = changes(400);
    assert_eq!(msg.items.len(), MAX_ITEMS);
    assert_eq!(MAX_ITEMS, 20);
    assert_eq!(msg.more, 380);
    let small = changes(3);
    assert_eq!((small.items.len(), small.more), (3, 0));
}

#[test]
fn slack_three_changes() {
    insta::assert_snapshot!(json(&slack::payload(&changes(3))));
}

#[test]
fn slack_four_hundred_changes() {
    insta::assert_snapshot!(json(&slack::payload(&changes(400))));
}

#[test]
fn discord_three_changes() {
    insta::assert_snapshot!(json(&discord::payload(&changes(3))));
}

#[test]
fn discord_four_hundred_changes() {
    insta::assert_snapshot!(json(&discord::payload(&changes(400))));
}

#[test]
fn webhook_three_changes() {
    insta::assert_snapshot!(json(&webhook::payload(&changes(3))));
}

#[test]
fn webhook_four_hundred_changes() {
    insta::assert_snapshot!(json(&webhook::payload(&changes(400))));
}

#[test]
fn email_three_changes() {
    insta::assert_snapshot!(email_snapshot(&changes(3)));
}

#[test]
fn email_four_hundred_changes() {
    insta::assert_snapshot!(email_snapshot(&changes(400)));
}

#[test]
fn unreachable_and_test_messages_have_their_own_events() {
    let down = AlertMessage::unreachable(
        "example.com",
        "https://codoseo.com/sites/abc",
        CRAWL,
        "connection refused",
    );
    let v = webhook::payload(&down);
    assert_eq!(v["event"], "unreachable");
    assert_eq!(v["changes"], serde_json::json!([]));
    insta::assert_snapshot!("slack_unreachable", json(&slack::payload(&down)));
    insta::assert_snapshot!("email_unreachable", email_snapshot(&down));

    let test = AlertMessage::test("example.com", "https://codoseo.com/sites/abc");
    let v = webhook::payload(&test);
    assert_eq!(v["event"], "test");
    assert_eq!(v["crawl_id"], serde_json::Value::Null);
    insta::assert_snapshot!("discord_test", json(&discord::payload(&test)));
}

#[test]
fn discord_stays_inside_its_limits_even_with_huge_values() {
    let mut msg = changes(20);
    for it in &mut msg.items {
        it.after = "x".repeat(2000);
        it.url = Some(format!("https://example.com/{}", "p".repeat(500)));
    }
    let v = discord::payload(&msg);
    let embed = &v["embeds"][0];
    assert!(embed["description"].as_str().unwrap().chars().count() <= 4096);
    assert!(embed["title"].as_str().unwrap().chars().count() <= 256);
    assert!(embed["fields"].as_array().map_or(0, Vec::len) <= 25);
    assert!(v["content"].as_str().unwrap().chars().count() <= 2000);
}

#[test]
fn slack_escapes_what_it_treats_as_markup() {
    let mut msg = changes(1);
    msg.items[0].after = "<!channel> & <script>".into();
    let text = slack::payload(&msg).to_string();
    assert!(!text.contains("<!channel>"), "{text}");
    assert!(
        text.contains("&lt;!channel&gt; &amp; &lt;script&gt;"),
        "{text}"
    );
}

#[test]
fn email_html_escapes_page_content() {
    let mut msg = changes(1);
    msg.items[0].after = "<script>alert(1)</script>".into();
    let e = message::email(&msg, "owner@example.com");
    let html = e.html.unwrap();
    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("&lt;script&gt;"));
}
