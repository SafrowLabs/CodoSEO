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

/// Decodes the HTML entities that pages use inside a user-agent string (`&amp;`, `&quot;`, numeric
/// references and the like). Unknown entities are left as they are.
fn unescape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').filter(|&j| j <= 10).and_then(|j| {
            let name = &rest[1..j];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    let num = name.strip_prefix('#')?;
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => num.parse().ok()?,
                    };
                    char::from_u32(code)
                }
            }?;
            Some((c, j + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[test]
fn unescape_handles_named_and_numeric_entities() {
    assert_eq!(
        unescape_html("a&amp;b &#40;x&#x29; &quot;q&quot; &bogus; &"),
        "a&b (x) \"q\" &bogus; &"
    );
}

#[test]
#[ignore = "needs network"]
fn user_agents_appear_on_source_pages() {
    let client = client();
    let mut failures = Vec::new();
    for bot in &registry().bots {
        let Some(ua) = &bot.user_agent else {
            continue;
        };
        match client.get(&bot.source_url).send() {
            Ok(r) if r.status().is_success() => match r.text() {
                Ok(body) if unescape_html(&body).contains(ua.as_str()) => {}
                Ok(_) => failures.push(format!(
                    "{}: {ua:?} not found on {}",
                    bot.token, bot.source_url
                )),
                Err(e) => failures.push(format!("{} {}: {e}", bot.token, bot.source_url)),
            },
            // Help centres often block automated clients; warn, do not fail.
            Ok(r) => println!(
                "warning: {} {} answered {}; user agent not checked",
                bot.token,
                bot.source_url,
                r.status()
            ),
            Err(e) => failures.push(format!("{} {}: {e}", bot.token, bot.source_url)),
        }
    }
    assert!(
        failures.is_empty(),
        "user agents not found verbatim:\n{}",
        failures.join("\n")
    );
}
