//! The site graph's crawl tree, folder tree and bubble map: `data.json` draws the latest crawl's
//! tree from home with link scores and orphans, `page.json` answers for one page with its
//! inlinks and route, both 404 for another account, and each tab mounts its view.

mod support;

use axum::http::StatusCode;
use codoseo_core::page::PageRecord;
use serde_json::Value;
use support::{TestApp, page};
use url::Url;

const DOMAIN: &str = "example.com";

fn hash_hex(path: &str) -> String {
    let url = Url::parse(&format!("https://{DOMAIN}{path}")).unwrap();
    format!("{:016x}", codoseo_core::url::url_hash(&url))
}

fn at(path: &str, depth: Option<u16>) -> PageRecord {
    PageRecord {
        depth,
        ..page(DOMAIN, path)
    }
}

/// Home, three pages a click away and one found only in the sitemap. The test crawl links
/// home to every other page.
fn fixture() -> Vec<PageRecord> {
    vec![
        at("/", Some(0)),
        at("/about", Some(1)),
        at("/blog", Some(1)),
        at("/blog/hello", Some(1)),
        at("/lp/promo", None),
    ]
}

fn json(body: &str) -> Value {
    serde_json::from_str(body).expect("json body")
}

#[tokio::test]
async fn data_draws_the_tree_from_home() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl(&site, fixture(), vec![]).await;

    let res = app
        .get(&format!("/s/{}/graph/data.json", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let data = json(&res.body);
    assert_eq!(data["empty"], false);
    let nodes = data["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 5);
    let root = data["root"].as_u64().unwrap() as usize;
    assert_eq!(nodes[root]["u"], "/");
    assert_eq!(nodes[root]["d"], 0);
    assert!(nodes[root]["p"].is_null());
    for (i, n) in nodes.iter().enumerate() {
        let k = n["k"].as_u64().unwrap();
        assert!((1..=100).contains(&k), "{k}");
        assert_eq!(n["x"], "ok");
        let orphan = n["u"] == "/lp/promo";
        assert_eq!(n["o"], orphan, "{}", n["u"]);
        if i == root || orphan {
            assert!(n["p"].is_null(), "{}", n["u"]);
        } else {
            assert_eq!(n["p"].as_u64(), Some(root as u64), "{}", n["u"]);
            assert_eq!(n["d"], 1);
        }
    }
    let about = nodes.iter().find(|n| n["u"] == "/about").unwrap();
    assert_eq!(about["h"], hash_hex("/about"));
    assert_eq!(about["t"], "/about | A page title that is long enough");
    assert_eq!(about["s"], 200);
    // The only link beyond the tree: home to the page it never adopted.
    let promo = nodes.iter().position(|n| n["u"] == "/lp/promo").unwrap() as u64;
    assert_eq!(data["e"], serde_json::json!([root as u64, promo]));
    assert_eq!(data["stats"]["pages"], 5);
    assert_eq!(data["stats"]["orphans"], 1);
    assert_eq!(data["stats"]["depth"], 1);
    assert!(data["capped"].is_null());
}

#[tokio::test]
async fn data_is_empty_without_a_graph() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    let path = format!("/s/{}/graph/data.json", site.id);

    let data = json(&app.get(&path, Some(&cookie)).await.body);
    assert_eq!(data["empty"], true);
    assert_eq!(data["nodes"], serde_json::json!([]));

    // A crawl finished before graphs were kept.
    let crawl = app.finished_crawl(&site, fixture(), vec![]).await;
    sqlx::query("DELETE FROM crawl_graphs WHERE crawl_id = $1")
        .bind(crawl)
        .execute(app.pool())
        .await
        .unwrap();
    let data = json(&app.get(&path, Some(&cookie)).await.body);
    assert_eq!(data["empty"], true);
}

#[tokio::test]
async fn page_lists_inlinks_and_the_route_from_home() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl(&site, fixture(), vec![]).await;

    let res = app
        .get(
            &format!("/s/{}/graph/page.json?h={}", site.id, hash_hex("/blog")),
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let p = json(&res.body);
    assert_eq!(p["url"], "https://example.com/blog");
    assert_eq!(p["depth"], 1);
    assert_eq!(p["x"], "ok");
    assert_eq!(p["linked_from"], 1);
    assert_eq!(p["links"][0]["url"], "https://example.com/");
    assert_eq!(p["links"][0]["anchor"], "Main navigation");
    assert_eq!(p["links"][0]["h"], hash_hex("/"));
    assert_eq!(p["outlinks"], 0);
    let route: Vec<_> = p["route"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["url"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(route, ["https://example.com/", "https://example.com/blog"]);
    assert_eq!(
        p["route"][1]["title"],
        "/blog | A page title that is long enough"
    );

    // Home links to every page.
    let home = json(
        &app.get(
            &format!("/s/{}/graph/page.json?h={}", site.id, hash_hex("/")),
            Some(&cookie),
        )
        .await
        .body,
    );
    assert_eq!(home["outlinks"], 4);
    assert_eq!(home["route"].as_array().unwrap().len(), 1);

    for bad in ["nothex", "0000000000000001", ""] {
        let res = app
            .get(
                &format!("/s/{}/graph/page.json?h={bad}", site.id),
                Some(&cookie),
            )
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{bad}");
    }
}

#[tokio::test]
async fn another_accounts_graph_is_a_404() {
    let app = TestApp::new().await;
    let (account, _) = app.login("a@x.com").await;
    let (_, other) = app.login("b@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl(&site, fixture(), vec![]).await;
    for path in [
        format!("/s/{}/graph/data.json", site.id),
        format!("/s/{}/graph/page.json?h={}", site.id, hash_hex("/")),
    ] {
        let res = app.get(&path, Some(&other)).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn each_tab_mounts_its_view() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl(&site, fixture(), vec![]).await;
    for (tab, view) in [("tree", "tree"), ("folders", "folders"), ("map", "map")] {
        let res = app
            .get(&format!("/s/{}/graph/{tab}", site.id), Some(&cookie))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{tab}");
        assert!(
            res.body.contains(&format!(r#"data-graph="{view}""#)),
            "{tab}"
        );
        assert!(
            res.body
                .contains(&format!(r#"data-src="/s/{}/graph/data.json?v=1""#, site.id)),
            "{tab}"
        );
        assert!(res.body.contains("/assets/graph."), "{tab}");
        assert!(res.body.contains("/assets/d3.min."), "{tab}");
        let start = res.body.find(r#"<section class="gv"#).unwrap();
        let view_html = &res.body[start..res.body[start..].find("</section>").unwrap() + start];
        assert!(!view_html.contains('\u{2014}'), "{tab} has an em dash");
    }
}

/// graph.js and the server-drawn graph views share app.css. A class graph.js builds its cards
/// from must have one plain rule: a second `.name { … }` further down restyles every use of it
/// (that is how the tree cards once lost their labels and the depth cards their place).
#[test]
fn each_graph_js_class_has_one_plain_rule_in_app_css() {
    let js = include_str!("../assets/graph.js");
    let css = include_str!("../assets/app.css");
    let mut classes: Vec<&str> = Vec::new();
    for (i, _) in js.match_indices("gv-") {
        let name = &js[i..];
        let end = name
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .unwrap_or(name.len());
        let name = name[..end].trim_end_matches('-');
        if !classes.contains(&name) {
            classes.push(name);
        }
    }
    assert!(classes.contains(&"gv-card"), "{classes:?}");
    let doubled: Vec<String> = classes
        .iter()
        .filter_map(|c| {
            let rule = format!(".{c} {{");
            let n = css.lines().filter(|l| l.starts_with(&rule)).count();
            (n > 1).then(|| format!("{c} ({n} rules)"))
        })
        .collect();
    assert!(doubled.is_empty(), "styled twice: {doubled:?}");
}
