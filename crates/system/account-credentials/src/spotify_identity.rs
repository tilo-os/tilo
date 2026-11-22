//! Bind native Spotify authorization to a Web API account, never a login name.

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
    pub fn from_profile(profile: &serde_json::Value) -> Result<Self, crate::CredentialError> {
        fn valid(value: &str) -> bool {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        }
        if let Some(value) = profile.get("account_id") {
            return value
                .as_str()
                .filter(|v| valid(v))
                .map(|v| Self::AccountId(v.into()))
                .ok_or(crate::CredentialError::NeedsAuthentication);
        }
        profile
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|v| valid(v))
            .map(|v| Self::LegacyUserId(v.into()))
            .ok_or(crate::CredentialError::NeedsAuthentication)
    }
}
