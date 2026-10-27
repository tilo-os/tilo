//! Tilo's public `OAuth` client registrations, as data.
//!
//! The committed table `oauth-clients.toml` holds what each issuer's
//! developer console issued: the client id, Google's installed-app client
//! secret, the loopback ports an issuer that matches redirect URIs exactly
//! has registered, and the scopes of each use (cloud storage, mail) of that
//! one client. An environment variable overrides the id (and Google's
//! secret) at runtime, then at build time.

use std::sync::OnceLock;

use serde::Deserialize;
use tilo_oauth::{Provider, Redirect, RedirectPorts, Refresh, Registration};

use crate::providers::{Capability, OAuthCloud, OAuthIssuer, OAuthUse};

/// The committed defaults.
const DEFAULTS: &str = include_str!("../oauth-clients.toml");

/// Where a Copilot subscription's GitHub token is traded for Copilot tokens.
pub const COPILOT_TOKEN_ENDPOINT: &str = "https://api.github.com/copilot_internal/v2/token";

/// The scopes each use of one client asks for. An empty list: this issuer
/// is not used for that.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Scopes {
    pub files: Vec<String>,
    pub mail: Vec<String>,
    pub calendar: Vec<String>,
    pub identity: Vec<String>,
    pub music: Vec<String>,
    pub music_streaming: Vec<String>,
    pub models: Vec<String>,
    pub model_downloads: Vec<String>,
}

impl Scopes {
    pub fn of(&self, capability: Capability) -> &[String] {
        match capability {
            Capability::Files => &self.files,
            Capability::Mail => &self.mail,
            Capability::Calendar => &self.calendar,
            Capability::Identity => &self.identity,
            Capability::Music => &self.music,
            Capability::MusicStreaming => &self.music_streaming,
            Capability::Models => &self.models,
            Capability::ModelDownloads => &self.model_downloads,
        }
    }
}

/// One issuer's entry.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ClientEntry {
    pub client_id: String,
    pub client_secret: String,
    /// Empty: any free loopback port.
    pub redirect_ports: Vec<u16>,
    /// The redirect's host when the issuer registered one other than
    /// `127.0.0.1`; empty for that.
    pub redirect_host: String,
    /// The redirect's path when the issuer registered one other than
    /// `/oauth`; empty for that.
    pub redirect_path: String,
    pub paste_code_fallback: bool,
    pub scopes: Scopes,
}

/// The whole table.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ClientTable {
    pub google: ClientEntry,
    pub microsoft: ClientEntry,
    pub dropbox: ClientEntry,
    pub spotify: ClientEntry,
    pub openai: ClientEntry,
    pub github: ClientEntry,
    pub huggingface: ClientEntry,
}

impl ClientTable {
    /// Parse a table in the format of `oauth-clients.toml`.
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

    pub fn client(&self, issuer: OAuthIssuer) -> &ClientEntry {
        match issuer {
            OAuthIssuer::Google => &self.google,
            OAuthIssuer::Microsoft => &self.microsoft,
            OAuthIssuer::Dropbox => &self.dropbox,
            OAuthIssuer::Spotify => &self.spotify,
            OAuthIssuer::OpenAi => &self.openai,
            OAuthIssuer::GitHub => &self.github,
            OAuthIssuer::HuggingFace => &self.huggingface,
        }
    }

    /// The entry behind a cloud storage provider.
    pub fn entry(&self, cloud: OAuthCloud) -> &ClientEntry {
        self.client(cloud.issuer())
    }

    /// The scopes `oauth_use` asks for; empty when this table does not
    /// configure that use.
    pub fn scopes(&self, oauth_use: OAuthUse) -> &[String] {
        self.client(oauth_use.issuer)
            .scopes
            .of(oauth_use.capability)
    }

    /// The issuer's descriptor with the scopes of `oauth_use`. A GitHub
    /// grant for models is a Copilot subscription's: it is renewed by
    /// trading the GitHub token for a Copilot token.
    pub fn descriptor(&self, oauth_use: OAuthUse) -> Provider {
        let refresh = match (oauth_use.issuer, oauth_use.capability) {
            (OAuthIssuer::GitHub, Capability::Models) => Refresh::CopilotToken {
                endpoint: COPILOT_TOKEN_ENDPOINT.into(),
            },
            _ => Refresh::RefreshToken,
        };
        Provider {
            scopes: self.scopes(oauth_use).to_vec(),
            refresh,
            ..oauth_use.issuer.endpoints()
        }
    }

    /// The registration of `issuer`: environment overrides first, then this
    /// table. `None` when no client id is known (the "not configured"
    /// state).
    pub fn client_registration(&self, issuer: OAuthIssuer) -> Option<Registration> {
        let entry = self.client(issuer);
        let non_empty = |value: &str| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        let client_id = issuer
            .client_id()
            .resolve()
            .or_else(|| non_empty(&entry.client_id))?;
        let client_secret = issuer
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
            redirect: {
                let default = Redirect::default();
                Redirect {
                    host: non_empty(&entry.redirect_host).unwrap_or(default.host),
                    path: non_empty(&entry.redirect_path).unwrap_or(default.path),
                }
            },
            paste_code_fallback: entry.paste_code_fallback,
        })
    }

    /// The registration for a cloud storage provider; see
    /// [`Self::client_registration`].
    pub fn registration(&self, cloud: OAuthCloud) -> Option<Registration> {
        self.client_registration(cloud.issuer())
    }

    /// The descriptor and registration of `oauth_use`, `None` when the
    /// issuer has no client id or the use has no scopes.
    pub fn config(&self, oauth_use: OAuthUse) -> Option<(Provider, Registration)> {
        if self.scopes(oauth_use).is_empty() {
            return None;
        }
        let registration = self.client_registration(oauth_use.issuer)?;
        Some((self.descriptor(oauth_use), registration))
    }

    /// Whether `oauth_use` can sign in with this table (and the
    /// environment).
    pub fn is_configured(&self, oauth_use: OAuthUse) -> bool {
        self.config(oauth_use).is_some()
    }
}

impl OAuthCloud {
    /// The registration this build uses; see [`ClientTable::registration`].
    pub fn registration(self) -> Option<Registration> {
        ClientTable::defaults().registration(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GMAIL: OAuthUse = OAuthUse::mail(OAuthIssuer::Google);
    const OUTLOOK: OAuthUse = OAuthUse::mail(OAuthIssuer::Microsoft);

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
        assert_eq!(table.spotify.client_id, "fc7bc68792734157a2c9857cba652df7");
        assert!(table.spotify.client_secret.is_empty());
        assert_eq!(table.spotify.redirect_ports, [47654]);
        assert!(!table.spotify.paste_code_fallback);
    }

    #[test]
    fn the_committed_table_scopes_each_use_of_one_client() {
        let table = ClientTable::parse(DEFAULTS).expect("oauth-clients.toml");
        for cloud in OAuthCloud::ALL {
            assert!(!table.scopes(cloud.oauth_use()).is_empty(), "{cloud:?}");
        }
        assert_eq!(table.scopes(GMAIL), ["https://mail.google.com/"]);
        assert_eq!(
            table.scopes(OAuthUse::calendar(OAuthIssuer::Google)),
            ["https://www.googleapis.com/auth/calendar"]
        );
        assert_eq!(
            table.scopes(OAuthUse::identity(OAuthIssuer::Microsoft)),
            ["offline_access", "User.Read"]
        );
        assert!(
            table
                .scopes(OAuthUse::calendar(OAuthIssuer::Microsoft))
                .is_empty()
        );
        assert_eq!(
            table.scopes(OUTLOOK),
            [
                "offline_access",
                "https://outlook.office.com/IMAP.AccessAsUser.All",
                "https://outlook.office.com/SMTP.Send",
            ]
        );
        assert_eq!(
            table.scopes(OAuthCloud::GoogleDrive.oauth_use()),
            ["https://www.googleapis.com/auth/drive"]
        );
        // Vertex AI asks for the cloud scope alone, in its own grant: none
        // of the Google uses shares a scope with another.
        let vertex = OAuthUse::new(OAuthIssuer::Google, Capability::Models);
        assert_eq!(
            table.scopes(vertex),
            ["https://www.googleapis.com/auth/cloud-platform"]
        );
        for other in [GMAIL, OAuthCloud::GoogleDrive.oauth_use()] {
            assert!(
                table
                    .scopes(other)
                    .iter()
                    .all(|scope| !table.scopes(vertex).contains(scope))
            );
        }
        // Dropbox is storage only.
        assert!(
            table
                .scopes(OAuthUse::mail(OAuthIssuer::Dropbox))
                .is_empty()
        );
        assert!(!table.is_configured(OAuthUse::mail(OAuthIssuer::Dropbox)));
        let (gmail, gmail_client) = table.config(GMAIL).unwrap();
        let (drive, drive_client) = table.config(OAuthCloud::GoogleDrive.oauth_use()).unwrap();
        assert_eq!(gmail_client, drive_client);
        assert_eq!(gmail.token_endpoint, drive.token_endpoint);
        assert_ne!(gmail.scopes, drive.scopes);
    }

    #[test]
    fn the_model_subscriptions_sign_in_as_their_issuers_require() {
        let table = ClientTable::parse(DEFAULTS).expect("oauth-clients.toml");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_OAUTH_OPENAI_CLIENT_ID")
            .remove("TILO_OAUTH_GITHUB_CLIENT_ID")
            .remove("TILO_OAUTH_HUGGINGFACE_CLIENT_ID");
        let chatgpt = OAuthUse::new(OAuthIssuer::OpenAi, Capability::Models);
        let (provider, client) = table.config(chatgpt).expect("ChatGPT is configured");
        assert_eq!(client.redirect_ports, RedirectPorts::Registered(vec![1455]));
        assert_eq!(client.redirect.host, "localhost");
        assert_eq!(client.redirect.path, "/auth/callback");
        assert!(
            provider
                .scopes
                .iter()
                .any(|scope| scope == "offline_access")
        );
        assert_eq!(provider.refresh, Refresh::RefreshToken);

        let copilot = OAuthUse::new(OAuthIssuer::GitHub, Capability::Models);
        let (provider, client) = table.config(copilot).expect("Copilot is configured");
        assert!(provider.device_authorization_endpoint.is_some());
        assert_eq!(
            provider.refresh,
            Refresh::CopilotToken {
                endpoint: COPILOT_TOKEN_ENDPOINT.into()
            }
        );
        assert_eq!(client.redirect, Redirect::default());
        // A GitHub grant for anything else would be refreshed as usual.
        assert_eq!(
            table
                .descriptor(OAuthUse::identity(OAuthIssuer::GitHub))
                .refresh,
            Refresh::RefreshToken
        );

        // Hugging Face signs in for downloads on any loopback port of its
        // registered `/callback`, with a device code where there is no
        // browser; its registration has no inference scope.
        let downloads = OAuthUse::new(OAuthIssuer::HuggingFace, Capability::ModelDownloads);
        let (provider, client) = table.config(downloads).expect("Hugging Face is configured");
        assert_eq!(provider.scopes, ["openid", "profile", "gated-repos"]);
        assert!(provider.device_authorization_endpoint.is_some());
        assert_eq!(client.redirect_ports, RedirectPorts::Any);
        assert_eq!(client.redirect.path, "/callback");
        assert_eq!(client.secret(), None);
        assert!(!table.is_configured(OAuthUse::new(OAuthIssuer::HuggingFace, Capability::Models)));
    }

    #[test]
    fn an_empty_entry_is_not_configured_and_a_filled_one_is() {
        let table = ClientTable::parse(
            r#"
            [google]
            client_id = "id.apps.googleusercontent.com"
            client_secret = "desktop-secret"
            [google.scopes]
            mail = ["https://mail.google.com/"]
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
            assert!(!table.is_configured(OUTLOOK));
        }
        if option_env!("TILO_OAUTH_GOOGLE_CLIENT_ID").is_none()
            && option_env!("TILO_OAUTH_GOOGLE_CLIENT_SECRET").is_none()
        {
            let google = table.registration(OAuthCloud::GoogleDrive).unwrap();
            assert_eq!(google.client_id, "id.apps.googleusercontent.com");
            assert_eq!(google.secret(), Some("desktop-secret"));
            assert_eq!(google.redirect_ports, RedirectPorts::Any);
            // A client without storage scopes signs in to Gmail only.
            assert!(table.is_configured(GMAIL));
            assert!(!table.is_configured(OAuthCloud::GoogleDrive.oauth_use()));
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
