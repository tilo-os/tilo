//! Custody-backed calendar access. Refresh and discovery are blocking; run them
//! off the async/UI thread. Credentials and unrestricted clients never leave here.

use account_custody::{Custodian, OAuthIssuer, OAuthUse};
use accounts::{AccountId, AccountStore, SecretVault, cloud_types};

use crate::discovery::{self, CalendarCollection};
use crate::net::{self, DavResponse, Depth, Precondition, Url};
use crate::{Credentials, Options, PropName, Redirects};

mod pending;
pub use pending::{PendingCalendars, PendingGoogleCalendar, PendingNextcloudCalendar};

const GOOGLE_SERVER: &str = "https://apidata.googleusercontent.com/caldav/v2/";
const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar";

/// Read the password for the existing portable Nextcloud calendar projection,
/// including protected copied grants. The caller must keep this value private.
pub fn calendar_password(secret: &accounts::Secret) -> String {
    account_custody::password_credential(secret).to_string()
}

/// A reusable service holding the signed-in user's credential custodian.
#[derive(Clone)]
pub struct Service {
    custodian: Custodian,
    #[cfg(feature = "test-support")]
    fixture: Option<Url>,
}

impl Service {
    /// Use the signed-in user's stores.
    pub fn live() -> Self {
        Self::new(Custodian::live())
    }

    /// Use an explicit custodian, including isolated integration-fixture stores.
    pub fn new(custodian: Custodian) -> Self {
        Self {
            custodian,
            #[cfg(feature = "test-support")]
            fixture: None,
        }
    }

    /// Trust exactly one loopback origin as a Google fixture. This is explicit
    /// per service and does not permit a second loopback server or arbitrary URL.
    ///
    /// # Errors
    /// If the fixture is not an HTTP(S) loopback origin without URL metadata.
    #[cfg(feature = "test-support")]
    pub fn for_test_origin(custodian: Custodian, origin: &str) -> Result<Self, String> {
        let url = Url::parse(origin).map_err(|_| "invalid fixture origin")?;
        discovery::safe_url(&url, origin)?;
        let loopback = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !loopback || url.path() != "/" {
            return Err("fixture must be a loopback origin".into());
        }
        Ok(Self {
            custodian,
            fixture: Some(url),
        })
    }

    /// Discover collections with a canonical Google or Nextcloud credential.
    /// Google requires the calendar grant, not a mail/files grant.
    ///
    /// # Errors
    /// Missing account/grant, unsafe endpoints, refresh or discovery failures.
    pub fn discover(
        &self,
        id: &AccountId,
        server_url: &str,
        username: &str,
        google: bool,
    ) -> Result<Vec<CalendarCollection>, String> {
        let binding = self.binding(server_url, google)?;
        let credentials = if google {
            self.google_credential(id)?
        } else {
            self.require_type(id, cloud_types::NEXTCLOUD)?;
            let vault = match self.custodian.config_root() {
                Some(root) => SecretVault::open_in(root),
                None => SecretVault::open(),
            };
            let secret = vault.get(id).ok_or("calendar credential is missing")?;
            let password = account_custody::password_credential(&secret);
            if password.is_empty() {
                return Err("calendar credential is unavailable".into());
            }
            Credentials::new(username, password.to_string())
        };
        let client = crate::DavClient::new(&options())
            .map_err(|error| error.to_string())?
            .with_credentials(credentials);
        discovery::calendars_checked(&client, &binding.base, &|url| {
            binding.check(url.as_str()).map(|_| ())
        })
    }

    /// Build an opaque origin-bound async client. An account ID selects Google
    /// custody; without one, use the supplied Basic password. Checks precede any
    /// refresh or credential use. Refresh is blocking.
    ///
    /// # Errors
    /// Unsafe URLs, missing/wrong grants or accounts, refresh/transport failures.
    pub fn authorised(
        &self,
        id: Option<&str>,
        server_url: &str,
        username: &str,
        password: &str,
        target: &str,
    ) -> Result<AuthorisedClient, String> {
        let binding = self.binding(server_url, id.is_some())?;
        binding.check(target)?;
        let credentials = match id {
            Some(id) => self.google_credential(&AccountId::from(id))?,
            None => Credentials::new(username, password),
        };
        let client =
            net::DavClient::with_fetch_proxy(&options(), tilo_net::proxy::ProxyUse::CrossOrigin)
                .map_err(|error| error.to_string())?
                .with_credentials(credentials);
        Ok(AuthorisedClient { client, binding })
    }

    fn binding(&self, server: &str, google: bool) -> Result<Binding, String> {
        let base = Url::parse(server).map_err(|_| "invalid calendar server URL")?;
        discovery::safe_url(&base, server)?;
        #[cfg(feature = "test-support")]
        let google = google
            && !self
                .fixture
                .as_ref()
                .is_some_and(|fixture| fixture.origin() == base.origin());
        let binding = Binding { base, google };
        binding.check(server)?;
        Ok(binding)
    }

    fn require_type(&self, id: &AccountId, expected: &str) -> Result<(), String> {
        let store = match self.custodian.config_root() {
            Some(root) => AccountStore::load_in(root),
            None => AccountStore::load(),
        };
        let account = store
            .get(id)
            .ok_or("canonical calendar account is missing")?;
        if !account.enabled {
            return Err("calendar account is disabled".into());
        }
        let matches = if expected == cloud_types::GOOGLE_DRIVE {
            account.type_id.as_str() == cloud_types::GOOGLE_DRIVE
                || (account.type_id.as_str() == "caldav"
                    && account
                        .config
                        .get("calendar_provider")
                        .and_then(serde_json::Value::as_str)
                        == Some("google"))
        } else {
            account.type_id.as_str() == expected
        };
        if !matches {
            return Err("account type does not support this calendar provider".into());
        }
        Ok(())
    }

    fn google_credential(&self, id: &AccountId) -> Result<Credentials, String> {
        self.require_type(id, cloud_types::GOOGLE_DRIVE)?;
        self.require_calendar_scope(id)?;
        let token = self
            .custodian
            .oauth_access_token(id, OAuthUse::calendar(OAuthIssuer::Google), false)
            .map_err(|error| error.to_string())?;
        // A refresh response can narrow the grant. Never transmit that token.
        self.require_calendar_scope(id)?;
        Ok(Credentials::bearer(token.to_string()))
    }

    fn require_calendar_scope(&self, id: &AccountId) -> Result<(), String> {
        let scopes = self
            .custodian
            .oauth_scopes(id)
            .map_err(|error| error.to_string())?;
        if !scopes.as_deref().is_some_and(|scopes| {
            scopes
                .split_whitespace()
                .any(|scope| scope == CALENDAR_SCOPE)
        }) {
            return Err("Google account has no calendar grant; sign in for calendar access".into());
        }
        Ok(())
    }
}

fn options() -> Options {
    Options {
        user_agent: Some("Tilo-Calendar/0.1"),
        timeout: std::time::Duration::from_secs(20),
        redirects: Redirects::Refuse,
        ..Options::default()
    }
}

struct Binding {
    base: Url,
    google: bool,
}

impl Binding {
    fn check(&self, target: &str) -> Result<Url, String> {
        let url = discovery::safe_url(&self.base, target)?;
        if self.google {
            let google = Url::parse(GOOGLE_SERVER).map_err(|_| "invalid Google endpoint")?;
            if url.origin() != google.origin()
                || !(url.path() == "/caldav/v2" || url.path().starts_with("/caldav/v2/"))
            {
                return Err(
                    "Google calendar credentials require the Google CalDAV v2 endpoint".into(),
                );
            }
        }
        Ok(url)
    }
}

/// An opaque DAV capability bound to its configured origin. Every operation
/// validates its own target, and automatic redirects are disabled.
pub struct AuthorisedClient {
    client: net::DavClient,
    binding: Binding,
}

impl AuthorisedClient {
    /// `REPORT` with an XML body.
    ///
    /// # Errors
    /// Unsafe target or transport failure.
    pub async fn report(
        &self,
        url: &str,
        depth: Depth,
        body: String,
    ) -> Result<DavResponse, String> {
        let url = self.binding.check(url)?;
        self.client
            .report(url.as_str(), depth, body)
            .await
            .map_err(|error| error.to_string())
    }

    /// `PROPFIND` for properties at depth.
    ///
    /// # Errors
    /// Unsafe target or transport failure.
    pub async fn propfind(
        &self,
        url: &str,
        depth: Depth,
        props: &[PropName],
    ) -> Result<DavResponse, String> {
        let url = self.binding.check(url)?;
        self.client
            .propfind(url.as_str(), depth, props)
            .await
            .map_err(|error| error.to_string())
    }

    /// Conditional `PUT`.
    ///
    /// # Errors
    /// Unsafe target or transport failure.
    pub async fn put(
        &self,
        url: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, String> {
        let url = self.binding.check(url)?;
        self.client
            .put(url.as_str(), body, content_type, precondition)
            .await
            .map_err(|error| error.to_string())
    }

    /// Conditional `DELETE`.
    ///
    /// # Errors
    /// Unsafe target or transport failure.
    pub async fn delete(
        &self,
        url: &str,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, String> {
        let url = self.binding.check(url)?;
        self.client
            .delete(url.as_str(), precondition)
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests;
