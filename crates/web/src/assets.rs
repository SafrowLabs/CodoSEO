//! Static assets embedded in the binary (the self-hosted UI makes no outside requests).
//!
//! Each asset is served under a content-hashed name (`/assets/app.3f2a1b9c.css`) with a
//! one-year immutable cache, so a deploy never serves a stale stylesheet. CSS, JS and SVG are
//! gzip-compressed once, the first time any asset is asked for.

use std::borrow::Cow;
use std::io::Write;
use std::sync::LazyLock;

use axum::extract::Path;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use flate2::Compression;
use flate2::write::GzEncoder;
use xxhash_rust::xxh3::xxh3_64;

struct Source {
    name: &'static str,
    content_type: &'static str,
    bytes: &'static [u8],
}

const SOURCES: &[Source] = &[
    Source {
        name: "Geist-Variable.woff2",
        content_type: "font/woff2",
        bytes: include_bytes!("../assets/fonts/Geist-Variable.woff2"),
    },
    Source {
        name: "GeistMono-Variable.woff2",
        content_type: "font/woff2",
        bytes: include_bytes!("../assets/fonts/GeistMono-Variable.woff2"),
    },
    Source {
        name: "htmx.min.js",
        content_type: "text/javascript; charset=utf-8",
        bytes: include_bytes!("../assets/htmx.min.js"),
    },
    Source {
        name: "app.js",
        content_type: "text/javascript; charset=utf-8",
        bytes: include_bytes!("../assets/app.js"),
    },
    Source {
        name: "favicon.svg",
        content_type: "image/svg+xml",
        bytes: include_bytes!("../assets/favicon.svg"),
    },
    // Last, so its font URLs can be rewritten to the fonts' hashed names.
    Source {
        name: "app.css",
        content_type: "text/css; charset=utf-8",
        bytes: include_bytes!("../assets/app.css"),
    },
];

pub struct Asset {
    pub name: &'static str,
    /// `/assets/<stem>.<hash>.<ext>`
    pub url: String,
    content_type: &'static str,
    bytes: Cow<'static, [u8]>,
    gzip: Option<Vec<u8>>,
}

pub struct Assets {
    items: Vec<Asset>,
}

static ASSETS: LazyLock<Assets> = LazyLock::new(Assets::build);

impl Assets {
    fn build() -> Assets {
        let mut items: Vec<Asset> = Vec::with_capacity(SOURCES.len());
        for src in SOURCES {
            let mut bytes = Cow::Borrowed(src.bytes);
            if src.name.ends_with(".css") {
                // `url(/assets/Geist-Variable.woff2)` in the stylesheet -> the hashed URL.
                let mut text = String::from_utf8_lossy(src.bytes).into_owned();
                for done in &items {
                    text = text.replace(&format!("/assets/{}", done.name), &done.url);
                }
                bytes = Cow::Owned(text.into_bytes());
            }
            let hash = xxh3_64(&bytes);
            let (stem, ext) = src.name.rsplit_once('.').unwrap_or((src.name, ""));
            let url = format!("/assets/{stem}.{:08x}.{ext}", hash as u32);
            let gzip = compressible(src.content_type).then(|| gzip(&bytes));
            items.push(Asset {
                name: src.name,
                url,
                content_type: src.content_type,
                bytes,
                gzip,
            });
        }
        Assets { items }
    }

    fn by_url(&self, url: &str) -> Option<&Asset> {
        self.items.iter().find(|a| a.url == url)
    }

    fn by_name(&self, name: &str) -> Option<&Asset> {
        self.items.iter().find(|a| a.name == name)
    }
}

fn compressible(content_type: &str) -> bool {
    content_type.starts_with("text/") || content_type.starts_with("image/svg")
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(bytes).expect("writing to a Vec cannot fail");
    enc.finish().expect("writing to a Vec cannot fail")
}

/// The hashed URL of an embedded asset, for templates: `{{ crate::assets::url("app.css") }}`.
pub fn url(name: &str) -> &'static str {
    match ASSETS.by_name(name) {
        Some(a) => a.url.as_str(),
        None => panic!("unknown asset {name:?}"),
    }
}

/// `GET /assets/{file}`
pub async fn serve(Path(file): Path<String>, headers: HeaderMap) -> Response {
    let Some(asset) = ASSETS.by_url(&format!("/assets/{file}")) else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
            "not found",
        )
            .into_response();
    };
    let accepts_gzip = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|e| e.trim().starts_with("gzip")));

    let mut res = match (&asset.gzip, accepts_gzip) {
        (Some(gz), true) => {
            let mut r = gz.clone().into_response();
            r.headers_mut()
                .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            r
        }
        _ => asset.bytes.to_vec().into_response(),
    };
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    if asset.gzip.is_some() {
        h.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_asset_has_a_hashed_url() {
        for src in SOURCES {
            let u = url(src.name);
            assert!(u.starts_with("/assets/"), "{u}");
            assert_ne!(u, format!("/assets/{}", src.name));
        }
    }

    #[test]
    fn css_points_at_hashed_fonts() {
        let css = ASSETS.by_name("app.css").unwrap();
        let text = std::str::from_utf8(&css.bytes).unwrap();
        assert!(text.contains(url("Geist-Variable.woff2")));
        assert!(!text.contains("/assets/Geist-Variable.woff2"));
    }
}
