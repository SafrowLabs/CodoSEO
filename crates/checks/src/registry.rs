//! Metadata for every check, indexed by `CheckId`.

use codoseo_core::check::{CheckId, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Response,
    Indexability,
    Links,
    OnPage,
    Content,
    Technical,
    Sitemap,
    Social,
    Schema,
}

/// What a check needs in order to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Looks at one page on its own and sets a bit on that page.
    Page,
    /// Needs the whole crawl but still sets a bit on each affected page.
    Site,
    /// A fact about the whole site; no page bit is set.
    SiteWide,
}

#[derive(Debug, Clone, Copy)]
pub struct CheckDef {
    pub id: CheckId,
    pub severity: Severity,
    pub category: Category,
    pub scope: Scope,
    pub title: &'static str,
}

const fn d(
    id: CheckId,
    severity: Severity,
    category: Category,
    scope: Scope,
    title: &'static str,
) -> CheckDef {
    CheckDef {
        id,
        severity,
        category,
        scope,
        title,
    }
}

use Category::{
    Content, Indexability, Links, OnPage, Response, Schema, Sitemap, Social, Technical,
};
use Scope::{Page, Site, SiteWide};
use Severity::{Critical as C, Notice as N, Warning as W};

/// Every check, in `CheckId` order.
pub static CHECKS: [CheckDef; 44] = [
    d(
        CheckId::Http4xx,
        C,
        Response,
        Page,
        "Page returns a 4xx error",
    ),
    d(
        CheckId::Http5xx,
        C,
        Response,
        Page,
        "Page returns a 5xx error",
    ),
    d(
        CheckId::FetchFailed,
        C,
        Response,
        Page,
        "Page could not be fetched",
    ),
    d(
        CheckId::RedirectLoop,
        C,
        Response,
        Page,
        "Redirect loop or too many redirects",
    ),
    d(
        CheckId::Redirected,
        N,
        Response,
        Page,
        "URL redirects to another URL",
    ),
    d(
        CheckId::RedirectChain,
        W,
        Response,
        Page,
        "Redirect chain has 2 or more hops",
    ),
    d(
        CheckId::Noindex,
        N,
        Indexability,
        Page,
        "Page is set to noindex",
    ),
    d(
        CheckId::Canonicalised,
        N,
        Indexability,
        Page,
        "Page points to a different canonical URL",
    ),
    d(
        CheckId::CanonicalMissing,
        N,
        Indexability,
        Page,
        "Canonical URL is missing",
    ),
    d(
        CheckId::BlockedByRobots,
        W,
        Indexability,
        Page,
        "Page is blocked by robots.txt",
    ),
    d(
        CheckId::CanonicalToNon200,
        W,
        Indexability,
        Site,
        "Canonical URL does not return 200",
    ),
    d(
        CheckId::RobotsBlocksSite,
        C,
        Indexability,
        SiteWide,
        "robots.txt blocks the whole site",
    ),
    d(CheckId::TitleMissing, W, OnPage, Page, "Title is missing"),
    d(
        CheckId::TitleTooLong,
        N,
        OnPage,
        Page,
        "Title is over 60 characters",
    ),
    d(
        CheckId::TitleTooShort,
        N,
        OnPage,
        Page,
        "Title is under 30 characters",
    ),
    d(
        CheckId::TitleMultiple,
        W,
        OnPage,
        Page,
        "Page has more than one title",
    ),
    d(
        CheckId::TitleDuplicate,
        W,
        OnPage,
        Site,
        "Title is used on other pages",
    ),
    d(
        CheckId::DescriptionMissing,
        W,
        OnPage,
        Page,
        "Meta description is missing",
    ),
    d(
        CheckId::DescriptionTooLong,
        N,
        OnPage,
        Page,
        "Meta description is over 160 characters",
    ),
    d(
        CheckId::DescriptionTooShort,
        N,
        OnPage,
        Page,
        "Meta description is under 70 characters",
    ),
    d(
        CheckId::DescriptionDuplicate,
        W,
        OnPage,
        Site,
        "Meta description is used on other pages",
    ),
    d(CheckId::H1Missing, W, OnPage, Page, "H1 heading is missing"),
    d(
        CheckId::H1Multiple,
        N,
        OnPage,
        Page,
        "Page has more than one H1",
    ),
    d(
        CheckId::H1Duplicate,
        N,
        OnPage,
        Site,
        "H1 is used on other pages",
    ),
    d(
        CheckId::ThinContent,
        N,
        Content,
        Page,
        "Page has under 200 words",
    ),
    d(
        CheckId::ContentDuplicate,
        W,
        Content,
        Site,
        "Page content duplicates another page",
    ),
    d(
        CheckId::ImagesMissingAlt,
        N,
        Content,
        Page,
        "Images are missing alt text",
    ),
    d(
        CheckId::LinksToBroken,
        W,
        Links,
        Site,
        "Page links to a broken URL",
    ),
    d(
        CheckId::LinksToRedirect,
        N,
        Links,
        Site,
        "Page links to a redirecting URL",
    ),
    d(
        CheckId::Orphan,
        W,
        Links,
        Site,
        "Page has no internal links pointing to it",
    ),
    d(
        CheckId::NoInternalOutlinks,
        N,
        Links,
        Page,
        "Page has no internal links out",
    ),
    d(
        CheckId::NofollowInternalLinks,
        N,
        Links,
        Page,
        "Page has nofollow internal links",
    ),
    d(
        CheckId::DeepPage,
        N,
        Links,
        Page,
        "Page is more than 3 clicks from the homepage",
    ),
    d(
        CheckId::SitemapNon200,
        W,
        Sitemap,
        Site,
        "Page is in the sitemap but not 200",
    ),
    d(
        CheckId::SitemapNoindex,
        W,
        Sitemap,
        Site,
        "Page is in the sitemap but noindex",
    ),
    d(
        CheckId::SitemapCanonicalised,
        N,
        Sitemap,
        Site,
        "Page is in the sitemap but canonicalised",
    ),
    d(
        CheckId::NotInSitemap,
        N,
        Sitemap,
        Site,
        "Indexable page is not in the sitemap",
    ),
    d(
        CheckId::SitemapMissing,
        N,
        Sitemap,
        SiteWide,
        "Site has no sitemap",
    ),
    d(
        CheckId::MixedContent,
        W,
        Technical,
        Page,
        "HTTPS page loads HTTP resources",
    ),
    d(
        CheckId::NotHttps,
        W,
        Technical,
        Page,
        "Page is served over HTTP",
    ),
    d(
        CheckId::SlowResponse,
        N,
        Technical,
        Page,
        "Response took over 1 second",
    ),
    d(
        CheckId::OgMissing,
        N,
        Social,
        Page,
        "Open Graph title or image is missing",
    ),
    d(
        CheckId::JsonldInvalid,
        W,
        Schema,
        Page,
        "JSON-LD does not parse",
    ),
    d(
        CheckId::HreflangMissingSelf,
        N,
        Schema,
        Page,
        "Hreflang has no entry for the page itself",
    ),
];

/// The metadata for one check.
pub fn def(id: CheckId) -> &'static CheckDef {
    &CHECKS[id as usize]
}
