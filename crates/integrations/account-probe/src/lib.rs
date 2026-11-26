//! Bounded, injectable account connection probes used by resource actions. The
//! probes speak the protocols through the same clients the apps use:
//! `async-imap` (as Mail does) for IMAP and the shared `tilo-dav` client for
//! `CalDAV`/`WebDAV`. An email account's SMTP sign-in is spoken a line at a
//! time (`smtp`), so the probe carries no mail transport.
//!
//! They run wherever the account's server can be reached: on the device that
//! keeps the account, and on the machine that runs a person's workspace when
//! the account is added from a page. What a page's Mail reads and sends there
//! is its own crate (tilo-mail-session).

use std::sync::Arc;
use web_time::Duration;

use tilo_dav::{Credentials, DavClient, Depth, StatusCode, props};
use tokio::time::timeout;

#[cfg(unix)]
pub mod connect;
#[cfg(test)]
mod dav_reach_tests;
pub mod imap;
pub mod mail_servers;
mod smtp;
/// A mail server with real sockets that takes one made-up login: what a
/// rehearsal signs in to.
#[cfg(any(test, feature = "test-support"))]
pub mod stand_in;

pub trait AccountTester: Send + Sync {
    /// Resolve real calendars during setup. Test doubles must opt in explicitly.
    fn discover_calendars(
        &self,
        _account: &accounts::Account,
        _secret: &accounts::Secret,
    ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
        Err("Calendar discovery is unavailable".into())
    }

    /// Work out where the mail of `address` lives, from the address alone
    /// ([`mail_servers::find`]). `allowed` says whether a server of that name may be
    /// asked anything. Test doubles must opt in explicitly.
    fn find_mail_servers(
        &self,
        _address: &str,
        _allowed: &dyn Fn(&str) -> bool,
    ) -> Result<mail_servers::MailServers, String> {
        Err("Mail server discovery is unavailable".into())
    }

    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String>;
}

/// Sign in to the IMAP server `client` opened: with the account's bearer
/// token by XOAUTH2 where the provider signed it in on its own page, and
/// with its password otherwise. Errors never carry the credential.
async fn imap_signed_in(
    client: async_imap::Client<imap::SecureStream>,
    user: &str,
    secret: &str,
    oauth: bool,
) -> Result<async_imap::Session<imap::SecureStream>, String> {
    let signed_in = if oauth {
        let bearer = imap::Bearer {
            user: user.to_owned(),
            access_token: secret.to_owned(),
        };
        timeout(
            LiveAccountTester::NETWORK_TIMEOUT,
            client.authenticate("XOAUTH2", bearer),
        )
        .await
    } else {
        timeout(
            LiveAccountTester::NETWORK_TIMEOUT,
            client.login(user, secret),
        )
        .await
    };
    signed_in
        .map_err(|_| "IMAP server did not answer before the timeout".to_string())?
        .map_err(|(error, _)| match error {
            async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) => {
                "IMAP server rejected the saved credential".to_string()
            }
            async_imap::error::Error::Validate(_) => {
                "login contains unsupported control characters".to_string()
            }
            _ => "IMAP server closed the connection unexpectedly".to_string(),
        })
}

#[derive(Default)]
pub struct LiveAccountTester;

impl LiveAccountTester {
    const NETWORK_TIMEOUT: Duration = Duration::from_secs(8);

    fn imap_login_name(value: &str) -> Result<&str, String> {
        if value.chars().any(char::is_control) {
            return Err("login contains unsupported control characters".into());
        }
        Ok(value)
    }

    /// Secure the connection the way the account says ([`imap::open`]), read
    /// the greeting, authenticate, and log out; then sign in to the account's
    /// SMTP server and leave, so an account that reads mail is one that
    /// sends it. A TCP accept alone is deliberately not success: it says
    /// nothing about the saved credential or whether this is a mail
    /// endpoint.
    fn email(
        reach: &imap::Reach,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        // Without the system's authorities every server's certificate would be refused and
        // the server blamed: say which file this program lacks instead.
        if let Some(lacks) = tilo_net::lacks_authorities() {
            return Err(lacks);
        }
        Self::proved(reach, account, secret, tilo_net::system_roots())
    }

    /// [`Self::email`]'s proof, trusting `roots`: every connection of it is
    /// made within `reach`.
    fn proved(
        reach: &imap::Reach,
        account: &accounts::Account,
        secret: &accounts::Secret,
        roots: rustls::RootCertStore,
    ) -> Result<String, String> {
        let host = account
            .config
            .get("imap_host")
            .and_then(serde_json::Value::as_str)
            .filter(|host| !host.trim().is_empty())
            .ok_or_else(|| "account has no IMAP server".to_string())?;
        let port = account
            .config
            .get("imap_port")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(993);
        let username = account
            .config
            .get("username")
            .and_then(serde_json::Value::as_str)
            .filter(|username| !username.trim().is_empty())
            .or_else(|| {
                account
                    .config
                    .get("email")
                    .and_then(serde_json::Value::as_str)
            })
            .ok_or_else(|| "account has no IMAP login name".to_string())?;
        if secret.password.is_empty() {
            return Err("account has no saved password".into());
        }
        let user = Self::imap_login_name(username.trim())?;
        let password = Self::imap_login_name(secret.password.trim())?;
        let smtp_host = account
            .config
            .get("smtp_host")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .ok_or_else(|| "account has no SMTP server".to_string())?;
        let smtp_port = account
            .config
            .get("smtp_port")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(465);

        // An account the provider signed in on its own page is proved the
        // way it will read and send: its secret is a bearer token, carried
        // by XOAUTH2 to both servers.
        // A setting that does not read is a password account, never one
        // signed in with a token.
        let oauth = accounts::email::is_oauth(&account.config);
        let tls = tilo_net::client_config_with_roots(roots)
            .map(Arc::new)
            .map_err(|_| "could not initialize secure protocol versions".to_string())?;
        let security = accounts::email::imap_security_of(&account.config);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "could not start the IMAP probe".to_string())?;
        runtime.block_on(async {
            let client = imap::open(
                reach,
                host,
                port,
                security,
                Arc::clone(&tls),
                Self::NETWORK_TIMEOUT,
            )
            .await?;
            let mut session = imap_signed_in(client, user, password, oauth).await?;
            let _ = timeout(Self::NETWORK_TIMEOUT, session.logout()).await;
            // An account that reads mail and cannot send any is found out
            // here, not at its person's first message.
            smtp::sign_in(
                reach,
                smtp_host,
                smtp_port,
                user,
                password,
                oauth,
                tls,
                Self::NETWORK_TIMEOUT,
            )
            .await?;
            Ok("IMAP and SMTP sign-in succeeded".into())
        })
    }

    /// A DAV account's sign-in at its server, reached `within` a reach where
    /// one is given: a server's, judged at each connection. A device's own
    /// tester gives none and reaches any address.
    fn dav(
        within: Option<&imap::Reach>,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        let url = account
            .config
            .get(if account.type_id.as_str() == "webdav" {
                "base_url"
            } else {
                "server_url"
            })
            .and_then(|value| value.as_str())
            .ok_or_else(|| "account has no server URL".to_string())?;
        let parsed = tilo_dav::Url::parse(url).map_err(|_| "account has an invalid server URL")?;
        if !accounts::is_secure_endpoint(parsed.as_str()) {
            return Err("refusing to send a credential without HTTPS".into());
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("server URL must not contain credentials".into());
        }
        let username = account
            .config
            .get("username")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "account has no username".to_string())?;
        let client = DavClient::new(&tilo_dav::Options {
            user_agent: None,
            timeout: Self::NETWORK_TIMEOUT,
            connect_timeout: Some(Self::NETWORK_TIMEOUT),
            // Never replay Basic credentials to a redirect target. Discovery
            // belongs in setup; a stored endpoint must answer at its exact
            // origin.
            redirects: tilo_dav::Redirects::Refuse,
            lookup: within.map(imap::Reach::lookup),
        })
        .map_err(|_| {
            tilo_net::lacks_authorities()
                .unwrap_or_else(|| "could not initialize the secure HTTP client".to_owned())
        })?
        .with_credentials(Credentials::new(username, secret.password.clone()));
        let response = client
            .propfind(parsed, Depth::Zero, &[props::RESOURCETYPE])
            .map_err(|error| format!("connection failed: {error}"))?;
        if response.status().is_success() {
            Ok("Server accepted the signed-in DAV request".into())
        } else if response.status() == StatusCode::UNAUTHORIZED {
            Err("server rejected the saved credential".into())
        } else {
            Err(format!(
                "server returned HTTP {}",
                response.status().as_u16()
            ))
        }
    }
}

impl AccountTester for LiveAccountTester {
    fn discover_calendars(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
        Self::calendars(None, account, secret)
    }

    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        Self::tested(None, account, secret)
    }
}

impl LiveAccountTester {
    /// The calendars of a `CalDAV` account, asked of its server `within` a
    /// reach where one is given: a server's, judged at each connection. A
    /// device's own tester gives none and reaches any address.
    fn calendars(
        within: Option<&imap::Reach>,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
        let url = account
            .config
            .get("server_url")
            .and_then(serde_json::Value::as_str)
            .ok_or("account has no server URL")?;
        let url = tilo_dav::Url::parse(url).map_err(|_| "invalid server URL")?;
        if !accounts::is_secure_endpoint(url.as_str()) {
            return Err("refusing to send a credential without HTTPS".into());
        }
        let username = account
            .config
            .get("username")
            .and_then(serde_json::Value::as_str)
            .ok_or("account has no username")?;
        let client = DavClient::new(&tilo_dav::Options {
            timeout: Duration::from_secs(2),
            connect_timeout: Some(Duration::from_secs(2)),
            redirects: tilo_dav::Redirects::Refuse,
            lookup: within.map(imap::Reach::lookup),
            ..tilo_dav::Options::default()
        })
        .map_err(|_| {
            tilo_net::lacks_authorities()
                .unwrap_or_else(|| "could not initialize the secure HTTP client".to_owned())
        })?
        .with_credentials(Credentials::new(username, secret.password.clone()));
        tilo_dav::discovery::calendars(&client, &url)
    }

    /// Whether `account` signs in with `secret`, over connections `within` a
    /// reach where one is given.
    fn tested(
        within: Option<&imap::Reach>,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        match account.type_id.as_str() {
            "email" => Self::email(
                &within.cloned().unwrap_or_else(imap::Reach::any),
                account,
                secret,
            ),
            "caldav" => {
                let configured = account
                    .config
                    .get("calendars")
                    .and_then(serde_json::Value::as_array)
                    .filter(|calendars| !calendars.is_empty())
                    .ok_or("Choose calendars in Edit account before checking this connection")?;
                let discovered = Self::calendars(within, account, secret)?;
                if !configured.iter().all(|calendar| {
                    calendar
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| discovered.iter().any(|found| found.href == id))
                }) {
                    return Err("A selected calendar is no longer accessible. Edit the account to choose calendars again.".into());
                }
                Ok(format!("Found {} selected calendars", configured.len()))
            }
            "webdav" | "nextcloud" => Self::dav(within, account, secret),
            _ => Err("unsupported account type".into()),
        }
    }
}

/// A server's proof of a mail account's sign-in, trusting `extra` beside the system's own
/// authorities, with every connection going only where `reach` lets it: a test's mail
/// server, whose certificate no authority signed ([`stand_in`]). It proves mail accounts
/// alone.
#[cfg(any(test, feature = "test-support"))]
pub struct TrustingMailTester {
    pub reach: imap::Reach,
    pub extra: Vec<rustls::pki_types::CertificateDer<'static>>,
}

#[cfg(any(test, feature = "test-support"))]
impl AccountTester for TrustingMailTester {
    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        let mut roots = tilo_net::system_roots();
        for certificate in &self.extra {
            roots
                .add(certificate.clone())
                .map_err(|error| format!("a trusted certificate is not readable: {error}"))?;
        }
        LiveAccountTester::proved(&self.reach, account, secret, roots)
    }
}

/// What proves accounts on a server: [`LiveAccountTester`], with every
/// connection it makes for an account, for mail, a calendar or files, going
/// only where its reach lets it ([`imap::Reach::public_unless`]), and which
/// also works out an address's mail servers over the network
/// ([`mail_servers::find`]). Only a person's server asks that, so an app that
/// proves sign-ins takes [`LiveAccountTester`] and carries no DNS resolver.
pub struct FindingAccountTester(pub imap::Reach);

impl AccountTester for FindingAccountTester {
    fn discover_calendars(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
        LiveAccountTester::calendars(Some(&self.0), account, secret)
    }

    fn find_mail_servers(
        &self,
        address: &str,
        allowed: &dyn Fn(&str) -> bool,
    ) -> Result<mail_servers::MailServers, String> {
        mail_servers::find(
            address,
            &mail_servers::Live::new(allowed).within(self.0.clone()),
        )
    }

    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        LiveAccountTester::tested(Some(&self.0), account, secret)
    }
}
