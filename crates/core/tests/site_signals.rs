use codoseo_core::output::{LinkStatus, LlmsTxtFetch, SiteSignals};

#[test]
fn old_site_signals_json_parses_without_llms_txt() {
    let s: SiteSignals = serde_json::from_str(r#"{"home_headers":[],"tdmrep":null}"#).unwrap();
    assert_eq!(s.llms_txt, None);
    let s: SiteSignals = serde_json::from_str("{}").unwrap();
    assert_eq!(s, SiteSignals::default());
}

#[test]
fn llms_txt_round_trips_and_fills_defaults() {
    let f: LlmsTxtFetch = serde_json::from_str(r#"{"status":200}"#).unwrap();
    assert_eq!(f.status, 200);
    assert!(f.body.is_empty() && f.links.is_empty() && !f.truncated);
    let s = SiteSignals {
        llms_txt: Some(LlmsTxtFetch {
            status: 200,
            error: None,
            content_type: Some("text/plain".into()),
            bytes: 12,
            truncated: false,
            body: "# Site".into(),
            final_url: Some("https://example.com/llms.txt".into()),
            links: vec![LinkStatus {
                url: "https://example.com/a".into(),
                status: None,
                from_crawl: true,
            }],
            external_unchecked: 3,
        }),
        ..SiteSignals::default()
    };
    let back: SiteSignals = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back, s);
}

#[test]
fn a_fetch_without_a_response_keeps_its_error() {
    let f = LlmsTxtFetch {
        error: Some("request timed out".into()),
        ..LlmsTxtFetch::default()
    };
    let v = serde_json::to_value(&f).unwrap();
    assert_eq!(v["status"], 0);
    assert_eq!(v["error"], "request timed out");
    assert_eq!(serde_json::from_value::<LlmsTxtFetch>(v).unwrap(), f);
    // A response leaves the error out of the JSON.
    let v = serde_json::to_value(LlmsTxtFetch {
        status: 200,
        ..LlmsTxtFetch::default()
    })
    .unwrap();
    assert!(v.get("error").is_none());
}
