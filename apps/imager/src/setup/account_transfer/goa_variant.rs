//! Bounded GOA `a{sv}` credentials decoded by the canonical GLib parser.
//!
//! The caller owns a zeroizing Secret Service response. GLib's immutable
//! temporary variants are dropped within this function and never formatted;
//! their allocator does not expose a supported zeroization API. Extracted
//! credential copies are zeroized on every rejected path.

use std::collections::BTreeSet;

use account_custody::TokenSet;
use glib::{Variant, VariantTy};
use zeroize::Zeroizing;

pub const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_FIELDS: usize = 64;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_EXPIRY_SECONDS: u64 = 253_402_300_799;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Invalid,
    LimitExceeded,
    MissingRefreshToken,
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "Selected online account credentials have an unsupported format",
            Self::LimitExceeded => "Selected online account credentials exceed migration limits",
            Self::MissingRefreshToken => "Selected online account has no usable refresh credential",
        })
    }
}
impl std::error::Error for ParseError {}

/// Preserve the GOA grant without inferring scopes or restarting its lifetime.
/// The absolute Unix expiry is represented as `obtained_at = 0` and the expiry
/// seconds in `expires_in`, preserving `TokenSet::expires_at()` exactly to the
/// destination format's second precision. Authorization codes and passwords
/// are never included in the returned grant.
///
/// # Errors
/// Refuses malformed/type-invalid dictionaries, duplicate keys, oversized
/// inputs and unusable refresh tokens. Diagnostics contain no source text.
pub fn parse(secret: &Zeroizing<Vec<u8>>) -> Result<TokenSet, ParseError> {
    if secret.len() > MAX_SECRET_BYTES {
        return Err(ParseError::LimitExceeded);
    }
    let text = std::str::from_utf8(secret).map_err(|_| ParseError::Invalid)?;
    if text.contains('\0') {
        return Err(ParseError::Invalid);
    }
    let dictionary =
        Variant::parse(Some(VariantTy::VARDICT), text).map_err(|_| ParseError::Invalid)?;
    if dictionary.n_children() > MAX_FIELDS {
        return Err(ParseError::LimitExceeded);
    }
    let mut keys = BTreeSet::new();
    let mut access = Zeroizing::new(String::new());
    let mut refresh = Zeroizing::new(String::new());
    let mut expiry = 0;
    for index in 0..dictionary.n_children() {
        let entry = dictionary
            .try_child_value(index)
            .ok_or(ParseError::Invalid)?;
        let key = entry.try_child_value(0).ok_or(ParseError::Invalid)?;
        let key = key.str().ok_or(ParseError::Invalid)?;
        if key.len() > 256 || !keys.insert(key.to_owned()) {
            return Err(ParseError::Invalid);
        }
        let value = entry
            .try_child_value(1)
            .and_then(|value| value.as_variant())
            .ok_or(ParseError::Invalid)?;
        match key {
            "access_token" => access = token(&value)?,
            "refresh_token" => refresh = token(&value)?,
            "access_token_expires_at" => expiry = absolute_expiry(&value)?,
            _ => {}
        }
    }
    if refresh.trim().is_empty() {
        return Err(ParseError::MissingRefreshToken);
    }
    Ok(TokenSet {
        access_token: std::mem::take(&mut *access),
        refresh_token: std::mem::take(&mut *refresh),
        obtained_at: 0,
        expires_in: expiry,
        scope: None,
    })
}

fn token(value: &Variant) -> Result<Zeroizing<String>, ParseError> {
    if value.type_() != VariantTy::STRING {
        return Err(ParseError::Invalid);
    }
    let value = value.str().ok_or(ParseError::Invalid)?;
    if value.len() > MAX_TOKEN_BYTES {
        return Err(ParseError::LimitExceeded);
    }
    if value.chars().any(char::is_control) {
        return Err(ParseError::Invalid);
    }
    Ok(Zeroizing::new(value.to_owned()))
}
fn absolute_expiry(value: &Variant) -> Result<u64, ParseError> {
    let microseconds = value.get::<i64>().ok_or(ParseError::Invalid)?;
    let microseconds = u64::try_from(microseconds).map_err(|_| ParseError::Invalid)?;
    let seconds = microseconds
        .checked_div(1_000_000)
        .ok_or(ParseError::Invalid)?;
    if seconds == 0 || seconds > MAX_EXPIRY_SECONDS {
        return Err(ParseError::Invalid);
    }
    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn secret(text: &str) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(text.as_bytes().to_vec())
    }

    #[test]
    fn actual_glib_dictionary_preserves_escaping_tokens_and_absolute_expiry() {
        let input = secret(
            r#"{'authorization_code': <'discard-code'>, 'access_token': <"access'quote\\tail">, 'refresh_token': <"refresh'quote\\tail">, 'access_token_expires_at': <int64 2000000123456789>, 'password': <'discard-password'>, 'other': <true>}"#,
        );
        let parsed = parse(&input).expect("canonical upstream a{sv}");
        assert_eq!(parsed.access_token, "access'quote\\tail");
        assert_eq!(parsed.refresh_token, "refresh'quote\\tail");
        assert_eq!(parsed.obtained_at, 0);
        assert_eq!(parsed.expires_at(), Some(2_000_000_123));
        assert_eq!(parsed.scope, None);
        let debug = format!("{parsed:?}");
        assert!(!debug.contains("refresh'quote"));
        assert!(!debug.contains("access'quote"));
    }

    #[test]
    fn refresh_only_does_not_invent_scopes_or_expiry() {
        let parsed =
            parse(&secret("{'refresh_token': <'refresh-only'>}")).expect("refreshable grant");
        assert!(parsed.access_token.is_empty());
        assert_eq!(parsed.refresh_token, "refresh-only");
        assert_eq!(parsed.expires_at(), None);
        assert_eq!(parsed.scope, None);
        assert_eq!(
            parse(&secret("{'password': <'not-a-refresh-token'>}"))
                .expect_err("password is not a grant"),
            ParseError::MissingRefreshToken
        );
        assert_eq!(
            parse(&secret("{'refresh_token': <'   '>}")).expect_err("unusable refresh token"),
            ParseError::MissingRefreshToken
        );
    }

    #[test]
    fn malformed_wrong_type_duplicates_and_expiry_refuse_without_secret_diagnostics() {
        for text in [
            "{'refresh_token': <'secret-unterminated>}",
            "['secret-wrong-container']",
            "{'refresh_token': <int64 12>}",
            "{'refresh_token': <objectpath '/not_a_token'>}",
            "{'refresh_token': <'secret'>}\0ignored",
            "{'refresh_token': <'first'>, 'refresh_token': <'second'>}",
            "{'refresh_token': <'secret'>, 'access_token_expires_at': <'123'>}",
            "{'refresh_token': <'secret'>, 'access_token_expires_at': <int64 -1>}",
            "{'refresh_token': <'secret'>, 'access_token_expires_at': <int64 0>}",
            "{'refresh_token': <'secret\\ncontrol'>}",
        ] {
            let error = parse(&secret(text)).expect_err("invalid source grant");
            assert_eq!(error, ParseError::Invalid);
            assert!(!error.to_string().contains("secret"));
        }
        assert_eq!(
            parse(&Zeroizing::new(vec![0xff])).expect_err("non UTF-8 response"),
            ParseError::Invalid
        );
    }

    #[test]
    fn malformed_secret_sentinel_never_enters_error_debug_or_display() {
        let sentinel = "synthetic-private-sentinel-4af8";
        let input = secret(&format!("{{'refresh_token': <'{sentinel}>}}"));
        let error = parse(&input).expect_err("malformed private source response");
        assert_eq!(error, ParseError::Invalid);
        assert!(!format!("{error:?}").contains(sentinel));
        assert!(!error.to_string().contains(sentinel));
    }

    #[test]
    fn oversized_input_token_and_dictionary_refuse() {
        assert_eq!(
            parse(&Zeroizing::new(vec![
                b' ';
                MAX_SECRET_BYTES.saturating_add(1)
            ]))
            .expect_err("bounded response"),
            ParseError::LimitExceeded
        );
        let token = "x".repeat(MAX_TOKEN_BYTES.saturating_add(1));
        assert_eq!(
            parse(&secret(&format!("{{'refresh_token': <'{token}'>}}")))
                .expect_err("bounded token"),
            ParseError::LimitExceeded
        );
        let fields = (0..MAX_FIELDS.saturating_add(1))
            .map(|index| format!("'field{index}': <true>"))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            parse(&secret(&format!("{{{fields}}}"))).expect_err("bounded dictionary"),
            ParseError::LimitExceeded
        );
    }
}
