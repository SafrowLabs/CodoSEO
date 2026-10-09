use codoseo_geo::intent::{Intent, Stance};
use codoseo_geo::registry::{Purpose, registry};

fn bot(token: &str) -> &'static codoseo_geo::Bot {
    registry().bot(token).expect("bot")
}

#[test]
fn defaults_allow_search_and_fetch_and_have_no_view_on_the_rest() {
    let i = Intent::default();
    assert_eq!(i.effective(bot("OAI-SearchBot")), Stance::Allow);
    assert_eq!(i.effective(bot("ChatGPT-User")), Stance::Allow);
    assert_eq!(i.effective(bot("GPTBot")), Stance::Any);
    assert_eq!(i.effective(bot("Google-Agent")), Stance::Any);
    assert_eq!(i.effective(bot("OAI-AdsBot")), Stance::Any);
}

#[test]
fn bot_override_beats_purpose_and_ignores_case() {
    let i: Intent = serde_json::from_str(
        r#"{"purposes": {"search": "allow", "training": "block"}, "bots": {"gptbot": "allow", "NotABot": "block"}}"#,
    )
    .expect("intent");
    assert_eq!(i.purpose_stance(Purpose::Training), Stance::Block);
    assert_eq!(i.effective(bot("GPTBot")), Stance::Allow);
    assert_eq!(i.effective(bot("CCBot")), Stance::Block);
    // Unknown tokens are kept, not applied to anything.
    assert_eq!(i.bot_override("NotABot"), Some(Stance::Block));
}

#[test]
fn missing_keys_mean_defaults_and_the_shape_round_trips() {
    let i: Intent = serde_json::from_str("{}").expect("empty");
    assert_eq!(i, Intent::default());
    assert_eq!(serde_json::to_string(&i).expect("json"), "{}");
    let json = r#"{"purposes":{"training":"block"},"bots":{"GPTBot":"any"}}"#;
    let i: Intent = serde_json::from_str(json).expect("intent");
    assert_eq!(serde_json::to_string(&i).expect("json"), json);
}
