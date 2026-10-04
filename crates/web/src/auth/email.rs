//! Email addresses: a light validity check, and the canonical form used to find an account.
//!
//! Review focus 5: one person using variants (`Ana@Gmail.com`, `a.na+seo@gmail.com`) to get
//! extra free accounts. The canonical form lowercases the address, drops a `+tag` on every
//! domain, and for Gmail also removes dots and maps `googlemail.com` to `gmail.com`.

/// Trims and checks that `raw` looks like `local@domain.tld`. Returns the trimmed address.
pub fn parse(raw: &str) -> Option<&str> {
    let email = raw.trim();
    if email.len() > 254 || email.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (local, domain) = email.rsplit_once('@')?;
    let domain_ok = domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
        && domain
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-');
    (!local.is_empty() && !local.contains('@') && domain_ok).then_some(email)
}

/// The account lookup key for an address already accepted by [`parse`].
pub fn canonical(email: &str) -> String {
    let lower = email.trim().to_lowercase();
    let Some((local, domain)) = lower.rsplit_once('@') else {
        return lower;
    };
    let local = local.split('+').next().unwrap_or(local);
    let (local, domain) = match domain {
        "gmail.com" | "googlemail.com" => (local.replace('.', ""), "gmail.com"),
        _ => (local.to_owned(), domain),
    };
    format!("{local}@{domain}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_variants_share_one_canonical_form() {
        let a = canonical("Ana@Gmail.com");
        assert_eq!(a, "ana@gmail.com");
        assert_eq!(canonical("a.na+seo@gmail.com"), a);
        assert_eq!(canonical("A.N.A@googlemail.com"), a);
    }

    #[test]
    fn other_domains_keep_dots_but_drop_tags() {
        assert_eq!(
            canonical("First.Last+x@Example.com"),
            "first.last@example.com"
        );
        assert_ne!(canonical("a.na@example.com"), canonical("ana@example.com"));
    }

    #[test]
    fn parse_accepts_and_rejects() {
        assert_eq!(parse("  ana@example.com "), Some("ana@example.com"));
        for bad in [
            "",
            "ana",
            "ana@",
            "@example.com",
            "ana@example",
            "a b@example.com",
            "ana@ex..com",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }
}
