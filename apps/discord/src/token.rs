//! The bot token and the webhook tokens live in the host's secret store,
//! never in the record (bots.md §6). The token is set on the host through
//! the `set-bot-token` effect; the worker asks the host for it.

use ui::secrets::SecretStore;

pub const TOKEN_PURPOSE: &str = "bot-token";
pub const TOKEN_ITEM: &str = "default";
pub const WEBHOOK_PURPOSE: &str = "webhook";

#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    Shape,
    Store(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shape => f.write_str("that does not look like a Discord bot token"),
            Self::Store(error) => write!(f, "the host's secret store refused the token: {error}"),
        }
    }
}

impl std::error::Error for TokenError {}

/// Three non-empty dot-separated parts of base64url characters.
#[must_use]
pub fn valid_token_shape(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

/// Store the bot token.
///
/// # Errors
/// [`TokenError::Shape`] for something that is not a token, or the store's error.
pub fn store_token(store: &SecretStore, token: &str) -> Result<(), TokenError> {
    let token = token.trim();
    if !valid_token_shape(token) {
        return Err(TokenError::Shape);
    }
    store
        .item(TOKEN_PURPOSE, TOKEN_ITEM)
        .put_blocking(token.as_bytes())
        .map_err(|error| TokenError::Store(error.to_string()))
}

#[must_use]
pub fn token_present(store: &SecretStore) -> bool {
    matches!(
        store.item(TOKEN_PURPOSE, TOKEN_ITEM).get_blocking(),
        Ok(Some(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPED: &str = "MTA4NzY1NDMyMTA5ODc2NTQzMg.GxYzAb.abcdefghijklmnopqrstuvwxyz0123456789AB";

    #[test]
    fn a_token_is_stored_under_the_host_purpose() {
        let store = SecretStore::memory(crate::APP_ID);
        assert!(!token_present(&store));
        store_token(&store, &format!("  {SHAPED}\n")).unwrap();
        assert!(token_present(&store));
        let stored = store
            .item(TOKEN_PURPOSE, TOKEN_ITEM)
            .get_blocking()
            .unwrap()
            .unwrap();
        assert_eq!(stored.expose(), SHAPED.as_bytes(), "trimmed");
    }

    #[test]
    fn a_malformed_token_is_refused_without_echoing_it() {
        let store = SecretStore::memory(crate::APP_ID);
        let error = store_token(&store, "not-a-token-secret-value").unwrap_err();
        assert!(!error.to_string().contains("secret-value"));
        assert!(!token_present(&store));
        assert!(!valid_token_shape("a.b"));
        assert!(valid_token_shape(SHAPED));
    }
}
