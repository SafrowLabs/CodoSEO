//! Test-only helpers shared by the crawler and CLI tests: a local HTTP server and a
//! builder for small generated sites. Not published.

mod server;
mod site;

pub use server::{TestServer, gzip};
pub use site::{Page, SiteBuilder, TestSite, html_page};
