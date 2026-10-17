//! The synthetic probe false-positive corpus, B1's exit criterion: fake sites that answer the way
//! real servers, CDNs and bot managers answer AI bots, probed end to end with every candidate bot,
//! and each (fixture, bot) pair scored as a true or false positive or negative. The test prints a
//! table and the totals and fails on any false positive, false negative or wrong cause.
//!
//! Run it with `cargo test -p codoseo-crawler --test probe_corpus -- --nocapture` to see the table.
//!
//! The fixtures copy the status codes, header names and page snippets the vendors really send, so
//! a fixture passes because the detector reads real responses, not because the fixture repeats the
//! detector's strings. The fixtures run concurrently on the real clock with short gaps: a paused
//! tokio clock fires the client's timeouts against local sockets (see `tests/probe.rs`).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use codoseo_core::check::IssueBits;
use codoseo_core::crawl::{AddressPolicy, RobotsFile, SitemapSummary};
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, PageFields, PageRecord};
use codoseo_core::url::url_hash;
use codoseo_crawler::probe::{ProbeConfig, run_probes};
use codoseo_geo::probe::{ProbeReport, Verdict, candidates, plan_probes};
use codoseo_geo::report::{build_report, important_urls};
use codoseo_testkit::{Page, SiteBuilder, TestSite};
use tokio::task::JoinSet;

// The nine probed tokens. Claude-SearchBot, Claude-User, Amzn-SearchBot and Amzn-User have no
// documented full user agent, so they are never probed and can't be targets here.
const OAI: &str = "OAI-SearchBot";
const CHATGPT: &str = "ChatGPT-User";
const PPLX: &str = "PerplexityBot";
const PPLX_USER: &str = "Perplexity-User";
const META_INDEX: &str = "meta-webindexer";
const META_FETCH: &str = "meta-externalfetcher";
const DUCK: &str = "DuckAssistBot";
const MISTRAL_INDEX: &str = "MistralAI-Index";
const MISTRAL_USER: &str = "MistralAI-User";
const ALL_AI: [&str; 9] = [
    OAI,
    CHATGPT,
    PPLX,
    PPLX_USER,
    META_INDEX,
    META_FETCH,
    DUCK,
    MISTRAL_INDEX,
    MISTRAL_USER,
];

/// What a fixture expects for one bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// A positive verdict whose cause slug starts with this.
    Problem(&'static str),
    /// No positive verdict: same, inconclusive or skipped by robots.txt.
    Fine,
    /// The site treats the real bot differently in a way a synthetic probe can't see. Listed, not
    /// scored.
    Undetectable,
}

struct Fixture {
    name: &'static str,
    site: fn() -> SiteBuilder,
    expect: fn(&str) -> Expect,
    /// The page probed.
    path: &'static str,
    /// The robots.txt the crawl saw.
    robots: &'static str,
}

const ALLOW_ALL: &str = "User-agent: *\nAllow: /\n";

/// `cause` for the `targets`, `Fine` for every other bot.
fn only(targets: &[&str], token: &str, cause: &'static str) -> Expect {
    if targets.contains(&token) {
        Expect::Problem(cause)
    } else {
        Expect::Fine
    }
}

fn fine(_: &str) -> Expect {
    Expect::Fine
}

// ---- Pages ----------------------------------------------------------------------------------

const LOREM: &str = "Our team ships hand-made ceramic cookware from a small workshop in Porto. \
    Every piece is thrown, glazed and fired on site, then tested on gas, electric and induction \
    hobs before it leaves. This page covers care, sizing and the warranty in full detail. ";

/// An ordinary article page with `paragraphs` paragraphs of text, about 230 bytes each.
fn article(title: &str, paragraphs: usize) -> String {
    let body: String = (0..paragraphs)
        .map(|i| format!("<p id=\"p{i}\">{LOREM}</p>\n"))
        .collect();
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n\
         <meta name=\"description\" content=\"Hand-made ceramic cookware from Porto.\">\n\
         <link rel=\"stylesheet\" href=\"/assets/site.css\">\n</head>\n<body>\n\
         <header><nav><a href=\"/\">Home</a> <a href=\"/shop\">Shop</a> \
         <a href=\"/contact\">Contact</a></nav></header>\n<main>\n<h1>{title}</h1>\n{body}\
         </main>\n<footer>&copy; 2026 Porto Ceramics</footer>\n</body>\n</html>\n"
    )
}

const HOME_TITLE: &str = "Porto Ceramics | Hand-made cookware";

fn home() -> Page {
    Page::html(&article(HOME_TITLE, 24))
}

/// `page` for the bots whose token the user agent contains, `default` for everyone else.
fn for_bots(tokens: &[&str], page: Page, default: Page) -> Page {
    Page::by_user_agent(tokens.iter().map(|t| (*t, page.clone())).collect(), default)
}

/// nginx's stock error page.
fn nginx_error(status: u16, reason: &str) -> Page {
    Page::status(
        status,
        &format!(
            "<html>\r\n<head><title>{status} {reason}</title></head>\r\n<body>\r\n\
             <center><h1>{status} {reason}</h1></center>\r\n<hr><center>nginx</center>\r\n\
             </body>\r\n</html>\r\n"
        ),
    )
    .header("server", "nginx")
}

/// A normal page as served through Cloudflare: Cloudflare adds its own headers to every response.
fn cloudflare_ok() -> Page {
    home()
        .header("server", "cloudflare")
        .header("cf-ray", "8d1f2a3b4c5d6e7f-AMS")
        .header("cf-cache-status", "DYNAMIC")
}

/// Cloudflare's managed challenge interstitial.
fn cloudflare_challenge(status: u16) -> Page {
    let body = r#"<!DOCTYPE html><html lang="en-US"><head><title>Just a moment...</title><meta http-equiv="Content-Type" content="text/html; charset=UTF-8"><meta http-equiv="X-UA-Compatible" content="IE=Edge"><meta name="robots" content="noindex,nofollow"><meta name="viewport" content="width=device-width,initial-scale=1"><style>*{box-sizing:border-box;margin:0;padding:0}html{line-height:1.15;-webkit-text-size-adjust:100%;color:#313131;font-family:system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial}</style><meta http-equiv="refresh" content="390"></head><body class="no-js"><div class="main-wrapper" role="main"><div class="main-content"><noscript><div class="h2"><span id="challenge-error-text">Enable JavaScript and cookies to continue</span></div></noscript></div></div><script>(function(){window._cf_chl_opt={cvId: '3',cZone: "www.portoceramics.example",cType: 'managed',cRay: '8d1f2a3b4c5d6e80',cH: 'Qm9ndXNIYXNoVmFsdWU',cUPMDTk: "\/?__cf_chl_tk=Zm9vYmFy-1728640000-1.0.1.1-YmF6",cFPWv: 'g',cITimeS: '1728640000',cTTimeMs: '1000',cMTimeMs: '390000',cTplC: 0,cTplV: 5,cTplB: 'cf',cK: "",fa: "\/?__cf_chl_f_tk=Zm9vYmFy-1728640000-1.0.1.1-YmF6",md: 'bWQ',mdrd: 'bWRyZA'};var cpo = document.createElement('script');cpo.src = '/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1?ray=8d1f2a3b4c5d6e80';window._cf_chl_opt.cOgUHash = location.hash === '' && location.href.indexOf('#') !== -1 ? '#' : location.hash;var a = document.getElementsByTagName('head')[0];a.appendChild(cpo);}());</script></body></html>"#;
    Page::status(status, body)
        .header("server", "cloudflare")
        .header("cf-ray", "8d1f2a3b4c5d6e80-AMS")
        .header("cf-mitigated", "challenge")
        .header(
            "cache-control",
            "private, max-age=0, no-store, no-cache, must-revalidate, post-check=0, pre-check=0",
        )
        .header("expires", "Thu, 01 Jan 1970 00:00:01 GMT")
        .header("cross-origin-embedder-policy", "require-corp")
        .header("referrer-policy", "same-origin")
        .header("x-frame-options", "SAMEORIGIN")
}

/// Cloudflare's "Access denied" block page for a WAF or AI-bot rule (error 1020). The error number
/// and the word "Error" sit in separate elements, as on the real page.
fn cloudflare_1020() -> Page {
    let body = r#"<!DOCTYPE html>
<!--[if lt IE 7]> <html class="no-js ie6 oldie" lang="en-US"> <![endif]-->
<!--[if gt IE 8]><!--> <html class="no-js" lang="en-US"> <!--<![endif]-->
<head>
<title>Access denied | www.portoceramics.example used Cloudflare to restrict access</title>
<meta charset="UTF-8" />
<meta http-equiv="Content-Type" content="text/html; charset=UTF-8" />
<meta http-equiv="X-UA-Compatible" content="IE=Edge" />
<meta name="robots" content="noindex, nofollow" />
<meta name="viewport" content="width=device-width,initial-scale=1" />
<link rel="stylesheet" id="cf_styles-css" href="/cdn-cgi/styles/main.css" />
</head>
<body>
<div id="cf-wrapper">
    <div class="cf-alert cf-alert-error cf-cookie-error hidden" id="cookie-alert" data-translate="enable_cookies">Please enable cookies.</div>
    <div id="cf-error-details" class="p-0">
        <header class="mx-auto pt-10 lg:pt-6 lg:px-8 w-240 lg:w-full mb-15 antialiased">
            <h1 class="inline-block md:block mr-2 md:mb-2 font-light text-60 md:text-3xl text-black-dark leading-tight">
              <span data-translate="error">Error</span>
              <span>1020</span>
            </h1>
            <span class="inline-block md:block heading-ray-id font-mono text-15 lg:text-sm lg:leading-relaxed">Ray ID: 8d1f2a3b4c5d6e81 &bull;</span>
            <span class="inline-block md:block heading-ray-id font-mono text-15 lg:text-sm lg:leading-relaxed">2026-10-11 09:12:44 UTC</span>
            <h2 class="text-gray-600 leading-1.3 text-3xl lg:text-2xl font-light">Access denied</h2>
        </header>
        <section class="w-240 lg:w-full mx-auto mb-8 lg:px-8">
            <div id="what-happened-section" class="w-1/2 md:w-full">
                <h2 class="text-3xl leading-tight font-normal mb-4 text-black-dark antialiased" data-translate="what_happened">What happened?</h2>
                <p>This website is using a security service to protect itself from online attacks.</p>
            </div>
        </section>
        <div class="cf-error-footer cf-wrapper w-240 lg:w-full py-10 sm:py-4 sm:px-8 mx-auto text-center sm:text-left border-solid border-0 border-t border-gray-300">
            <p class="text-13">
                <span class="cf-footer-item sm:block sm:mb-1">Cloudflare Ray ID: <strong class="font-semibold">8d1f2a3b4c5d6e81</strong></span>
                <span class="cf-footer-separator sm:hidden">&bull;</span>
                <span id="cf-footer-item-ip" class="cf-footer-item hidden sm:block sm:mb-1">Your IP: <button type="button" id="cf-footer-ip-reveal" class="cf-footer-ip-reveal-btn">Click to reveal</button></span>
                <span class="cf-footer-item sm:block sm:mb-1"><span>Performance &amp; security by</span> <a rel="noopener noreferrer" href="https://www.cloudflare.com/5xx-error-landing" id="brand_link" target="_blank">Cloudflare</a></span>
            </p>
        </div>
    </div>
</div>
</body>
</html>
"#;
    Page::status(403, body)
        .header("server", "cloudflare")
        .header("cf-ray", "8d1f2a3b4c5d6e81-AMS")
        .header("referrer-policy", "same-origin")
        .header("x-frame-options", "SAMEORIGIN")
}

/// A normal page behind DataDome: it adds its header and cookie to the traffic it lets through.
fn datadome_ok() -> Page {
    home()
        .header("x-datadome", "protected")
        .header("accept-ch", "Sec-CH-UA,Sec-CH-UA-Mobile,Sec-CH-UA-Platform,Sec-CH-UA-Arch")
        .header(
            "set-cookie",
            "datadome=AHrlqAAAAAMAbcdEFGhijKLMnop~~; Max-Age=31536000; Domain=.portoceramics.example; Path=/; Secure; SameSite=Lax",
        )
}

/// DataDome's block response: a 403 with a script that loads its CAPTCHA.
fn datadome_block() -> Page {
    let body = r#"<html lang="en"><head><title>portoceramics.example</title><style>#cmsg{animation: A 1.5s;}@keyframes A{0%{opacity:0;}99%{opacity:0;}100%{opacity:1;}}</style></head><body style="margin:0"><p id="cmsg">Please enable JS and disable any ad blocker</p><script data-cfasync="false">var dd={'rt':'c','cid':'AHrlqAAAAAMAbcdEFGhijKLMnop==','hsh':'2211F522B61E269B869FA6EAFFB5E1','t':'bv','s':45123,'e':'b5e2f0a1c3d4e5f60718293a4b5c6d7e8f','host':'geo.captcha-delivery.com','cookie':'zyxwVUTsrq~~'}</script><script data-cfasync="false" src="https://ct.captcha-delivery.com/c.js"></script></body></html>"#;
    Page::status(403, body)
        .header("x-datadome", "protected")
        .header("x-datadome-cid", "AHrlqAAAAAMAbcdEFGhijKLMnop==")
        .header("x-dd-b", "1")
        .header("cache-control", "max-age=0, private, no-cache, no-store, must-revalidate")
        .header(
            "set-cookie",
            "datadome=zyxwVUTsrq~~; Max-Age=31536000; Domain=.portoceramics.example; Path=/; Secure; SameSite=Lax",
        )
}

/// HUMAN (PerimeterX) "Press & Hold" block page.
fn human_press_hold() -> Page {
    let body = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta name="description" content="px-captcha">
    <title>Access to this page has been denied</title>
    <style>.px-captcha-error-header{color:#f0f1f2;font-size:29px;margin:67px 0 33px;font-weight:700;line-height:1.24}.px-captcha-message{color:#f0f1f2;font-size:16px}</style>
</head>
<body>
<div class="px-captcha-container">
    <div class="px-captcha-header">Before we continue...</div>
    <div class="px-captcha-message">Press &amp; Hold to confirm you are<br>a human (and not a bot).</div>
    <div id="px-captcha"></div>
    <p class="px-captcha-refid">Reference ID 8a2f1c3e-87d1-11ef-b2c4-5f6e7d8c9b0a</p>
</div>
<script>
    window._pxVid = '';
    window._pxUuid = '8a2f1c3e-87d1-11ef-b2c4-5f6e7d8c9b0a';
    window._pxAppId = 'PXa1B2c3D4';
    window._pxHostUrl = '/a1B2c3D4/xhr';
    window._pxCustomLogo = '';
    window._pxJsClientSrc = '/a1B2c3D4/init.js';
    window._pxFirstPartyEnabled = true;
    var script = document.createElement('script');
    script.src = '/a1B2c3D4/captcha/captcha.js?a=c&u=8a2f1c3e-87d1-11ef-b2c4-5f6e7d8c9b0a&v=&m=0';
    document.head.appendChild(script);
</script>
</body>
</html>
"#;
    Page::html(body).header(
        "cache-control",
        "private, no-cache, no-store, must-revalidate",
    )
}

/// Akamai's "Access Denied" edge page, with its character-reference escaping.
fn akamai_denied() -> Page {
    let body = "<HTML><HEAD>\n<TITLE>Access Denied</TITLE>\n</HEAD><BODY>\n<H1>Access Denied</H1>\n \n\
        You don't have permission to access \"http&#58;&#47;&#47;www&#46;portoceramics&#46;example&#47;\" on this server.<P>\n\
        Reference&#32;&#35;18&#46;2d6e2017&#46;1728640000&#46;1a2b3c4d\n\
        <P>https&#58;&#47;&#47;errors&#46;edgesuite&#46;net&#47;18&#46;2d6e2017&#46;1728640000&#46;1a2b3c4d</P>\n\
        </BODY>\n</HTML>\n";
    Page::status(403, body)
        .header("server", "AkamaiGHost")
        .header("mime-version", "1.0")
        .header("expires", "Fri, 11 Oct 2026 09:12:44 GMT")
}

/// A normal page behind Imperva: its `x-iinfo` and `x-cdn` headers and cookies are on every
/// response.
fn imperva_ok() -> Page {
    home()
        .header("x-cdn", "Imperva")
        .header("x-iinfo", "12-34567890-34567891 NNNN CT(1 2 0) RT(1728640000000 3) q(0 0 0 -1) r(1 1) U6")
        .header(
            "set-cookie",
            "visid_incap_2345678=AbCdEfGhIjKlMnOpQrStUvWxYz0AAAAAQUIPAAAAAAA; expires=Sat, 11 Oct 2027 09:12:44 GMT; HttpOnly; path=/; Domain=.portoceramics.example; Secure; SameSite=None",
        )
}

/// Imperva's (Incapsula's) 200 stub: an iframe that loads the challenge.
fn imperva_stub() -> Page {
    let body = r#"<html style="height:100%"><head><META NAME="ROBOTS" CONTENT="NOINDEX, NOFOLLOW"><meta name="format-detection" content="telephone=no"><meta name="viewport" content="initial-scale=1.0"><meta http-equiv="X-UA-Compatible" content="IE=edge,chrome=1"></head><body style="margin:0px;height:100%"><iframe id="main-iframe" src="/_Incapsula_Resource?CWUDNSAI=24&xinfo=12-34567890-0%200NNN%20RT%281728640000000%2014%29%20q%280%20-1%20-1%20-1%29%20r%280%20-1%29%20B12%284%2c315%2c0%29%20U18&incident_id=1234000560012345678-123456789012345678&edet=12&cinfo=04000000&rpinfo=0&mth=GET" frameborder=0 width="100%" height="100%" marginheight="0px" marginwidth="0px">Request unsuccessful. Incapsula incident ID: 1234000560012345678-123456789012345678</iframe></body></html>"#;
    Page::html(body)
        .header("x-cdn", "Imperva")
        .header(
            "x-iinfo",
            "12-34567890-0 0NNN RT(1728640000000 14) q(0 -1 -1 -1) r(0 -1) B12(4,315,0) U18",
        )
        .header("cache-control", "no-cache")
}

/// A normal page on Vercel.
fn vercel_ok() -> Page {
    home()
        .header("server", "Vercel")
        .header(
            "x-vercel-id",
            "fra1::iad1::8k2xq-1728640000000-0a1b2c3d4e5f",
        )
        .header("x-vercel-cache", "HIT")
}

/// Vercel's Security Checkpoint, which Vercel serves with a 429.
fn vercel_checkpoint() -> Page {
    let body = r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Vercel Security Checkpoint</title><meta name="robots" content="noindex, nofollow"><style>body{margin:0;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;background:#fff;color:#000}.container{display:flex;flex-direction:column;align-items:center;justify-content:center;min-height:100vh}</style></head><body><div class="container"><div class="spinner" aria-hidden="true"></div><p id="header-text">We're verifying your browser</p><noscript><p>Enable JavaScript to continue.</p></noscript><p class="footer">Website owner? <a href="https://vercel.com/docs/security/attack-challenge-mode">Click here to fix</a></p></div><script src="/.well-known/vercel/security/static/challenge.v2.min.js" async></script></body></html>"#;
    Page::status(429, body)
        .header("server", "Vercel")
        .header("x-vercel-id", "fra1::8k2xq-1728640000001-1b2c3d4e5f6a")
        .header("x-vercel-mitigated", "challenge")
        .header(
            "x-vercel-challenge-token",
            "2.1728640000.60.ZWUxYjM0OTE4.YmFzZTY0c2ln",
        )
        .header("cache-control", "private, no-store, max-age=0")
}

// ---- The corpus -----------------------------------------------------------------------------

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            name: "ua_403",
            site: || {
                SiteBuilder::new().every_path(for_bots(
                    &[OAI, PPLX],
                    nginx_error(403, "Forbidden"),
                    home(),
                ))
            },
            expect: |t| only(&[OAI, PPLX], t, "blocked_403"),
            path: "/",
            robots: ALLOW_ALL,
        },
        // The plan targets Claude-*, which has no documented user agent and is not probed; the
        // two OpenAI and Perplexity user fetchers stand in for it.
        Fixture {
            name: "cf_challenge_200",
            site: || {
                SiteBuilder::new().every_path(for_bots(
                    &[CHATGPT, PPLX_USER],
                    cloudflare_challenge(200),
                    cloudflare_ok(),
                ))
            },
            expect: |t| only(&[CHATGPT, PPLX_USER], t, "challenge_cloudflare"),
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "cf_ai_crawl_control_403",
            site: || {
                SiteBuilder::new().every_path(for_bots(&ALL_AI, cloudflare_1020(), cloudflare_ok()))
            },
            expect: |_| Expect::Problem("challenge_cloudflare"),
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "flapping_429",
            site: || {
                let flap = Page::sequence(vec![
                    nginx_error(429, "Too Many Requests").header("retry-after", "1"),
                    home(),
                ]);
                SiteBuilder::new().every_path(for_bots(&ALL_AI, flap.clone(), flap))
            },
            expect: fine,
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "persistent_429_bots",
            site: || {
                SiteBuilder::new().every_path(for_bots(
                    &ALL_AI,
                    nginx_error(429, "Too Many Requests").header("retry-after", "1"),
                    home(),
                ))
            },
            expect: |_| Expect::Problem("throttled"),
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "geo_block_everyone",
            site: || {
                SiteBuilder::new().every_path(Page::status(
                    403,
                    "<!DOCTYPE html><html><head><title>Not available in your region</title></head>\
                     <body><h1>Sorry, this service is not available in your region.</h1>\
                     <p>Porto Ceramics only ships within the EU and the UK.</p></body></html>",
                ))
            },
            expect: fine,
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "login_wall_everyone",
            site: || {
                SiteBuilder::new()
                    .page("/", Page::redirect(302, "/login?next=%2F"))
                    .page(
                        "/login",
                        Page::html(
                            "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
                             <title>Sign in | Porto Ceramics Trade</title></head><body><main>\
                             <h1>Sign in to the trade portal</h1>\
                             <form method=\"post\" action=\"/login\">\
                             <label>Email <input type=\"email\" name=\"email\" autocomplete=\"username\"></label>\
                             <label>Password <input type=\"password\" name=\"password\" autocomplete=\"current-password\"></label>\
                             <input type=\"hidden\" name=\"csrf\" value=\"c2VjcmV0LXRva2Vu\">\
                             <button type=\"submit\">Sign in</button></form>\
                             <p><a href=\"/forgot\">Forgot your password?</a></p></main></body></html>",
                        ),
                    )
            },
            expect: fine,
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "different_but_fine",
            site: || {
                // A rotating block of recommendations: every response is 60% to 140% of the
                // usual size, with the same title.
                let sizes = [24, 14, 34, 18, 30, 15, 33, 20, 28, 16, 32, 22];
                let pages = sizes
                    .iter()
                    .cycle()
                    .take(40)
                    .map(|&n| Page::html(&article(HOME_TITLE, n)))
                    .collect();
                SiteBuilder::new().every_path(Page::sequence(pages))
            },
            expect: fine,
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "tiny_valid",
            site: || {
                SiteBuilder::new().every_path(Page::html(
                    "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
                     <title>Porto Ceramics</title></head><body><h1>Porto Ceramics</h1>\
                     <p>Our new shop opens in November. Write to us at hello@portoceramics.example \
                     for trade orders.</p></body></html>",
                ))
            },
            expect: fine,
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "redirect_home_for_bots",
            site: || {
                SiteBuilder::new().page("/", home()).page(
                    "/page",
                    for_bots(
                        &ALL_AI,
                        Page::redirect(302, "/"),
                        Page::html(&article("Glaze care guide | Porto Ceramics", 20)),
                    ),
                )
            },
            expect: |_| Expect::Problem("diverted"),
            path: "/page",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "datadome_403",
            site: || {
                SiteBuilder::new().every_path(for_bots(
                    &[OAI, PPLX, META_INDEX, MISTRAL_INDEX],
                    datadome_block(),
                    datadome_ok(),
                ))
            },
            expect: |t| {
                only(
                    &[OAI, PPLX, META_INDEX, MISTRAL_INDEX],
                    t,
                    "challenge_datadome",
                )
            },
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "human_press_hold_200",
            site: || {
                SiteBuilder::new().every_path(for_bots(
                    &[CHATGPT, PPLX_USER, MISTRAL_USER],
                    human_press_hold(),
                    home(),
                ))
            },
            expect: |t| only(&[CHATGPT, PPLX_USER, MISTRAL_USER], t, "challenge_human"),
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "akamai_denied_403",
            site: || {
                SiteBuilder::new().every_path(for_bots(&[OAI, CHATGPT], akamai_denied(), home()))
            },
            expect: |t| only(&[OAI, CHATGPT], t, "challenge_akamai"),
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "imperva_200_stub",
            site: || {
                SiteBuilder::new().every_path(for_bots(
                    &[PPLX, PPLX_USER, DUCK],
                    imperva_stub(),
                    imperva_ok(),
                ))
            },
            expect: |t| only(&[PPLX, PPLX_USER, DUCK], t, "challenge_imperva"),
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "vercel_checkpoint_429",
            site: || {
                SiteBuilder::new().every_path(for_bots(
                    &[OAI, META_FETCH],
                    vercel_checkpoint(),
                    vercel_ok(),
                ))
            },
            expect: |t| only(&[OAI, META_FETCH], t, "challenge_vercel"),
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "recaptcha_form_everyone",
            site: || {
                SiteBuilder::new().every_path(Page::html(
                    "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
                     <title>Contact us | Porto Ceramics</title>\
                     <script src=\"https://www.google.com/recaptcha/api.js\" async defer></script>\
                     </head><body><h1>Contact us</h1><form method=\"post\" action=\"/contact\">\
                     <label>Name <input name=\"name\"></label>\
                     <label>Email <input type=\"email\" name=\"email\"></label>\
                     <label>Message <textarea name=\"message\"></textarea></label>\
                     <div class=\"g-recaptcha\" data-sitekey=\"6LcAbCdEAAAAAFgHiJkLmNoPqRsTuVwXyZ012345\"></div>\
                     <button type=\"submit\">Send</button></form></body></html>",
                ))
            },
            expect: fine,
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "prerender_for_bots",
            site: || {
                // A prerendering service gives bots the fully rendered page, three times the
                // size of the app shell's server render, under the same title.
                SiteBuilder::new().every_path(for_bots(
                    &ALL_AI,
                    Page::html(&article(HOME_TITLE, 72)).header("x-prerender", "1"),
                    home(),
                ))
            },
            expect: fine,
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "js_shell_200_for_bots",
            site: || {
                // The site server-renders for user agents that look like browsers and sends the
                // bare app shell to the rest: the three bots whose user agent lacks "Mozilla".
                SiteBuilder::new().every_path(for_bots(
                    &[META_INDEX, META_FETCH, DUCK],
                    Page::html(
                        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
                         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
                         <title>React App</title><script defer=\"defer\" \
                         src=\"/static/js/main.3f2a1b7c.js\"></script><link \
                         href=\"/static/css/main.8e4c2d1a.css\" rel=\"stylesheet\"></head><body>\
                         <noscript>You need to enable JavaScript to run this app.</noscript>\
                         <div id=\"root\"></div></body></html>",
                    ),
                    home(),
                ))
            },
            expect: |t| only(&[META_INDEX, META_FETCH, DUCK], t, "degraded"),
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "robots_disallow_perplexity",
            site: || {
                SiteBuilder::new().every_path(for_bots(
                    &[PPLX],
                    nginx_error(403, "Forbidden"),
                    home(),
                ))
            },
            expect: fine,
            path: "/",
            robots: "User-agent: PerplexityBot\nDisallow: /\n\nUser-agent: *\nAllow: /\n",
        },
        Fixture {
            name: "bot_404",
            site: || {
                SiteBuilder::new().every_path(for_bots(
                    &[MISTRAL_INDEX, MISTRAL_USER],
                    nginx_error(404, "Not Found"),
                    home(),
                ))
            },
            expect: |t| only(&[MISTRAL_INDEX, MISTRAL_USER], t, "blocked_404"),
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "timeout_once",
            site: || {
                // Every user agent's first request hangs past the client timeout; the retry is
                // answered at once.
                let once = Page::sequence(vec![home().delayed(Duration::from_secs(3)), home()]);
                SiteBuilder::new().every_path(for_bots(&ALL_AI, once.clone(), once))
            },
            expect: fine,
            path: "/",
            robots: ALLOW_ALL,
        },
        Fixture {
            name: "maintenance_503_everyone",
            site: || {
                SiteBuilder::new().every_path(
                    Page::status(
                        503,
                        "<!DOCTYPE html><html><head><title>Down for maintenance</title></head>\
                         <body><h1>We'll be back soon</h1><p>Porto Ceramics is being updated. \
                         Please check back in an hour.</p></body></html>",
                    )
                    .header("retry-after", "3600"),
                )
            },
            expect: fine,
            path: "/",
            robots: ALLOW_ALL,
        },
        // A firewall rule on the real bots' published address ranges: our requests come from
        // another address, so the site serves them normally and the block can't be seen.
        Fixture {
            name: "ip_only_block",
            site: || SiteBuilder::new().every_path(home()),
            expect: |_| Expect::Undetectable,
            path: "/",
            robots: ALLOW_ALL,
        },
    ]
}

// ---- Running and scoring --------------------------------------------------------------------

/// A crawl of `site` that found `paths` (`/` first) as 200 HTML pages, with `robots` as its
/// robots.txt.
fn crawl_of(site: &TestSite, paths: &[&str], robots: &str) -> CrawlOutput {
    let pages = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let url = site.url(p);
            PageRecord {
                url_hash: url_hash(&url),
                url,
                status: 200,
                redirect_chain: Vec::new(),
                response_ms: 10,
                size_bytes: 6000,
                content_type: Some("text/html; charset=utf-8".to_owned()),
                depth: Some(i as u16),
                in_sitemap: false,
                indexability: Indexability::Indexable,
                fields: PageFields::default(),
                inlinks: 10 - i as u32,
                outlinks_internal: 0,
                outlinks_external: 0,
                issues: IssueBits::default(),
                key_hash: 0,
                redirect_target: None,
                outlinks_nofollow: 0,
                error: None,
            }
        })
        .collect();
    CrawlOutput {
        origin: site.url("/"),
        pages,
        links: LinkGraph::default(),
        robots: Some(RobotsFile {
            status: 200,
            body: robots.to_owned(),
            hash: 1,
        }),
        sitemap: SitemapSummary::default(),
        sitemap_files: Vec::new(),
        stop: StopReason::Completed,
        duration_ms: 1,
        signals: SiteSignals::default(),
    }
}

struct Outcome {
    name: &'static str,
    /// (token, verdict, expected) for every probed bot, in plan order.
    rows: Vec<(String, Verdict, Expect)>,
    elapsed: Duration,
}

/// Starts the fixture's site, builds the access report from a crawl of it, and probes its page
/// with every candidate bot. Also checks that robots-skipped bots were never requested.
async fn run_fixture(f: &'static Fixture) -> Outcome {
    let start = Instant::now();
    let site = (f.site)().start().await;
    let paths: Vec<&str> = if f.path == "/" {
        vec!["/"]
    } else {
        vec!["/", f.path]
    };
    let out = crawl_of(&site, &paths, f.robots);
    let important = important_urls(&out.pages, &out.origin, &Default::default());
    let report = build_report(&out, &important);
    let mut plan = plan_probes(&report, None, 5).expect("a probe plan");
    assert_eq!(
        plan.bots,
        candidates().0,
        "{}: all candidate bots probed",
        f.name
    );
    let target = site.url(f.path);
    plan.urls
        .retain(|&i| report.important[usize::from(i)].url == target.as_str());
    assert_eq!(plan.urls.len(), 1, "{}: probes {}", f.name, f.path);

    let mut cfg = ProbeConfig::new(
        &report,
        plan,
        AddressPolicy::AllowPrivate,
        Duration::from_secs(120),
        200,
    );
    cfg.min_gap = Duration::from_millis(20);
    cfg.request_timeout = Duration::from_secs(1);
    let probes: ProbeReport = run_probes(cfg).await;
    assert_eq!(probes.stop, None, "{}: the run finished", f.name);
    assert_eq!(probes.urls.len(), 1, "{}", f.name);

    let user_agents: BTreeMap<String, String> = candidates()
        .0
        .into_iter()
        .map(|b| (b.token, b.user_agent))
        .collect();
    let rows = probes.urls[0]
        .results
        .iter()
        .map(|r| {
            if r.verdict == Verdict::SkippedRobots {
                assert_eq!(
                    site.hits_from(&user_agents[&r.token]),
                    0,
                    "{}: {} is disallowed and never requested",
                    f.name,
                    r.token
                );
            }
            (r.token.clone(), r.verdict.clone(), (f.expect)(&r.token))
        })
        .collect();
    if f.name == "robots_disallow_perplexity" {
        assert_eq!(site.hits_from(PPLX), 0);
        assert_eq!(
            probes.urls[0]
                .results
                .iter()
                .find(|r| r.token == PPLX)
                .map(|r| r.verdict.clone()),
            Some(Verdict::SkippedRobots)
        );
    }
    Outcome {
        name: f.name,
        rows,
        elapsed: start.elapsed(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Score {
    Tp,
    Tn,
    Fp,
    Fn,
    /// A problem found where one was expected, under another cause.
    WrongCause,
    Unscored,
}

fn score(verdict: &Verdict, expect: Expect) -> Score {
    let found = verdict.cause_slug();
    match (expect, found) {
        (Expect::Undetectable, _) => Score::Unscored,
        (Expect::Fine, None) => Score::Tn,
        (Expect::Fine, Some(_)) => Score::Fp,
        (Expect::Problem(_), None) => Score::Fn,
        (Expect::Problem(want), Some(got)) if got.starts_with(want) => Score::Tp,
        (Expect::Problem(_), Some(_)) => Score::WrongCause,
    }
}

fn label(v: &Verdict) -> String {
    match v {
        Verdict::Inconclusive { reason } => format!("inconclusive({reason:?})"),
        Verdict::SkippedRobots => "skipped_robots".to_owned(),
        Verdict::Same => "same".to_owned(),
        v => v.cause_slug().unwrap_or("?").to_owned(),
    }
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        100.0 * n as f64 / d as f64
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn synthetic_probe_corpus_has_no_false_positives_or_negatives() {
    let fixtures: &'static [Fixture] = Box::leak(fixtures().into_boxed_slice());
    assert_eq!(fixtures.len(), 23);
    assert_eq!(candidates().0.len(), ALL_AI.len());

    let start = Instant::now();
    let mut set = JoinSet::new();
    for (i, f) in fixtures.iter().enumerate() {
        set.spawn(async move { (i, run_fixture(f).await) });
    }
    let mut outcomes: Vec<(usize, Outcome)> = Vec::new();
    while let Some(done) = set.join_next().await {
        outcomes.push(done.expect("a fixture ran to the end"));
    }
    outcomes.sort_by_key(|(i, _)| *i);

    let (mut tp, mut tn, mut fp, mut fn_, mut wrong) = (0, 0, 0, 0, 0);
    let mut errors = Vec::new();
    let mut undetectable = Vec::new();
    println!();
    println!(
        "{:<26} {:>3} {:>3} {:>3} {:>3} {:>5} {:>6}  positive or notable verdicts",
        "fixture", "TP", "TN", "FP", "FN", "cause", "secs"
    );
    for (_, o) in &outcomes {
        let mut c = [0usize; 5];
        let mut notes = Vec::new();
        for (token, verdict, expect) in &o.rows {
            let s = score(verdict, *expect);
            match s {
                Score::Tp => c[0] += 1,
                Score::Tn => c[1] += 1,
                Score::Fp => c[2] += 1,
                Score::Fn => c[3] += 1,
                Score::WrongCause => c[4] += 1,
                Score::Unscored => {
                    undetectable.push(format!("{}/{token}={}", o.name, label(verdict)))
                }
            }
            if matches!(s, Score::Fp | Score::Fn | Score::WrongCause) {
                errors.push(format!(
                    "{}: {token} expected {expect:?}, got {verdict:?} ({s:?})",
                    o.name
                ));
            }
            if *verdict != Verdict::Same && s != Score::Unscored {
                notes.push(format!("{token}={}", label(verdict)));
            }
        }
        tp += c[0];
        tn += c[1];
        fp += c[2];
        fn_ += c[3];
        wrong += c[4];
        println!(
            "{:<26} {:>3} {:>3} {:>3} {:>3} {:>5} {:>6.1}  {}",
            o.name,
            c[0],
            c[1],
            c[2],
            c[3],
            c[4],
            o.elapsed.as_secs_f64(),
            notes.join(" ")
        );
    }
    let pairs = tp + tn + fp + fn_ + wrong;
    println!();
    println!(
        "synthetic probe corpus: {pairs} pairs, FP {fp} ({:.1}%), FN {fn_} ({:.1}%)",
        pct(fp, fp + tn),
        pct(fn_, fn_ + tp)
    );
    println!(
        "TP {tp}, TN {tn}, wrong cause {wrong}; not scored (undetectable by a synthetic probe): {}",
        undetectable.len()
    );
    for u in &undetectable {
        println!("  undetectable: {u}");
    }
    println!("ran in {:.1} s", start.elapsed().as_secs_f64());
    for e in &errors {
        println!("  error: {e}");
    }

    assert_eq!(
        pairs,
        22 * ALL_AI.len(),
        "22 scored fixtures × every probed bot"
    );
    assert_eq!(undetectable.len(), ALL_AI.len());
    assert_eq!(fp, 0, "false positives: {errors:#?}");
    assert_eq!(fn_, 0, "false negatives: {errors:#?}");
    assert_eq!(wrong, 0, "wrong causes: {errors:#?}");
}
