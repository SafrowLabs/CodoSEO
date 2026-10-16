//! The `codoseo.toml` that `codoseo policy init` writes: every key, commented out at its
//! default, so the file documents itself and parses to [`SitePolicy::default`].
//!
//! [`SitePolicy::default`]: crate::SitePolicy::default

use crate::{HostPref, Slash};

/// The starting file. With `address` (from `--from URL`), the preferred address is filled in
/// and switched on.
pub fn init_file(address: Option<(HostPref, Slash)>) -> String {
    let address_block = match address {
        Some((host, slash)) => format!(
            "# The one form every page should use (read from the site's redirects).\n\
             [policy.seo.address]\n\
             host = \"{}\"\n\
             trailing_slash = \"{}\"\n",
            match host {
                HostPref::Apex => "apex",
                HostPref::Www => "www",
            },
            match slash {
                Slash::Without => "without",
                Slash::With => "with",
                Slash::Either => "either",
            }
        ),
        None => "# The one form every page should use: other forms should 301 to it.\n\
                 # [policy.seo.address]\n\
                 # host = \"apex\"              # apex | www\n\
                 # trailing_slash = \"without\" # without | with | either\n"
            .to_owned(),
    };
    format!(
        "# CodoSEO site policy: how this site is meant to work for search engines and AI.\n\
# `codoseo crawl` and `codoseo check` read ./codoseo.toml when it is there (or --policy FILE);\n\
# `codoseo policy validate` checks this file. Keys left out keep their defaults.\n\
# Docs: https://github.com/SafrowLabs/CodoSEO/blob/main/docs/policy.md\n\
\n\
[policy]\n\
version = 1\n\
\n\
# ---- SEO rules (evaluated from the next release; the format is stable) ----\n\
\n\
{address_block}\
\n\
# What search engines should index: path rules, top to bottom, the first match wins.\n\
# `*` matches anything, `/` included. keep_out expects a noindex tag or header.\n\
# [[policy.seo.index]]\n\
# pattern = \"/checkout/*\"\n\
# stance = \"keep_out\"         # index | keep_out | none\n\
\n\
# [policy.seo]\n\
# index_default = \"none\"      # what pages no rule matches should be\n\
# ignore_params = [\"utm_*\", \"gclid\", \"fbclid\"]  # parameters that don't change the page\n\
# pagination_param = \"page\"   # each page canonicalises to itself\n\
# must_work = [\"/\", \"/pricing\"]  # must answer 200 and be indexable on every crawl\n\
\n\
# Check thresholds; the defaults are shown.\n\
# [policy.seo.thresholds]\n\
# title_min = 30\n\
# title_max = 60\n\
# description_min = 70\n\
# description_max = 160\n\
# thin_words = 200\n\
# slow_ms = 1000\n\
# max_redirect_hops = 1\n\
# image_alt = true\n\
\n\
# Checks that don't apply to part of the site.\n\
# [[policy.seo.exceptions]]\n\
# check = \"thin_content\"\n\
# scope = \"/stores/*\"\n\
# note = \"Store pages are short by design\"\n\
\n\
# ---- AI rules ----\n\
\n\
[policy.ai]\n\
# Per purpose: allow | block | any (no preference: never reported either way).\n\
# Defaults: search and user_fetch allow; agent, training and ads any.\n\
# purposes = {{ search = \"allow\", user_fetch = \"allow\", agent = \"any\", training = \"any\", ads = \"any\" }}\n\
# Per-bot exceptions, by robots.txt token (codoseo bots lists them).\n\
# bots = {{ \"GPTBot\" = \"block\" }}\n\
# Page directives you use on purpose: nosnippet, max_snippet, noarchive, nocache, data_nosnippet.\n\
# accepted_directives = [\"max_snippet\"]\n\
# Synthetic fetch checks after each monitored crawl (the CLI runs them only with --probes).\n\
probes = true\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SitePolicy, parse_file};

    #[test]
    fn the_init_file_parses_to_the_defaults() {
        let parsed = parse_file(&init_file(None)).unwrap();
        assert_eq!(parsed.policy, SitePolicy::default());
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn an_address_from_the_site_is_switched_on() {
        let parsed = parse_file(&init_file(Some((HostPref::Www, Slash::With)))).unwrap();
        let a = parsed.policy.seo.address.unwrap();
        assert_eq!((a.host, a.trailing_slash), (HostPref::Www, Slash::With));
    }
}
