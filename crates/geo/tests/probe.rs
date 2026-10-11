//! The synthetic fetch probes' pure half: challenge detection and page titles.

use codoseo_geo::probe::{
    Challenge, FetchErrorKind, MAX_TITLE, ProbeFetch, Vendor, detect_challenge, page_title,
};

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
            Vendor::Cloudflare,
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
            Vendor::Cloudflare,
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
