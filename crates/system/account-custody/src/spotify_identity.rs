//! Bind native Spotify authorization to a Web API account, never a login name.
use std::io::Read as _;
use std::time::Duration;

/// Provider identity used to bind independently consented Spotify grants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpotifyIdentity {
    /// Immutable account-linking identity returned by modern Spotify profiles.
    AccountId(String),
    /// Explicit compatibility with profiles predating `account_id`.
    LegacyUserId(String),
}

impl SpotifyIdentity {
    /// Resolve a provider profile without treating either ID as a native username.
    /// # Errors
    /// The profile omits a valid bounded identity or supplies an invalid account ID.
    pub fn from_profile(profile: &serde_json::Value) -> Result<Self, crate::CustodyError> {
        fn valid(value: &str) -> bool {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        }
        if let Some(value) = profile.get("account_id") {
            return value
                .as_str()
                .filter(|v| valid(v))
                .map(|v| Self::AccountId(v.into()))
                .ok_or(crate::CustodyError::NeedsAuthentication);
        }
        profile
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|v| valid(v))
            .map(|v| Self::LegacyUserId(v.into()))
            .ok_or(crate::CustodyError::NeedsAuthentication)
    }
}

/// Confirm the exact native-session access token belongs to the expected account.
pub fn with_verified_spotify_access_token<T>(
    expected: &SpotifyIdentity,
    token: &str,
    consume: impl FnOnce(&str) -> T,
) -> Result<T, crate::CustodyError> {
    verified_at(expected, token, "https://api.spotify.com/v1/me", consume)
}

fn verified_at<T>(
    expected: &SpotifyIdentity,
    token: &str,
    endpoint: &str,
    consume: impl FnOnce(&str) -> T,
) -> Result<T, crate::CustodyError> {
    verify_at(expected, token, endpoint)?;
    Ok(consume(token))
}

fn verify_at(
    expected: &SpotifyIdentity,
    token: &str,
    endpoint: &str,
) -> Result<(), crate::CustodyError> {
    const LIMIT: u64 = 64 * 1024;
    if token.is_empty() {
        return Err(crate::CustodyError::NeedsAuthentication);
    }
    let client = tilo_net::http::blocking_client()
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| crate::CustodyError::Offline)?;
    let response = client
        .get(endpoint)
        .bearer_auth(token)
        .send()
        .map_err(|_| crate::CustodyError::Offline)?;
    if !response.status().is_success() || response.content_length().is_some_and(|v| v > LIMIT) {
        return Err(crate::CustodyError::NeedsAuthentication);
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    response
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| crate::CustodyError::Offline)?;
    if bytes.len() as u64 > LIMIT {
        return Err(crate::CustodyError::NeedsAuthentication);
    }
    let profile: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| crate::CustodyError::NeedsAuthentication)?;
    let matches = match expected {
        SpotifyIdentity::AccountId(id) => SpotifyIdentity::from_profile(&profile)
            .is_ok_and(|actual| actual == SpotifyIdentity::AccountId(id.clone())),
        SpotifyIdentity::LegacyUserId(id) => {
            profile.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())
        }
    };
    if !matches {
        return Err(crate::CustodyError::NeedsAuthentication);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_http::{Reply, Responder};

    #[test]
    fn exact_native_token_is_verified_before_consumer_and_opaque_account_is_not_a_username() {
        let api = Responder::start(vec![
            Reply::status(200)
                .body(br#"{"id":"visible-user","account_id":"other-account"}"#.to_vec()),
            Reply::status(200)
                .body(br#"{"id":"visible-user","account_id":"immutable-opaque"}"#.to_vec()),
        ]);
        let expected = SpotifyIdentity::AccountId("immutable-opaque".into());
        let native_started = std::cell::Cell::new(false);
        let refused = verified_at(&expected, "exact-native-token", &api.url(), |_| {
            native_started.set(true);
        });
        assert_eq!(refused, Err(crate::CustodyError::NeedsAuthentication));
        assert!(!native_started.get());
        // The authenticated native protocol can supply a different canonical
        // username. Consumer construction follows exact-token account proof,
        // without requiring equality between these separate identity domains.
        let native_name = verified_at(&expected, "exact-native-token", &api.url(), |token| {
            assert_eq!(token, "exact-native-token");
            native_started.set(true);
            "native-canonical-username"
        })
        .unwrap();
        assert!(native_started.get());
        assert_eq!(native_name, "native-canonical-username");
        for _ in 0..2 {
            let request = api.next_request();
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.header("Authorization"),
                Some("Bearer exact-native-token")
            );
        }
        api.assert_untouched();
    }

    #[test]
    fn legacy_user_identity_is_explicit_and_invalid_modern_identity_cannot_fall_back() {
        let legacy =
            SpotifyIdentity::from_profile(&serde_json::json!({"id":"legacy-user"})).unwrap();
        assert_eq!(legacy, SpotifyIdentity::LegacyUserId("legacy-user".into()));
        for account in [
            serde_json::json!(""),
            serde_json::json!(null),
            serde_json::json!("bad\nidentity"),
        ] {
            assert_eq!(
                SpotifyIdentity::from_profile(
                    &serde_json::json!({"id":"legacy-user","account_id":account})
                ),
                Err(crate::CustodyError::NeedsAuthentication)
            );
        }
        let api = Responder::start(vec![
            Reply::status(200)
                .body(br#"{"id":"legacy-user","account_id":"modern-account"}"#.to_vec()),
        ]);
        verified_at(&legacy, "legacy-token", &api.url(), |token| {
            assert_eq!(token, "legacy-token");
        })
        .unwrap();
        api.next_request();
        api.assert_untouched();
    }
}
