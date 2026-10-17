//! Discovery before an account has been committed to custody.

#[cfg(test)]
mod tests;

use account_custody::{Authorization, Custodian, TokenSet};
use zeroize::Zeroize;

use super::{Binding, CALENDAR_SCOPE, GOOGLE_SERVER, options};
use crate::{Credentials, DavClient, Url, discovery};
use discovery::CalendarCollection;

/// A completed, session-private Google calendar authorization. No token or
/// unrestricted HTTP client is exposed while resolving calendar identity.
pub struct PendingGoogleCalendar {
    tokens: TokenSet,
}

impl PendingGoogleCalendar {
    /// Redeem the existing browser authorization on a worker thread.
    /// The authorization must request Google's calendar scope.
    ///
    /// # Errors
    /// Invalid authorization, failed exchange or a narrowed calendar grant.
    pub fn complete(authorization: Authorization) -> Result<Self, String> {
        let url = Url::parse(authorization.url()).map_err(|_| "invalid authorization URL")?;
        let expected = account_custody::OAuthIssuer::Google.endpoints();
        let expected = Url::parse(&expected.authorization_endpoint)
            .map_err(|_| "invalid Google authorization URL")?;
        if url.origin() != expected.origin()
            || url.path() != expected.path()
            || !url.query_pairs().any(|(key, value)| {
                key == "scope"
                    && value
                        .split_whitespace()
                        .any(|scope| scope == CALENDAR_SCOPE)
            })
        {
            return Err("Google calendar authorization is required".into());
        }
        let tokens = Custodian::finish_sign_in(authorization).map_err(|error| error.to_string())?;
        let pending = Self { tokens };
        if pending.tokens.scope.as_deref().is_some_and(|scopes| {
            !scopes
                .split_whitespace()
                .any(|scope| scope == CALENDAR_SCOPE)
        }) {
            return Err("Google account has no calendar grant".into());
        }
        Ok(pending)
    }

    /// Resolve the authenticated primary calendar and discover its collections.
    /// Returns the provider-confirmed ID, DAV endpoint and collection metadata.
    /// Blocking; call off the UI thread.
    ///
    /// # Errors
    /// Identity lookup, unsafe identity or bounded DAV discovery failure.
    pub fn discover(&self) -> Result<PendingCalendars, String> {
        let client = DavClient::new(&options())
            .map_err(|error| error.to_string())?
            .with_credentials(Credentials::bearer(self.tokens.access_token.clone()));
        let response = client
            .get("https://www.googleapis.com/calendar/v3/calendars/primary")
            .map_err(|_| "could not resolve Google primary calendar")?;
        if !response.status().is_success() {
            return Err("Google primary calendar lookup failed".into());
        }
        let body = response
            .read_bounded(64 * 1024)
            .map_err(|_| "invalid Google calendar response")?;
        let value: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| "invalid Google calendar response")?;
        let id = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 2048
                    && !id.chars().any(char::is_whitespace)
                    && !id.chars().any(char::is_control)
            })
            .ok_or("invalid primary calendar identity")?;
        let mut base = Url::parse(GOOGLE_SERVER).map_err(|_| "invalid Google endpoint")?;
        base.path_segments_mut()
            .map_err(|()| "invalid Google endpoint")?
            .pop_if_empty()
            .push(id)
            .push("user");
        let binding = Binding { base, google: true };
        let collections = discovery::calendars_checked(&client, &binding.base, &|url| {
            binding.check(url.as_str()).map(|_| ())
        })?;
        Ok(PendingCalendars {
            server_url: binding.base.into(),
            username: id.into(),
            collections,
        })
    }

    /// Resolve the Google Drive mount from this combined consent grant.
    ///
    /// # Errors
    /// Missing files permission or provider lookup failure.
    pub fn resolve_storage(&self, custody: &Custodian) -> Result<serde_json::Value, String> {
        custody
            .resolve_storage(account_custody::OAuthCloud::GoogleDrive, &self.tokens)
            .map_err(|error| error.to_string())
    }

    /// Resolve the Gmail identity without exposing the combined consent grant.
    ///
    /// # Errors
    /// Missing mail permission or provider lookup failure.
    pub fn mail_identity(&self, custody: &Custodian) -> Result<String, String> {
        custody
            .gmail_identity(&self.tokens)
            .map_err(|error| error.to_string())
    }

    /// Stage this grant in the selected private vault without exposing its bytes.
    /// The caller owns the explicit save, after calendar selection is confirmed.
    pub fn stage(&self, id: &accounts::AccountId, vault: &mut accounts::SecretVault) {
        let mut secret = account_custody::oauth_secret(&self.tokens);
        vault.put(id, &secret);
        secret.password.zeroize();
        if let Some(token) = secret.token.as_mut() {
            token.zeroize();
        }
    }
}

impl Drop for PendingGoogleCalendar {
    fn drop(&mut self) {
        self.tokens.access_token.zeroize();
        self.tokens.refresh_token.zeroize();
    }
}

/// Provider metadata only; applications map this into their own UI model.
pub struct PendingCalendars {
    pub server_url: String,
    pub username: String,
    pub collections: Vec<CalendarCollection>,
}

/// A validated Nextcloud endpoint and ordinary, session-private login input.
/// Passwords and unrestricted clients remain inside this capability.
pub struct PendingNextcloudCalendar {
    binding: Binding,
    username: String,
    password: zeroize::Zeroizing<String>,
}

impl PendingNextcloudCalendar {
    /// Validate the deployment URL before accepting credentials for discovery.
    ///
    /// # Errors
    /// Invalid or unsafe Nextcloud deployment URL.
    pub fn new(server: &str, username: &str, password: &str) -> Result<Self, String> {
        let mut base =
            account_custody::nextcloud::server_url(server).map_err(|error| error.to_string())?;
        discovery::safe_url(&base, server)?;
        base.set_path(&format!(
            "{}/remote.php/dav/",
            base.path().trim_end_matches('/')
        ));
        discovery::safe_url(&base, base.as_str())?;
        Ok(Self {
            binding: Binding {
                base,
                google: false,
            },
            username: username.into(),
            password: zeroize::Zeroizing::new(password.into()),
        })
    }

    /// Discover collections with bounded, same-origin DAV requests. Blocking.
    ///
    /// # Errors
    /// Authentication, unsafe redirects or malformed discovery replies.
    pub fn discover(&self) -> Result<PendingCalendars, String> {
        let client = DavClient::new(&options())
            .map_err(|error| error.to_string())?
            .with_credentials(Credentials::new(&self.username, self.password.as_str()));
        let collections = discovery::calendars_checked(&client, &self.binding.base, &|url| {
            self.binding.check(url.as_str()).map(|_| ())
        })?;
        Ok(PendingCalendars {
            server_url: self.binding.base.to_string(),
            username: self.username.clone(),
            collections,
        })
    }
}
