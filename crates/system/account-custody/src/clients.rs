//! Tilo's public `OAuth` client registrations, as data.
//!
//! The committed table `oauth-clients.toml` holds what each provider's
//! developer console issued: the client id, Google's installed-app client
//! secret, and the loopback ports a provider that matches redirect URIs
//! exactly has registered. An environment variable overrides the id (and
//! Google's secret) at runtime, then at build time.

use std::sync::OnceLock;

use serde::Deserialize;
use tilo_oauth::{ClientId, RedirectPorts, Registration};

use crate::providers::OAuthCloud;

/// The committed defaults.
const DEFAULTS: &str = include_str!("../oauth-clients.toml");

/// One provider's entry.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ClientEntry {
    pub client_id: String,
    pub client_secret: String,
    /// Empty: any free loopback port.
    pub redirect_ports: Vec<u16>,
    pub paste_code_fallback: bool,
}

/// The whole table.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ClientTable {
    pub google: ClientEntry,
    pub microsoft: ClientEntry,
    pub dropbox: ClientEntry,
}

impl ClientTable {
    /// Parse a table in the format of `oauth-clients.toml`.
    ///
    /// # Errors
    /// The TOML error, when the text is not a valid table.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// The committed table.
    pub fn defaults() -> &'static Self {
        static TABLE: OnceLock<ClientTable> = OnceLock::new();
        TABLE.get_or_init(|| {
            Self::parse(DEFAULTS).unwrap_or_else(|error| {
                tracing::error!(%error, "oauth-clients.toml is invalid; no provider is configured");
                Self::default()
            })
        })
    }

    pub fn entry(&self, cloud: OAuthCloud) -> &ClientEntry {
        match cloud {
            OAuthCloud::GoogleDrive => &self.google,
            OAuthCloud::OneDrive => &self.microsoft,
            OAuthCloud::Dropbox => &self.dropbox,
        }
    }

    /// The registration for `cloud`: environment overrides first, then this
    /// table. `None` when no client id is known (the "not configured"
    /// state).
    pub fn registration(&self, cloud: OAuthCloud) -> Option<Registration> {
        let entry = self.entry(cloud);
        let non_empty = |value: &str| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        let client_id = cloud
            .client_id()
            .resolve()
            .or_else(|| non_empty(&entry.client_id))?;
        let client_secret = cloud
            .client_secret()
            .and_then(|key| key.resolve())
            .or_else(|| non_empty(&entry.client_secret));
        Some(Registration {
            client_id,
            client_secret,
            redirect_ports: if entry.redirect_ports.is_empty() {
                RedirectPorts::Any
            } else {
                RedirectPorts::Registered(entry.redirect_ports.clone())
            },
            paste_code_fallback: entry.paste_code_fallback,
        })
    }
}

impl OAuthCloud {
    /// The environment variable that overrides the client id, read at
    /// runtime and then at build time.
    pub fn client_id(self) -> ClientId {
        match self {
            Self::GoogleDrive => ClientId::new(
                "TILO_OAUTH_GOOGLE_CLIENT_ID",
                option_env!("TILO_OAUTH_GOOGLE_CLIENT_ID"),
            ),
            Self::OneDrive => ClientId::new(
                "TILO_OAUTH_MICROSOFT_CLIENT_ID",
                option_env!("TILO_OAUTH_MICROSOFT_CLIENT_ID"),
            ),
            Self::Dropbox => ClientId::new(
                "TILO_OAUTH_DROPBOX_CLIENT_ID",
                option_env!("TILO_OAUTH_DROPBOX_CLIENT_ID"),
            ),
        }
    }

    /// The environment variable that overrides the client secret, for the
    /// providers whose installed-app clients have one (Google).
    pub fn client_secret(self) -> Option<ClientId> {
        match self {
            Self::GoogleDrive => Some(ClientId::new(
                "TILO_OAUTH_GOOGLE_CLIENT_SECRET",
                option_env!("TILO_OAUTH_GOOGLE_CLIENT_SECRET"),
            )),
            Self::OneDrive | Self::Dropbox => None,
        }
    }

    /// The registration this build uses; see [`ClientTable::registration`].
    pub fn registration(self) -> Option<Registration> {
        ClientTable::defaults().registration(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_table_parses_and_names_every_provider() {
        let table = ClientTable::parse(DEFAULTS).expect("oauth-clients.toml");
        assert!(
            table
                .google
                .client_id
                .ends_with(".apps.googleusercontent.com")
        );
        assert!(!table.microsoft.client_id.is_empty());
        assert!(!table.dropbox.client_id.is_empty());
        // Google and Microsoft match loopback redirects without the port;
        // Dropbox matches exactly and so needs registered ports.
        assert!(table.google.redirect_ports.is_empty());
        assert!(table.microsoft.redirect_ports.is_empty());
        assert!(!table.dropbox.redirect_ports.is_empty());
        assert!(table.dropbox.paste_code_fallback);
    }

    #[test]
    fn an_empty_entry_is_not_configured_and_a_filled_one_is() {
        let table = ClientTable::parse(
            r#"
            [google]
            client_id = "id.apps.googleusercontent.com"
            client_secret = "desktop-secret"
            [dropbox]
            client_id = "key"
            redirect_ports = [1, 2]
            paste_code_fallback = true
            "#,
        )
        .unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_OAUTH_GOOGLE_CLIENT_ID")
            .remove("TILO_OAUTH_GOOGLE_CLIENT_SECRET")
            .remove("TILO_OAUTH_MICROSOFT_CLIENT_ID")
            .remove("TILO_OAUTH_DROPBOX_CLIENT_ID");
        if option_env!("TILO_OAUTH_MICROSOFT_CLIENT_ID").is_none() {
            assert_eq!(table.registration(OAuthCloud::OneDrive), None);
        }
        if option_env!("TILO_OAUTH_GOOGLE_CLIENT_ID").is_none()
            && option_env!("TILO_OAUTH_GOOGLE_CLIENT_SECRET").is_none()
        {
            let google = table.registration(OAuthCloud::GoogleDrive).unwrap();
            assert_eq!(google.client_id, "id.apps.googleusercontent.com");
            assert_eq!(google.secret(), Some("desktop-secret"));
            assert_eq!(google.redirect_ports, RedirectPorts::Any);
        }
        if option_env!("TILO_OAUTH_DROPBOX_CLIENT_ID").is_none() {
            let dropbox = table.registration(OAuthCloud::Dropbox).unwrap();
            assert_eq!(
                dropbox.redirect_ports,
                RedirectPorts::Registered(vec![1, 2])
            );
            assert!(dropbox.paste_code_fallback);
            assert_eq!(dropbox.secret(), None);
        }
    }

    #[test]
    fn the_environment_overrides_the_table() {
        let table = ClientTable::parse("[google]\nclient_id = \"from-table\"\n").unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_OAUTH_GOOGLE_CLIENT_ID", "from-env")
            .set("TILO_OAUTH_GOOGLE_CLIENT_SECRET", "secret-from-env");
        let google = table.registration(OAuthCloud::GoogleDrive).unwrap();
        assert_eq!(google.client_id, "from-env");
        assert_eq!(google.secret(), Some("secret-from-env"));
    }
}
