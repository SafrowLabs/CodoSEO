//! The synthetic fetch probes' pure half: challenge detection, page titles, the probe plan, the
//! verdicts and the probe report.

mod common;

use std::collections::HashSet;

use codoseo_core::output::CrawlOutput;
use codoseo_core::url::url_hash;
use codoseo_geo::probe::{
    Challenge, FetchErrorKind, InconclusiveReason, MAX_TITLE, NEVER_PROBE, PROBE_VERSION,
    ProbeFetch, ProbeReport, ProbeResult, ProbeStop, ProbedUrl, SkipReason, SkippedBot, Vendor,
    Verdict, candidates, classify, detect_challenge, is_probe_candidate, page_title, plan_probes,
    robots_skips,
};
use codoseo_geo::registry::{Purpose, registry};
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use codoseo_geo::robots::RobotsTxt;
use common::*;

type Headers = Vec<(String, String)>;

fn h(pairs: &[(&str, &str)]) -> Headers {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// A page of about `size` bytes with `inner` near the top.
fn padded(inner: &str, size: usize) -> Vec<u8> {
    let mut s = format!("<html><head><title>Shop</title></head><body>{inner}<p>");
    while s.len() < size {
        s.push_str("Lorem ipsum dolor sit amet, consectetur adipiscing elit. ");
    }
    s.push_str("</p></body></html>");
    s.into_bytes()
}

const VENDORS: [Vendor; 9] = [
    Vendor::Cloudflare,
    Vendor::Vercel,
    Vendor::AwsWaf,
    Vendor::DataDome,
    Vendor::Human,
    Vendor::Imperva,
    Vendor::Akamai,
    Vendor::Sucuri,
    Vendor::Generic,
];

#[test]
fn every_vendor_marker_is_detected() {
    // (status, headers, body, vendor, signal)
    let cases: Vec<(u16, Headers, Vec<u8>, Vendor, &str)> = vec![
        (
            403,
            h(&[("CF-Mitigated", "challenge")]),
            b"<html></html>".to_vec(),
            Vendor::Cloudflare,
            "cf-mitigated: challenge",
        ),
        (
            403,
            vec![],
            b"<title>Just a moment...</title>".to_vec(),
            Vendor::Cloudflare,
            "Just a moment...",
        ),
        (
            403,
            vec![],
            b"<div id=\"cf-chl-widget\"></div>".to_vec(),
            Vendor::Generic,
            "cf-chl",
        ),
        (
            200,
            vec![],
            b"<script>window._cf_chl_opt={cvId:'3'}</script>".to_vec(),
            Vendor::Cloudflare,
            "_cf_chl_opt",
        ),
        (
            403,
            vec![],
            b"<script src=\"https://challenges.cloudflare.com/x.js\"></script>".to_vec(),
            Vendor::Generic,
            "challenges.cloudflare.com",
        ),
        (
            403,
            vec![],
            b"<title>Attention Required! | Cloudflare</title>".to_vec(),
            Vendor::Cloudflare,
            "Attention Required! | Cloudflare",
        ),
        (
            403,
            vec![],
            b"<div id=\"cf-error-details\">".to_vec(),
            Vendor::Cloudflare,
            "cf-error-details",
        ),
        (
            403,
            vec![],
            b"<h1>Error 1020</h1> Access denied".to_vec(),
            Vendor::Cloudflare,
            "Error 1020",
        ),
        (
            403,
            vec![],
            b"<h1>Error 1010</h1> The owner of this website has banned your access based on your browser's signature".to_vec(),
            Vendor::Cloudflare,
            "Error 1010",
        ),
        (
            429,
            h(&[("x-vercel-mitigated", "challenge")]),
            b"".to_vec(),
            Vendor::Vercel,
            "x-vercel-mitigated: challenge",
        ),
        (
            429,
            vec![],
            b"<title>Vercel Security Checkpoint</title>".to_vec(),
            Vendor::Vercel,
            "Vercel Security Checkpoint",
        ),
        (
            202,
            h(&[("x-amzn-waf-action", "challenge")]),
            b"".to_vec(),
            Vendor::AwsWaf,
            "x-amzn-waf-action: challenge",
        ),
        (
            405,
            h(&[("X-Amzn-Waf-Action", "CAPTCHA")]),
            b"".to_vec(),
            Vendor::AwsWaf,
            "x-amzn-waf-action: captcha",
        ),
        (
            403,
            h(&[("x-datadome", "protected")]),
            b"<html></html>".to_vec(),
            Vendor::DataDome,
            "x-datadome",
        ),
        (
            403,
            h(&[("x-dd-b", "1")]),
            b"<html></html>".to_vec(),
            Vendor::DataDome,
            "x-dd-b",
        ),
        (
            200,
            vec![],
            b"<script src=\"https://ct.captcha-delivery.com/c.js\"></script>".to_vec(),
            Vendor::DataDome,
            "captcha-delivery.com",
        ),
        (
            403,
            vec![],
            b"<div id=\"px-captcha\"></div>".to_vec(),
            Vendor::Human,
            "px-captcha",
        ),
        (
            200,
            vec![],
            b"<script>window._pxCaptcha=1</script>".to_vec(),
            Vendor::Human,
            "_pxCaptcha",
        ),
        (
            403,
            vec![],
            b"<script src=\"/_px/init.js\"></script>".to_vec(),
            Vendor::Human,
            "/_px/",
        ),
        (
            200,
            vec![],
            b"<p>Press & Hold to confirm you are a human</p>".to_vec(),
            Vendor::Human,
            "Press & Hold",
        ),
        (
            200,
            vec![],
            b"<p>Press &amp; Hold to confirm you are a human</p>".to_vec(),
            Vendor::Human,
            "Press &amp; Hold",
        ),
        (
            403,
            h(&[("X-Iinfo", "10-123-0 0CNN RT(1 0) q(0 -1 -1 -1) r(0 -1)")]),
            b"<html></html>".to_vec(),
            Vendor::Imperva,
            "x-iinfo",
        ),
        (
            200,
            vec![],
            b"<iframe src=\"/_Incapsula_Resource?SWUDNSAI=31\"></iframe>".to_vec(),
            Vendor::Imperva,
            "_Incapsula_Resource",
        ),
        (
            200,
            vec![],
            b"Request unsuccessful. Incapsula incident ID: 123-456".to_vec(),
            Vendor::Imperva,
            "Incapsula incident ID",
        ),
        (
            403,
            h(&[("Server", "AkamaiGHost")]),
            b"<H1>Access Denied</H1> You don't have permission. Reference #18.6b2c.1".to_vec(),
            Vendor::Akamai,
            "AkamaiGHost: Access Denied",
        ),
        (
            403,
            h(&[("server", "AkamaiGHost")]),
            // Akamai's own page writes the reference with character references.
            b"<H1>Access Denied</H1> You don't have permission. Reference&#32;&#35;18&#46;6b2c".to_vec(),
            Vendor::Akamai,
            "AkamaiGHost: Access Denied",
        ),
        (
            403,
            vec![],
            b"https://errors.edgesuite.net/18.6b2c".to_vec(),
            Vendor::Akamai,
            "errors.edgesuite.net",
        ),
        (
            403,
            vec![],
            b"https&#58;&#47;&#47;errors&#46;edgesuite&#46;net&#47;18".to_vec(),
            Vendor::Akamai,
            "errors.edgesuite.net",
        ),
        (
            403,
            h(&[("X-Sucuri-Block", "BOT01")]),
            b"".to_vec(),
            Vendor::Sucuri,
            "x-sucuri-block",
        ),
        (
            403,
            vec![],
            b"<title>Sucuri WebSite Firewall - Access Denied</title>".to_vec(),
            Vendor::Sucuri,
            "Sucuri WebSite Firewall - Access Denied",
        ),
        (
            200,
            vec![],
            b"<div class=\"g-recaptcha\" data-sitekey=\"x\"></div>".to_vec(),
            Vendor::Generic,
            "g-recaptcha",
        ),
        (
            200,
            vec![],
            b"<script src=\"https://js.hcaptcha.com/1/api.js\"></script>".to_vec(),
            Vendor::Generic,
            "hcaptcha.com",
        ),
        (
            200,
            vec![],
            b"<div class=\"cf-turnstile\"></div>".to_vec(),
            Vendor::Generic,
            "cf-turnstile",
        ),
        (
            200,
            vec![],
            b"<title>Security CAPTCHA</title>".to_vec(),
            Vendor::Generic,
            "<title> captcha",
        ),
        (
            200,
            vec![],
            b"<p>Please verify you are human</p>".to_vec(),
            Vendor::Generic,
            "verify you are human",
        ),
        (
            200,
            vec![],
            b"<p>Enable JavaScript and cookies to continue</p>".to_vec(),
            Vendor::Generic,
            "enable JavaScript and cookies",
        ),
    ];
    let mut seen = std::collections::HashSet::new();
    for (status, headers, body, vendor, signal) in cases {
        let got = detect_challenge(status, &headers, &body);
        assert_eq!(
            got,
            Some(Challenge {
                vendor,
                signal: signal.to_owned()
            }),
            "{}",
            String::from_utf8_lossy(&body)
        );
        seen.insert(vendor);
    }
    for v in VENDORS {
        assert!(seen.contains(&v), "{v:?} has no fixture");
    }
}

#[test]
fn ordinary_pages_are_not_challenges() {
    let clean: Vec<(&str, u16, Headers, Vec<u8>)> = vec![
        (
            "a page that talks about Cloudflare",
            200,
            vec![],
            padded(
                "<h1>Why we moved our CDN to Cloudflare</h1><p>Cloudflare made our site faster.</p>",
                4_000,
            ),
        ),
        (
            "a large product page with a reCAPTCHA review form",
            200,
            vec![],
            padded("<form><div class=\"g-recaptcha\"></div></form>", 40 * 1024),
        ),
        (
            "server: cloudflare on a normal 200",
            200,
            h(&[("server", "cloudflare"), ("cf-ray", "8a1b2c3d4e5f-AMS")]),
            padded("<h1>Welcome</h1>", 2_000),
        ),
        (
            "DataDome's pass-through header on a normal 200",
            200,
            h(&[("x-datadome", "protected"), ("x-dd-b", "1")]),
            padded("<h1>Welcome</h1>", 2_000),
        ),
        (
            "Imperva's diagnostic header on a normal 200",
            200,
            h(&[("x-iinfo", "10-1-0 NNNN CT(0 0 0)")]),
            padded("<h1>Welcome</h1>", 2_000),
        ),
        (
            "an Akamai-served page without the denial",
            200,
            h(&[("server", "AkamaiGHost")]),
            padded("<h1>Access the app</h1><p>Reference guide</p>", 2_000),
        ),
        (
            "an Akamai-served 404 that only says Access Denied",
            404,
            h(&[("server", "AkamaiGHost")]),
            b"<h1>Access Denied</h1>".to_vec(),
        ),
        (
            "cf-mitigated with another value",
            200,
            h(&[("cf-mitigated", "none")]),
            padded("<h1>Welcome</h1>", 2_000),
        ),
        (
            "\"Please enable JavaScript\" alone",
            200,
            vec![],
            b"<title>App</title><noscript>Please enable JavaScript</noscript>".to_vec(),
        ),
        ("an empty body", 200, vec![], Vec::new()),
    ];
    for (name, status, headers, body) in clean {
        assert_eq!(detect_challenge(status, &headers, &body), None, "{name}");
    }
}

/// Cloudflare Turnstile on an ordinary page: the script tag the HTML carries, and the widget it
/// renders (what a prerendering service serves bots). Neither is a challenge page.
#[test]
fn a_page_with_a_turnstile_form_is_not_a_cloudflare_challenge() {
    let script = "<script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js\" async defer></script>";
    let widget = "<form><div class=\"cf-turnstile\" data-sitekey=\"0x4AAA\"><div><iframe \
         src=\"https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/b/turnstile/if/ov2/av0/rcv/x\" \
         id=\"cf-chl-widget-a1b2c\"></iframe><input type=\"hidden\" name=\"cf-turnstile-response\" \
         id=\"cf-chl-widget-a1b2c_response\"></div></div></form>";
    for inner in [script, widget] {
        let page = padded(inner, 60 * 1024);
        assert_eq!(detect_challenge(200, &[], &page), None, "{inner}");
    }

    // The baseline carries the script: the probes are still compared with it.
    let mut baseline = html(60_000, Some("Contact us"));
    baseline.challenge = detect_challenge(200, &[], &padded(script, 60 * 1024));
    let probe = html(60_000, Some("Contact us"));
    assert_eq!(
        classify("https://a.example/", &baseline, &probe),
        Verdict::Same
    );

    // A bot gets the prerendered page with the widget, CodoSEObot the script-only HTML.
    let mut prerendered = html(60_000, Some("Contact us"));
    prerendered.challenge = detect_challenge(200, &[], &padded(widget, 60 * 1024));
    assert_eq!(
        classify("https://a.example/", &probe, &prerendered),
        Verdict::Same
    );
}

#[test]
fn headers_win_over_the_body_and_case_does_not_matter() {
    let got = detect_challenge(
        403,
        &h(&[("X-Vercel-Mitigated", "Challenge")]),
        b"<title>Just a moment...</title>",
    );
    assert_eq!(got.map(|c| c.vendor), Some(Vendor::Vercel));
    let got = detect_challenge(403, &[], b"<TITLE>JUST A MOMENT...</TITLE>");
    assert_eq!(got.map(|c| c.vendor), Some(Vendor::Cloudflare));
    // A vendor marker counts on a large page; only the generic ones need a small one.
    let got = detect_challenge(200, &[], &padded("<div id=\"px-captcha\"></div>", 100_000));
    assert_eq!(got.map(|c| c.vendor), Some(Vendor::Human));
}

#[test]
fn only_the_first_64_kib_are_scanned() {
    let mut body = padded("", 70 * 1024);
    body.extend_from_slice(b"<title>Just a moment...</title>");
    assert_eq!(detect_challenge(403, &[], &body), None);
}

#[test]
fn vendor_slugs_and_labels_are_distinct_and_match_serde() {
    let mut slugs = std::collections::HashSet::new();
    for v in VENDORS {
        assert!(slugs.insert(v.slug()), "{v:?}");
        assert!(!v.label().is_empty());
        assert_eq!(
            serde_json::to_value(v).unwrap(),
            serde_json::Value::from(v.slug())
        );
    }
    assert_eq!(Vendor::AwsWaf.slug(), "aws_waf");
    assert_eq!(Vendor::DataDome.slug(), "datadome");
}

#[test]
fn page_titles_are_found_collapsed_and_cut() {
    assert_eq!(
        page_title(b"<html><head><TITLE lang=en>\n  Hello \t  world </TITLE>"),
        Some("Hello world".to_owned())
    );
    assert_eq!(
        page_title(b"<title>Fish &amp; Chips</title>"),
        Some("Fish & Chips".to_owned())
    );
    assert_eq!(page_title(b"<titlebar>x</titlebar>"), None);
    assert_eq!(page_title(b"<title>   </title>"), None);
    assert_eq!(page_title(b"<p>no title</p>"), None);
    assert_eq!(page_title(b"<title>unterminated"), None);
    let long = format!("<title>{}</title>", "é".repeat(300));
    assert_eq!(
        page_title(long.as_bytes()).map(|t| t.chars().count()),
        Some(MAX_TITLE)
    );
}

#[test]
fn a_probe_fetch_round_trips_and_defaults_its_fields() {
    let f = ProbeFetch {
        status: Some(403),
        error: None,
        final_url: Some("https://example.com/blocked".to_owned()),
        redirects: 1,
        bytes: 512,
        truncated: false,
        html: true,
        title: Some("Just a moment...".to_owned()),
        challenge: Some(Challenge {
            vendor: Vendor::Cloudflare,
            signal: "cf-mitigated: challenge".to_owned(),
        }),
        retried: true,
    };
    let json = serde_json::to_string(&f).unwrap();
    assert_eq!(serde_json::from_str::<ProbeFetch>(&json).unwrap(), f);
    let empty: ProbeFetch = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, ProbeFetch::default());
    let err = ProbeFetch {
        error: Some(FetchErrorKind::Timeout),
        ..ProbeFetch::default()
    };
    assert_eq!(
        serde_json::to_string(&err).unwrap(),
        r#"{"error":"timeout"}"#
    );
}

// ---- Verdicts -------------------------------------------------------------------------------

const PAGE: &str = "https://example.com/page";

/// A 2xx HTML fetch of `bytes` bytes titled `title`.
fn html(bytes: u32, title: Option<&str>) -> ProbeFetch {
    ProbeFetch {
        status: Some(200),
        bytes,
        html: true,
        title: title.map(str::to_owned),
        ..ProbeFetch::default()
    }
}

fn status(s: u16) -> ProbeFetch {
    ProbeFetch {
        status: Some(s),
        ..html(2_000, Some("Error"))
    }
}

fn error(kind: FetchErrorKind, retried: bool) -> ProbeFetch {
    ProbeFetch {
        error: Some(kind),
        retried,
        ..ProbeFetch::default()
    }
}

fn challenged(f: ProbeFetch, vendor: Vendor) -> ProbeFetch {
    ProbeFetch {
        challenge: Some(Challenge {
            vendor,
            signal: "marker".to_owned(),
        }),
        ..f
    }
}

fn landed(f: ProbeFetch, url: &str) -> ProbeFetch {
    ProbeFetch {
        final_url: Some(url.to_owned()),
        redirects: 1,
        ..f
    }
}

fn inconclusive(reason: InconclusiveReason) -> Verdict {
    Verdict::Inconclusive { reason }
}

#[test]
fn classify_follows_the_precedence() {
    use InconclusiveReason::*;
    let base = html(10_000, Some("Blue Widgets | Shop"));
    let same = || html(10_000, Some("Blue Widgets | Shop"));
    // (name, requested, baseline, probe, expected)
    let cases: Vec<(&str, &str, ProbeFetch, ProbeFetch, Verdict)> = vec![
        // 1. The baseline must be a clean 2xx HTML page.
        (
            "baseline 503",
            PAGE,
            status(503),
            status(403),
            inconclusive(BaselineFailed),
        ),
        (
            "baseline not HTML",
            PAGE,
            ProbeFetch {
                html: false,
                ..base.clone()
            },
            same(),
            inconclusive(BaselineFailed),
        ),
        (
            "baseline timed out",
            PAGE,
            error(FetchErrorKind::Timeout, true),
            same(),
            inconclusive(BaselineFailed),
        ),
        (
            "baseline challenged by a vendor, probe too",
            PAGE,
            challenged(base.clone(), Vendor::DataDome),
            challenged(same(), Vendor::Cloudflare),
            inconclusive(BaselineChallenged),
        ),
        (
            "baseline a 2xx page with a generic marker only is not challenged",
            PAGE,
            challenged(base.clone(), Vendor::Generic),
            same(),
            Verdict::Same,
        ),
        (
            "baseline a non-2xx page with a generic marker",
            PAGE,
            challenged(status(403), Vendor::Generic),
            same(),
            inconclusive(BaselineChallenged),
        ),
        (
            "baseline a challenged 403",
            PAGE,
            challenged(status(403), Vendor::Cloudflare),
            same(),
            inconclusive(BaselineChallenged),
        ),
        // 2. Probe errors.
        (
            "probe timed out after the retry",
            PAGE,
            base.clone(),
            error(FetchErrorKind::Timeout, true),
            inconclusive(ProbeError),
        ),
        (
            "connection refused twice",
            PAGE,
            base.clone(),
            error(FetchErrorKind::Connect, true),
            Verdict::Blocked { status: None },
        ),
        (
            "connection reset twice",
            PAGE,
            base.clone(),
            error(FetchErrorKind::Dropped, true),
            Verdict::Blocked { status: None },
        ),
        (
            "connection reset once, not retried",
            PAGE,
            base.clone(),
            error(FetchErrorKind::Dropped, false),
            inconclusive(ProbeError),
        ),
        (
            "too many redirects",
            PAGE,
            base.clone(),
            error(FetchErrorKind::TooManyRedirects, false),
            inconclusive(ProbeError),
        ),
        (
            "SSRF guard",
            PAGE,
            base.clone(),
            error(FetchErrorKind::Blocked, false),
            inconclusive(ProbeError),
        ),
        (
            "no status and no error",
            PAGE,
            base.clone(),
            ProbeFetch::default(),
            inconclusive(ProbeError),
        ),
        // 3. A challenge in the probe only, whatever the status.
        (
            "Cloudflare challenge on a 403",
            PAGE,
            base.clone(),
            challenged(status(403), Vendor::Cloudflare),
            Verdict::Challenged {
                vendor: Vendor::Cloudflare,
            },
        ),
        (
            "challenge wins over throttle",
            PAGE,
            base.clone(),
            challenged(status(429), Vendor::Vercel),
            Verdict::Challenged {
                vendor: Vendor::Vercel,
            },
        ),
        (
            "challenge on a 200",
            PAGE,
            base.clone(),
            challenged(same(), Vendor::Human),
            Verdict::Challenged {
                vendor: Vendor::Human,
            },
        ),
        // A generic marker on both sides is compared away, not a challenge; what else differs
        // still counts.
        (
            "generic marker on both, same page",
            PAGE,
            challenged(base.clone(), Vendor::Generic),
            challenged(same(), Vendor::Generic),
            Verdict::Same,
        ),
        (
            "generic marker on both, probe gets 403",
            PAGE,
            challenged(base.clone(), Vendor::Generic),
            challenged(status(403), Vendor::Generic),
            Verdict::Blocked { status: Some(403) },
        ),
        (
            "generic marker on both, probe throttled",
            PAGE,
            challenged(base.clone(), Vendor::Generic),
            challenged(status(429), Vendor::Generic),
            Verdict::Throttled,
        ),
        (
            "generic marker on both, probe diverted",
            PAGE,
            challenged(base.clone(), Vendor::Generic),
            challenged(
                landed(html(9_000, Some("Shop")), "https://example.com/"),
                Vendor::Generic,
            ),
            Verdict::Diverted {
                to: "https://example.com/".to_owned(),
            },
        ),
        (
            "generic marker on both, probe degraded",
            PAGE,
            challenged(base.clone(), Vendor::Generic),
            challenged(html(3_000, Some("Please wait")), Vendor::Generic),
            Verdict::Degraded,
        ),
        (
            "generic marker on the baseline, vendor challenge on the probe",
            PAGE,
            challenged(base.clone(), Vendor::Generic),
            challenged(status(403), Vendor::Cloudflare),
            Verdict::Challenged {
                vendor: Vendor::Cloudflare,
            },
        ),
        (
            "generic marker on the baseline, 403 without a marker for the probe",
            PAGE,
            challenged(base.clone(), Vendor::Generic),
            status(403),
            Verdict::Blocked { status: Some(403) },
        ),
        (
            "generic marker on the probe only",
            PAGE,
            base.clone(),
            challenged(status(403), Vendor::Generic),
            Verdict::Challenged {
                vendor: Vendor::Generic,
            },
        ),
        // 4. Throttled.
        ("429", PAGE, base.clone(), status(429), Verdict::Throttled),
        // 5. Blocked.
        (
            "403",
            PAGE,
            base.clone(),
            status(403),
            Verdict::Blocked { status: Some(403) },
        ),
        (
            "404 against a 200 baseline",
            PAGE,
            base.clone(),
            status(404),
            Verdict::Blocked { status: Some(404) },
        ),
        (
            "503",
            PAGE,
            base.clone(),
            status(503),
            Verdict::Blocked { status: Some(503) },
        ),
        // 6. Diverted.
        (
            "redirected home",
            PAGE,
            base.clone(),
            landed(html(9_000, Some("Shop")), "https://example.com/"),
            Verdict::Diverted {
                to: "https://example.com/".to_owned(),
            },
        ),
        (
            "baseline redirected, probe not",
            PAGE,
            landed(base.clone(), "https://example.com/page2"),
            same(),
            Verdict::Diverted {
                to: PAGE.to_owned(),
            },
        ),
        (
            "an http to https upgrade",
            "http://example.com/page",
            base.clone(),
            landed(same(), PAGE),
            Verdict::Same,
        ),
        (
            "a trailing slash",
            PAGE,
            base.clone(),
            landed(same(), "https://example.com/page/"),
            Verdict::Same,
        ),
        (
            "a fragment",
            PAGE,
            base.clone(),
            landed(same(), "https://example.com/page#top"),
            Verdict::Same,
        ),
        (
            "both redirected to the same place",
            PAGE,
            landed(base.clone(), "https://example.com/new"),
            landed(same(), "https://example.com/new"),
            Verdict::Same,
        ),
        (
            "another host",
            PAGE,
            base.clone(),
            landed(same(), "https://blocked.example.net/page"),
            Verdict::Diverted {
                to: "https://blocked.example.net/page".to_owned(),
            },
        ),
        // 7. Degraded: a different, much smaller page, or one without a title.
        (
            "different title at 30% size",
            PAGE,
            base.clone(),
            html(3_000, Some("Please wait")),
            Verdict::Degraded,
        ),
        (
            "no title where the baseline has one",
            PAGE,
            base.clone(),
            html(10_000, None),
            Verdict::Degraded,
        ),
        // 8. Same.
        ("identical", PAGE, base.clone(), same(), Verdict::Same),
        (
            "same title at 40% size",
            PAGE,
            base.clone(),
            html(4_000, Some("Blue Widgets | Shop")),
            Verdict::Same,
        ),
        (
            "same title in another case and spacing at 30% size",
            PAGE,
            base.clone(),
            html(3_000, Some("blue widgets |  SHOP")),
            Verdict::Same,
        ),
        (
            "different title at 60% size",
            PAGE,
            base.clone(),
            html(6_000, Some("Blue Widgets – Shop")),
            Verdict::Same,
        ),
        (
            "bigger page for bots",
            PAGE,
            base.clone(),
            html(40_000, Some("Blue Widgets | Shop (prerendered)")),
            Verdict::Same,
        ),
        (
            "neither has a title",
            PAGE,
            html(300, None),
            html(100, None),
            Verdict::Same,
        ),
    ];
    for (name, requested, baseline, probe, want) in cases {
        assert_eq!(classify(requested, &baseline, &probe), want, "{name}");
    }
}

#[test]
fn verdicts_name_their_cause() {
    let challenge = |v: Vendor| Verdict::Challenged { vendor: v };
    let blocked = |s: Option<u16>| Verdict::Blocked { status: s };
    let cases: Vec<(Verdict, Option<&str>)> = vec![
        (Verdict::Same, None),
        (Verdict::SkippedRobots, None),
        (inconclusive(InconclusiveReason::Unstable), None),
        (inconclusive(InconclusiveReason::ProbeError), None),
        (blocked(None), Some("blocked_dropped")),
        (blocked(Some(401)), Some("blocked_401")),
        (blocked(Some(403)), Some("blocked_403")),
        (blocked(Some(404)), Some("blocked_404")),
        (blocked(Some(451)), Some("blocked_451")),
        (blocked(Some(400)), Some("blocked_4xx")),
        (blocked(Some(410)), Some("blocked_4xx")),
        (blocked(Some(500)), Some("blocked_5xx")),
        (blocked(Some(503)), Some("blocked_5xx")),
        (
            Verdict::Diverted {
                to: "https://example.com/".to_owned(),
            },
            Some("diverted"),
        ),
        (Verdict::Degraded, Some("degraded")),
        (Verdict::Throttled, Some("throttled")),
    ];
    for (v, slug) in cases {
        assert_eq!(v.cause_slug(), slug, "{v:?}");
        assert_eq!(v.is_positive(), slug.is_some(), "{v:?}");
    }
    for v in VENDORS {
        let c = challenge(v);
        assert!(c.is_positive());
        assert_eq!(
            c.cause_slug(),
            Some(format!("challenge_{}", v.slug())).as_deref()
        );
    }
}

#[test]
fn verdicts_serialise_with_a_kind() {
    let cases = [
        (Verdict::Same, r#"{"kind":"same"}"#),
        (
            Verdict::Blocked { status: Some(403) },
            r#"{"kind":"blocked","status":403}"#,
        ),
        (
            Verdict::Blocked { status: None },
            r#"{"kind":"blocked","status":null}"#,
        ),
        (
            Verdict::Challenged {
                vendor: Vendor::AwsWaf,
            },
            r#"{"kind":"challenged","vendor":"aws_waf"}"#,
        ),
        (
            inconclusive(InconclusiveReason::BaselineChallenged),
            r#"{"kind":"inconclusive","reason":"baseline_challenged"}"#,
        ),
        (Verdict::SkippedRobots, r#"{"kind":"skipped_robots"}"#),
    ];
    for (v, json) in cases {
        assert_eq!(serde_json::to_string(&v).unwrap(), json);
        assert_eq!(serde_json::from_str::<Verdict>(json).unwrap(), v);
    }
}

// ---- Candidates and the plan ----------------------------------------------------------------

fn skip_of<'a>(skipped: &'a [SkippedBot], token: &str) -> Option<&'a SkipReason> {
    skipped.iter().find(|s| s.token == token).map(|s| &s.reason)
}

#[test]
fn candidates_are_search_and_user_fetch_bots_with_a_documented_user_agent() {
    let (bots, skipped) = candidates();
    let want: Vec<&str> = registry()
        .bots
        .iter()
        .filter(|b| is_probe_candidate(b) && b.user_agent.is_some())
        .map(|b| b.token.as_str())
        .collect();
    let got: Vec<&str> = bots.iter().map(|b| b.token.as_str()).collect();
    assert_eq!(got, want);
    assert!(got.contains(&"OAI-SearchBot") && got.contains(&"ChatGPT-User"));
    for b in &bots {
        let reg = registry().bot(&b.token).unwrap();
        assert_eq!(reg.user_agent.as_deref(), Some(b.user_agent.as_str()));
        assert!(matches!(reg.purpose, Purpose::Search | Purpose::UserFetch));
    }
    for t in [
        "Claude-SearchBot",
        "Claude-User",
        "Amzn-SearchBot",
        "Amzn-User",
    ] {
        assert_eq!(skip_of(&skipped, t), Some(&SkipReason::NoUserAgent), "{t}");
    }
    for t in NEVER_PROBE {
        assert_eq!(
            skip_of(&skipped, t),
            Some(&SkipReason::VerifiedSearchEngine),
            "{t}"
        );
        assert!(!is_probe_candidate(registry().bot(t).unwrap()));
    }
    for t in ["Google-Agent", "GPTBot", "OAI-AdsBot"] {
        assert_eq!(
            skip_of(&skipped, t),
            Some(&SkipReason::NotProbedPurpose),
            "{t}"
        );
    }
    // Control tokens never make requests: neither probed nor listed.
    for t in ["Google-Extended", "Applebot-Extended"] {
        assert!(!got.contains(&t) && skip_of(&skipped, t).is_none(), "{t}");
    }
    // Every crawling bot is accounted for exactly once.
    let crawling = registry().bots.iter().filter(|b| b.crawls).count();
    assert_eq!(bots.len() + skipped.len(), crawling);
}

fn report_for(out: &CrawlOutput, starred: &[&str]) -> AccessReport {
    let starred: HashSet<u64> = starred.iter().map(|p| url_hash(&url(p))).collect();
    let important = important_urls(&out.pages, &out.origin, &starred);
    build_report(out, &important)
}

fn index_of(report: &AccessReport, path: &str) -> u16 {
    let want = url(path).to_string();
    report
        .important
        .iter()
        .position(|i| i.url == want)
        .expect(path) as u16
}

#[test]
fn the_plan_takes_the_home_page_then_starred_then_most_linked() {
    // Most linked: /a, /b, /c, /d; /d is starred.
    let out = crawl(vec![
        page("/", 1),
        page("/a", 40),
        page("/b", 30),
        page("/c", 20),
        page("/d", 10),
    ]);
    let r = report_for(&out, &["/d"]);
    let plan = plan_probes(&r, None, 10).expect("plan");
    let want: Vec<u16> = ["/", "/d", "/a", "/b", "/c"]
        .iter()
        .map(|p| index_of(&r, p))
        .collect();
    assert_eq!(plan.urls, want);

    let plan = plan_probes(&r, None, 3).expect("plan");
    assert_eq!(plan.urls, want[..3]);
    let plan = plan_probes(&r, None, 1).expect("plan");
    assert_eq!(plan.urls, [0]);
}

#[test]
fn the_plan_skips_pages_that_are_not_html() {
    let mut json = page("/feed", 50);
    json.content_type = Some("application/json".to_owned());
    let out = crawl(vec![
        page("/", 1),
        json,
        page("/gone", 40).status(404),
        page("/ok", 30),
    ]);
    let r = report_for(&out, &[]);
    let plan = plan_probes(&r, None, 5).expect("plan");
    assert_eq!(plan.urls, [0, index_of(&r, "/ok")]);
}

#[test]
fn no_plan_without_verdicts_urls_or_pages() {
    let out = site(3, |p| p);
    assert!(plan_probes(&report_for(&out, &[]), None, 0).is_none());

    let unavailable = with_robots(site(3, |p| p), 503, "");
    assert!(plan_probes(&report_for(&unavailable, &[]), None, 5).is_none());
    let mut unknown = site(3, |p| p);
    unknown.robots = None;
    assert!(plan_probes(&report_for(&unknown, &[]), None, 5).is_none());

    let broken = crawl(vec![page("/", 5).status(500), page("/x", 4).status(404)]);
    assert!(plan_probes(&report_for(&broken, &[]), None, 5).is_none());

    // A missing robots.txt allows everything: probes run.
    let missing = with_robots(site(3, |p| p), 404, "");
    let plan = plan_probes(&report_for(&missing, &[]), None, 5).expect("plan");
    assert_eq!(plan.crawl_delay, None);
}

#[test]
fn the_plan_probes_the_candidates_only() {
    let r = report_for(&site(2, |p| p), &[]);
    let plan = plan_probes(&r, None, 5).expect("plan");
    let (bots, skipped) = candidates();
    assert_eq!(plan.bots, bots);
    assert_eq!(plan.skipped_bots, skipped);
    for b in &plan.bots {
        let reg = registry().bot(&b.token).unwrap();
        assert!(reg.crawls && reg.purpose != Purpose::Agent, "{}", b.token);
        assert!(
            !NEVER_PROBE.iter().any(|n| n.eq_ignore_ascii_case(&b.token)),
            "{}",
            b.token
        );
    }
}

#[test]
fn the_plan_waits_the_longest_crawl_delay_of_its_bots_and_codoseobot() {
    let plan_with = |body: &str| {
        let out = with_robots(site(2, |p| p), 200, body);
        let r = report_for(&out, &[]);
        let robots = RobotsTxt::parse(body.as_bytes());
        plan_probes(&r, Some(&robots), 5).expect("plan").crawl_delay
    };
    assert_eq!(plan_with("User-agent: *\nAllow: /\n"), None);
    assert_eq!(
        plan_with(
            "User-agent: *\nCrawl-delay: 2\n\nUser-agent: PerplexityBot\nCrawl-delay: 5\nAllow: /\n"
        ),
        Some(5.0)
    );
    assert_eq!(
        plan_with("User-agent: CodoSEObot\nCrawl-delay: 7\n\nUser-agent: *\nCrawl-delay: 1\n"),
        Some(7.0)
    );
    // A bot that is not probed doesn't slow the probes down.
    assert_eq!(
        plan_with("User-agent: GPTBot\nCrawl-delay: 30\nDisallow: /\n"),
        None
    );
}

#[test]
fn robots_skips_reads_the_reports_verdicts() {
    let out = with_robots(
        site(3, |p| p),
        200,
        "User-agent: PerplexityBot\nDisallow: /p1\n",
    );
    let r = report_for(&out, &[]);
    let p1 = index_of(&r, "/p1");
    assert!(robots_skips(&r, p1, "PerplexityBot"));
    assert!(robots_skips(&r, p1, "perplexitybot"));
    assert!(!robots_skips(&r, 0, "PerplexityBot"));
    assert!(!robots_skips(&r, p1, "OAI-SearchBot"));
    assert!(!robots_skips(&r, p1, "NoSuchBot"));
}

// ---- The probe report -----------------------------------------------------------------------

fn result(token: &str, verdict: Verdict) -> ProbeResult {
    ProbeResult {
        token: token.to_owned(),
        fetch: (verdict != Verdict::SkippedRobots).then(|| html(1_000, Some("Shop"))),
        verdict,
    }
}

fn probe_report(results: Vec<ProbeResult>) -> ProbeReport {
    let r = report_for(&site(2, |p| p), &[]);
    let plan = plan_probes(&r, None, 1).expect("plan");
    let mut report = ProbeReport::new("hello@codoseo.com", &plan);
    report.urls.push(ProbedUrl {
        url: 0,
        baseline: html(1_000, Some("Shop")),
        results,
    });
    report.requests = 3;
    report
}

#[test]
fn a_probe_report_is_conclusive_only_with_a_real_verdict() {
    let r = probe_report(vec![
        result(
            "OAI-SearchBot",
            inconclusive(InconclusiveReason::BaselineFailed),
        ),
        result("PerplexityBot", Verdict::SkippedRobots),
    ]);
    assert!(!r.conclusive());
    let r = probe_report(vec![
        result("OAI-SearchBot", inconclusive(InconclusiveReason::Unstable)),
        result("PerplexityBot", Verdict::Same),
    ]);
    assert!(r.conclusive());
    assert!(probe_report(vec![result("OAI-SearchBot", Verdict::Throttled)]).conclusive());
    assert!(!probe_report(vec![]).conclusive());
}

#[test]
fn a_probe_report_is_synthetic_and_round_trips() {
    let mut r = probe_report(vec![
        result(
            "OAI-SearchBot",
            Verdict::Challenged {
                vendor: Vendor::Cloudflare,
            },
        ),
        result("PerplexityBot", Verdict::SkippedRobots),
    ]);
    r.stop = Some(ProbeStop::Budget);
    assert!(r.synthetic);
    assert_eq!(r.version, PROBE_VERSION);
    assert_eq!(r.from, "hello@codoseo.com");
    let (bots, skipped) = candidates();
    assert_eq!(
        r.bots,
        bots.iter().map(|b| b.token.clone()).collect::<Vec<_>>()
    );
    assert_eq!(r.skipped_bots, skipped);

    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["synthetic"], true);
    assert_eq!(json["stop"], "budget");
    // A robots skip carries no fetch.
    assert!(json["urls"][0]["results"][1].get("fetch").is_none());
    let back: ProbeReport = serde_json::from_value(json).unwrap();
    assert_eq!(back, r);
}

#[test]
fn access_reports_carry_probes_and_old_ones_still_load() {
    let mut r = report_for(&site(2, |p| p), &[]);
    assert_eq!(r.probes, None);
    let json = serde_json::to_value(&r).unwrap();
    assert!(
        json.get("probes").is_none(),
        "absent probes are not written"
    );

    let old = r#"{"version":1,"registry_version":1,"robots":{"status":200,"availability":"ok"},"important":[{"url":"https://example.com/","reason":"home","status":200,"html_ok":true}],"bots":[],"engines":[]}"#;
    let old: AccessReport = serde_json::from_str(old).expect("old report");
    assert_eq!(old.probes, None);

    r.probes = Some(probe_report(vec![result("OAI-SearchBot", Verdict::Same)]));
    let back: AccessReport = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back, r);
}
