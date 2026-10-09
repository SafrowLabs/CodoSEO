//! Network check of the registry's URLs. Ignored by default; `.github/workflows/bots.yml` runs it:
//! `cargo test -p codoseo-geo --test registry_live -- --ignored --nocapture`

use std::time::Duration;

use codoseo_geo::registry;
use reqwest::blocking::Client;

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (compatible; codoseo-registry-check)")
        .build()
        .expect("client")
}

#[test]
#[ignore = "needs network"]
fn ip_range_files_are_json_prefix_lists() {
    let client = client();
    let mut failures = Vec::new();
    for bot in &registry().bots {
        let Some(url) = &bot.ip_ranges_url else {
            continue;
        };
        let result = client
            .get(url)
            .send()
            .map_err(|e| e.to_string())
            .and_then(|r| {
                let status = r.status();
                if status != 200 {
                    return Err(format!("status {status}"));
                }
                r.json::<serde_json::Value>().map_err(|e| e.to_string())
            })
            .and_then(|v| {
                let prefixes = v["prefixes"].as_array().ok_or("no prefixes array")?;
                let ok = !prefixes.is_empty()
                    && prefixes
                        .iter()
                        .all(|p| p.get("ipv4Prefix").is_some() || p.get("ipv6Prefix").is_some());
                ok.then_some(())
                    .ok_or_else(|| "prefix entries malformed".to_owned())
            });
        if let Err(e) = result {
            failures.push(format!("{} {url}: {e}", bot.token));
        }
    }
    assert!(
        failures.is_empty(),
        "IP files failed:\n{}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "needs network"]
fn source_pages_answer() {
    let client = client();
    let mut failures = Vec::new();
    for bot in &registry().bots {
        match client.get(&bot.source_url).send() {
            Ok(r) if r.status().as_u16() < 400 => {}
            // Help centres often block automated clients; warn, do not fail.
            Ok(r) if r.status().as_u16() == 403 => {
                println!("warning: {} {} answered 403", bot.token, bot.source_url);
            }
            Ok(r) => failures.push(format!("{} {}: {}", bot.token, bot.source_url, r.status())),
            Err(e) => failures.push(format!("{} {}: {e}", bot.token, bot.source_url)),
        }
    }
    assert!(
        failures.is_empty(),
        "source pages failed:\n{}",
        failures.join("\n")
    );
}
