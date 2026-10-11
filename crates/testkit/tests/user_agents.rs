//! Responses chosen by User-Agent, response headers, dropped connections and the
//! request log.

use std::time::Duration;

use codoseo_testkit::{Page, SeenRequest, SiteBuilder, TestSite};

const OAI: &str = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; OAI-SearchBot/1.3; +https://openai.com/searchbot";
const CLAUDE: &str = "Claude-User/1.0 (+Claude-User@anthropic.com)";
const OURS: &str = "CodoSEObot/0.1 (+https://codoseo.com/bot)";

async fn get(site: &TestSite, path: &str, ua: Option<&str>) -> reqwest::Response {
    let client = reqwest::Client::new();
    let mut req = client.get(site.url(path));
    if let Some(ua) = ua {
        req = req.header("user-agent", ua);
    }
    req.send().await.unwrap()
}

async fn status_of(site: &TestSite, path: &str, ua: &str) -> u16 {
    get(site, path, Some(ua)).await.status().as_u16()
}

#[tokio::test]
async fn pages_are_chosen_by_user_agent_substring() {
    let site = SiteBuilder::new()
        .page(
            "/",
            Page::by_user_agent(
                vec![
                    (
                        "OAI-SearchBot",
                        Page::status(403, "<title>Forbidden</title>"),
                    ),
                    ("Claude-", Page::status(404, "<title>Gone</title>")),
                ],
                Page::html("<title>Home</title>"),
            ),
        )
        .start()
        .await;

    assert_eq!(status_of(&site, "/", OAI).await, 403);
    assert_eq!(status_of(&site, "/", CLAUDE).await, 404);
    assert_eq!(status_of(&site, "/", OURS).await, 200);
    let res = get(&site, "/", None).await;
    assert_eq!(res.status().as_u16(), 200, "no User-Agent gets the default");
    assert_eq!(res.text().await.unwrap(), "<title>Home</title>");
}

#[tokio::test]
async fn the_first_matching_rule_wins() {
    let site = SiteBuilder::new()
        .page(
            "/",
            Page::by_user_agent(
                vec![
                    ("Claude-User", Page::status(401, "")),
                    ("Claude", Page::status(402, "")),
                ],
                Page::html("ok"),
            ),
        )
        .start()
        .await;

    assert_eq!(status_of(&site, "/", CLAUDE).await, 401);
    assert_eq!(status_of(&site, "/", "Claude-SearchBot/1.0").await, 402);
}

#[tokio::test]
async fn sequences_are_counted_per_path_and_matched_rule() {
    let throttled_once = || Page::sequence(vec![Page::status(429, ""), Page::html("ok")]);
    let site = SiteBuilder::new()
        .page(
            "/",
            Page::by_user_agent(
                vec![
                    ("OAI-SearchBot", throttled_once()),
                    ("Claude-", throttled_once()),
                ],
                throttled_once(),
            ),
        )
        .page(
            "/other",
            Page::by_user_agent(vec![("Claude-", throttled_once())], Page::html("ok")),
        )
        .start()
        .await;

    assert_eq!(status_of(&site, "/", OAI).await, 429);
    assert_eq!(
        status_of(&site, "/", CLAUDE).await,
        429,
        "a separate count per rule"
    );
    assert_eq!(
        status_of(&site, "/", OURS).await,
        429,
        "the default has its own count"
    );
    assert_eq!(status_of(&site, "/", OAI).await, 200);
    assert_eq!(
        status_of(&site, "/", OAI).await,
        200,
        "the last page repeats"
    );
    assert_eq!(status_of(&site, "/", CLAUDE).await, 200);
    assert_eq!(status_of(&site, "/", OURS).await, 200);
    assert_eq!(
        status_of(&site, "/other", CLAUDE).await,
        429,
        "a separate count per path"
    );
    assert_eq!(status_of(&site, "/other", CLAUDE).await, 200);
}

#[tokio::test]
async fn every_path_can_be_chosen_by_user_agent_and_be_a_sequence() {
    let site = SiteBuilder::new()
        .every_path(Page::by_user_agent(
            vec![(
                "Claude-",
                Page::sequence(vec![Page::status(503, ""), Page::html("ok")]),
            )],
            Page::html("ok"),
        ))
        .start()
        .await;

    assert_eq!(status_of(&site, "/a", CLAUDE).await, 503);
    assert_eq!(status_of(&site, "/a", CLAUDE).await, 200);
    assert_eq!(
        status_of(&site, "/b", CLAUDE).await,
        503,
        "counted per path"
    );
    assert_eq!(status_of(&site, "/b", OURS).await, 200);
}

#[tokio::test]
async fn fixtures_set_any_status_header_and_body() {
    let site = SiteBuilder::new()
        .page(
            "/",
            Page::status(200, "<title>Just a moment...</title>")
                .header("cf-mitigated", "challenge")
                .header("server", "cloudflare"),
        )
        .page(
            "/denied",
            Page::status(403, "Access Denied").header("server", "AkamaiGHost"),
        )
        .page(
            "/busy",
            Page::status(429, "slow down").header("retry-after", "3"),
        )
        .page(
            "/json",
            Page::status(503, "{}").header("content-type", "application/json"),
        )
        .page("/page", Page::redirect(302, "/"))
        .start()
        .await;

    let res = get(&site, "/", None).await;
    assert_eq!(res.headers()["cf-mitigated"], "challenge");
    assert_eq!(res.headers()["server"], "cloudflare");
    assert_eq!(res.text().await.unwrap(), "<title>Just a moment...</title>");

    let res = get(&site, "/denied", None).await;
    assert_eq!(res.status().as_u16(), 403);
    assert_eq!(res.headers()["server"], "AkamaiGHost");

    let res = get(&site, "/busy", None).await;
    assert_eq!(res.status().as_u16(), 429);
    assert_eq!(res.headers()["retry-after"], "3");

    let res = get(&site, "/json", None).await;
    assert_eq!(res.status().as_u16(), 503);
    assert_eq!(res.headers()["content-type"], "application/json");

    let res = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(site.url("/page"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 302);
    assert_eq!(res.headers()["location"], "/");
}

#[tokio::test]
async fn a_dropped_connection_gives_no_response() {
    let site = SiteBuilder::new()
        .page(
            "/",
            Page::by_user_agent(vec![("Claude-", Page::drop_connection())], Page::html("ok")),
        )
        .start()
        .await;

    let err = reqwest::Client::new()
        .get(site.url("/"))
        .header("user-agent", CLAUDE)
        .send()
        .await
        .expect_err("the connection is closed before any response");
    assert!(!err.is_status(), "{err}");
    assert_eq!(status_of(&site, "/", OURS).await, 200);
    assert_eq!(
        site.hits_from("Claude-"),
        1,
        "a dropped request is still logged"
    );
}

#[tokio::test]
async fn any_page_can_be_delayed() {
    let site = SiteBuilder::new()
        .page(
            "/",
            Page::by_user_agent(
                vec![(
                    "Claude-",
                    Page::status(403, "").delayed(Duration::from_secs(5)),
                )],
                Page::html("ok"),
            ),
        )
        .start()
        .await;

    let err = reqwest::Client::new()
        .get(site.url("/"))
        .header("user-agent", CLAUDE)
        .timeout(Duration::from_millis(200))
        .send()
        .await
        .expect_err("the response comes after the timeout");
    assert!(err.is_timeout(), "{err}");
    assert_eq!(status_of(&site, "/", OURS).await, 200);
}

#[tokio::test]
async fn requests_are_logged_with_user_agent_and_from() {
    let site = SiteBuilder::new().html("/", "Home", &[]).start().await;

    reqwest::Client::new()
        .get(site.url("/"))
        .header("user-agent", OAI)
        .header("from", "hello@codoseo.com")
        .send()
        .await
        .unwrap();
    get(&site, "/missing?x=1", Some(CLAUDE)).await;
    get(&site, "/", None).await;

    assert_eq!(
        site.requests(),
        vec![
            SeenRequest {
                path: "/".to_owned(),
                user_agent: Some(OAI.to_owned()),
                from: Some("hello@codoseo.com".to_owned()),
            },
            SeenRequest {
                path: "/missing?x=1".to_owned(),
                user_agent: Some(CLAUDE.to_owned()),
                from: None,
            },
            SeenRequest {
                path: "/".to_owned(),
                user_agent: None,
                from: None,
            },
        ]
    );
    assert_eq!(site.hits_from("OAI-SearchBot"), 1);
    assert_eq!(site.hits_from("Claude-"), 1);
    assert_eq!(site.hits_from("PerplexityBot"), 0);
    assert_eq!(site.hits(), 3);
}
