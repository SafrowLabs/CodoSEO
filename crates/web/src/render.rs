//! Rendering helpers shared by every route: templates to HTML, htmx request detection, and
//! the `HX-Trigger` toast header.

use std::convert::Infallible;

use askama::Template;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::Html;

use crate::error::AppError;

/// Renders a template, turning a template error into a 500 page.
pub fn html<T: Template>(t: &T) -> Result<Html<String>, AppError> {
    Ok(Html(t.render()?))
}

pub fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v == "true")
}

/// What htmx told us about the request. A boosted request (a normal link or form that htmx
/// took over) still wants the whole page; a plain `hx-get` wants just its fragment.
#[derive(Debug, Clone, Default)]
pub struct Hx {
    pub request: bool,
    pub boosted: bool,
    pub target: Option<String>,
}

impl Hx {
    /// True when only a fragment should be returned (htmx asked, and not as a boosted
    /// navigation).
    pub fn partial(&self) -> bool {
        self.request && !self.boosted
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Hx {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Hx, Infallible> {
        let h = &parts.headers;
        Ok(Hx {
            request: is_htmx(h),
            boosted: h.get("hx-boosted").is_some_and(|v| v == "true"),
            target: h
                .get("hx-target")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Ok,
    Info,
    Error,
}

impl ToastKind {
    fn as_str(self) -> &'static str {
        match self {
            ToastKind::Ok => "ok",
            ToastKind::Info => "info",
            ToastKind::Error => "error",
        }
    }
}

/// An `HX-Trigger` header that makes `app.js` show a toast:
/// `([toast(ToastKind::Ok, "Crawl queued")], html)` from a handler.
pub fn toast(kind: ToastKind, message: &str) -> (HeaderName, HeaderValue) {
    let payload = serde_json::json!({ "toast": { "kind": kind.as_str(), "message": message } });
    (
        HeaderName::from_static("hx-trigger"),
        HeaderValue::from_str(&payload.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("{}")),
    )
}

/// `HX-Redirect`: htmx does a full navigation to `to`.
pub fn hx_redirect(to: &str) -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static("hx-redirect"),
        HeaderValue::from_str(to).unwrap_or_else(|_| HeaderValue::from_static("/")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_header_is_json() {
        let (name, value) = toast(ToastKind::Ok, "Crawl \"queued\"");
        assert_eq!(name, "hx-trigger");
        let v: serde_json::Value = serde_json::from_str(value.to_str().unwrap()).unwrap();
        assert_eq!(v["toast"]["kind"], "ok");
        assert_eq!(v["toast"]["message"], "Crawl \"queued\"");
    }
}
