//! Generating and recognising API keys. A key is `cdo_` and 43 characters of URL-safe base64
//! (256 random bits). Only its SHA-256 hash and a 12-character prefix are stored, so the key
//! exists in the clear once, in the response that creates it.

use crate::auth::session::{hash, random_token};
use crate::config::Config;
use crate::error::AppError;

/// Every key starts with this.
pub const KEY_PREFIX: &str = "cdo_";
/// Characters of the key kept (unhashed) to tell keys apart on the settings screen.
pub const DISPLAY_PREFIX_LEN: usize = 12;
/// `cdo_` and the 43 characters of 32 random bytes.
const KEY_LEN: usize = KEY_PREFIX.len() + 43;

/// A new key: the plaintext (show once, never store), its hash and its display prefix.
pub struct NewKey {
    pub plaintext: String,
    pub hash: Vec<u8>,
    pub prefix: String,
}

pub fn generate() -> NewKey {
    let plaintext = format!("{KEY_PREFIX}{}", random_token());
    NewKey {
        hash: hash_key(&plaintext),
        prefix: plaintext.chars().take(DISPLAY_PREFIX_LEN).collect(),
        plaintext,
    }
}

/// What is stored and looked up instead of the key.
pub fn hash_key(key: &str) -> Vec<u8> {
    hash(key)
}

/// Whether `key` has the shape of a key we hand out. A value that doesn't is rejected before
/// any lookup.
pub fn is_well_formed(key: &str) -> bool {
    key.len() == KEY_LEN
        && key.starts_with(KEY_PREFIX)
        && key[KEY_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The MCP server's address, `{BASE_URL}/mcp`.
pub fn mcp_url(config: &Config) -> Result<String, AppError> {
    Ok(config
        .base_url
        .join("mcp")
        .map_err(AppError::internal)?
        .to_string())
}

/// `claude mcp add ...` for the key (or a placeholder).
pub fn claude_command(mcp_url: &str, key: &str) -> String {
    format!(
        "claude mcp add --transport http codoseo {mcp_url} --header \"Authorization: Bearer {key}\""
    )
}

/// A `mcpServers` entry for MCP clients that take JSON, written out so the keys stay in the
/// order a person reads them.
pub fn mcp_servers_json(mcp_url: &str, key: &str) -> String {
    // The URL and the key are plain ASCII with no quotes or backslashes.
    format!(
        "{{\n  \"mcpServers\": {{\n    \"codoseo\": {{\n      \"type\": \"http\",\n      \
         \"url\": \"{mcp_url}\",\n      \"headers\": {{\n        \
         \"Authorization\": \"Bearer {key}\"\n      }}\n    }}\n  }}\n}}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_json_snippet_is_valid_and_carries_the_url_and_the_bearer_key() {
        let key = "cdo_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let json = mcp_servers_json("https://codoseo.com/mcp", key);
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        let server = &v["mcpServers"]["codoseo"];
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "https://codoseo.com/mcp");
        assert_eq!(server["headers"]["Authorization"], format!("Bearer {key}"));
        // Written in reading order, not alphabetically.
        assert!(json.find("\"type\"").unwrap() < json.find("\"url\"").unwrap());
        assert!(json.find("\"url\"").unwrap() < json.find("\"headers\"").unwrap());
    }

    #[test]
    fn the_claude_command_names_the_url_and_the_bearer_key() {
        assert_eq!(
            claude_command("https://codoseo.com/mcp", "cdo_x"),
            "claude mcp add --transport http codoseo https://codoseo.com/mcp --header \"Authorization: Bearer cdo_x\""
        );
    }

    #[test]
    fn a_key_is_cdo_and_43_url_safe_characters() {
        let key = generate();
        assert!(key.plaintext.starts_with("cdo_"));
        assert_eq!(key.plaintext.len(), 4 + 43);
        assert!(is_well_formed(&key.plaintext));
    }

    #[test]
    fn the_hash_is_sha256_of_the_whole_key_and_the_prefix_its_first_12_characters() {
        let key = generate();
        assert_eq!(key.hash, hash_key(&key.plaintext));
        assert_eq!(key.hash.len(), 32);
        assert_eq!(key.prefix, key.plaintext[..12]);
    }

    #[test]
    fn keys_do_not_repeat() {
        assert_ne!(generate().plaintext, generate().plaintext);
        assert_ne!(generate().hash, generate().hash);
    }

    #[test]
    fn malformed_keys_are_not_well_formed() {
        for bad in [
            "",
            "cdo_",
            "cdo_short",
            "xyz_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "cdo_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "cdo_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA ",
            "cdo_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA+",
        ] {
            assert!(!is_well_formed(bad), "{bad:?}");
        }
        assert!(is_well_formed(
            "cdo_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        ));
    }
}
