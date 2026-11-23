//! Calendar access with the sign-in the session released for an account: a password, or the
//! access token of the grant a provider's own page left. The grant itself, and its renewal, stay
//! with the workspace's vault; this side only binds what it was handed to the account's server.
//! Credentials and unrestricted clients never leave here.

use crate::discovery;
use crate::net::{self, DavResponse, Depth, Precondition, Url};
use crate::{Credentials, Options, PropName, Redirects};

mod pending;
pub use pending::{PendingCalendars, PendingGoogleCalendar, PendingNextcloudCalendar};

const GOOGLE_SERVER: &str = "https://apidata.googleusercontent.com/caldav/v2/";
const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar";

/// What a Google calendar is told when the session released no grant for it.
const NO_GRANT: &str = "Google account has no calendar grant; sign in for calendar access";

#[derive(Clone, Default)]
pub struct Service {
    #[cfg(feature = "test-support")]
    fixture: Option<Url>,
}

impl Service {
    pub fn live() -> Self {
        Self::default()
    }

    /// Trust exactly one loopback origin as a Google fixture. This is explicit per
    /// service and does not permit a second loopback server or arbitrary URL.
    #[cfg(feature = "test-support")]
    pub fn for_test_origin(origin: &str) -> Result<Self, String> {
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
        Ok(Self { fixture: Some(url) })
    }

    /// Build an opaque origin-bound async client signed in with `secret`, what the session
    /// released for the account: a Google calendar's grant (`google`) as its bearer access
    /// token, bound to Google's `CalDAV` endpoint, and any other calendar's password with
    /// `username`. Every check precedes the credential's use.
    pub fn authorised(
        &self,
        google: bool,
        server_url: &str,
        username: &str,
        secret: &str,
        target: &str,
    ) -> Result<AuthorisedClient, String> {
        let binding = Self::binding(
            server_url,
            google,
            #[cfg(feature = "test-support")]
            self.fixture.as_ref(),
        )?;
        binding.check(target)?;
        let credentials = if google {
            if secret.is_empty() {
                return Err(NO_GRANT.into());
            }
            Credentials::bearer(secret)
        } else {
            Credentials::new(username, secret)
        };
        let client =
            net::DavClient::with_fetch_proxy(&options(), tilo_net::proxy::ProxyUse::CrossOrigin)
                .map_err(|error| error.to_string())?
                .with_credentials(credentials);
        Ok(AuthorisedClient { client, binding })
    }

    fn binding(
        server: &str,
        google: bool,
        #[cfg(feature = "test-support")] fixture: Option<&Url>,
    ) -> Result<Binding, String> {
        let base = Url::parse(server).map_err(|_| "invalid calendar server URL")?;
        discovery::safe_url(&base, server)?;
        #[cfg(feature = "test-support")]
        let google = google && !fixture.is_some_and(|fixture| fixture.origin() == base.origin());
        let binding = Binding { base, google };
        binding.check(server)?;
        Ok(binding)
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

/// An opaque DAV capability bound to its configured origin. Every operation validates
/// its own target, and automatic redirects are disabled.
pub struct AuthorisedClient {
    client: net::DavClient,
    binding: Binding,
}

// Each operation returns its future boxed: an `async fn` called from
// another crate's async code is compiled into that crate, so Calendar would
// carry the request, the transport and the parsing it awaits, which this
// library holds once in the accounts engine.
impl AuthorisedClient {
    pub fn report<'a>(
        &'a self,
        url: &'a str,
        depth: Depth,
        body: String,
    ) -> net::DavFuture<'a, Result<DavResponse, String>> {
        Box::pin(async move {
            let url = self.binding.check(url)?;
            self.client
                .report(url.as_str(), depth, body)
                .await
                .map_err(|error| error.to_string())
        })
    }

    pub fn propfind<'a>(
        &'a self,
        url: &'a str,
        depth: Depth,
        props: &'a [PropName],
    ) -> net::DavFuture<'a, Result<DavResponse, String>> {
        Box::pin(async move {
            let url = self.binding.check(url)?;
            self.client
                .propfind(url.as_str(), depth, props)
                .await
                .map_err(|error| error.to_string())
        })
    }

    pub fn put<'a>(
        &'a self,
        url: &'a str,
        body: Vec<u8>,
        content_type: Option<&'a str>,
        precondition: Precondition<'a>,
    ) -> net::DavFuture<'a, Result<DavResponse, String>> {
        Box::pin(async move {
            let url = self.binding.check(url)?;
            self.client
                .put(url.as_str(), body, content_type, precondition)
                .await
                .map_err(|error| error.to_string())
        })
    }

    pub fn delete<'a>(
        &'a self,
        url: &'a str,
        precondition: Precondition<'a>,
    ) -> net::DavFuture<'a, Result<DavResponse, String>> {
        self.delete_saying(url, &[], precondition)
    }

    /// [`net::DavClient::delete_saying`], for this account's server only.
    pub fn delete_saying<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(&'a str, &'a str)],
        precondition: Precondition<'a>,
    ) -> net::DavFuture<'a, Result<DavResponse, String>> {
        Box::pin(async move {
            let url = self.binding.check(url)?;
            self.client
                .delete_saying(url.as_str(), headers, precondition)
                .await
                .map_err(|error| error.to_string())
        })
    }

    /// What the server holds at `url`, for this account's server only.
    pub fn get<'a>(&'a self, url: &'a str) -> net::DavFuture<'a, Result<DavResponse, String>> {
        Box::pin(async move {
            let url = self.binding.check(url)?;
            self.client
                .get(url.as_str())
                .await
                .map_err(|error| error.to_string())
        })
    }
}

#[cfg(test)]
mod tests;
