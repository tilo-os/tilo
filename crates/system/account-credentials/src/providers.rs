//! The `OAuth` providers and what their grants are used for, as data.
//!
//! An [`OAuthIssuer`] is a provider that registered Tilo as an installed app
//! and signs the person in (Google, Microsoft, Dropbox): its endpoints, and
//! one client registration in `oauth-clients.toml`. An [`OAuthUse`] is one
//! thing a grant from that issuer is for ([`Capability::Files`] for cloud
//! storage, [`Capability::Mail`] for IMAP and SMTP). Scopes belong to the
//! use, so Google Drive and Gmail share Google's client with different
//! scopes.

use serde::Deserialize;
use tilo_oauth::{ClientId, Provider, Refresh, Revocation};

pub const GOOGLE_DRIVE_TYPE: &str = accounts::cloud_types::GOOGLE_DRIVE;
pub const ONEDRIVE_TYPE: &str = accounts::cloud_types::ONEDRIVE;
pub const DROPBOX_TYPE: &str = accounts::cloud_types::DROPBOX;
pub const ICLOUD_DRIVE_TYPE: &str = accounts::cloud_types::ICLOUD_DRIVE;
pub const NEXTCLOUD_TYPE: &str = accounts::cloud_types::NEXTCLOUD;
pub const WEBDAV_TYPE: &str = accounts::cloud_types::WEBDAV;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OAuthIssuer {
    Google,
    Microsoft,
    Dropbox,
    Spotify,
    OpenAi,
    GitHub,
    HuggingFace,
}

pub(crate) fn valid_mailbox(email: &str) -> bool {
    email.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty() && !domain.is_empty() && !domain.contains('@')
    }) && !email.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl OAuthIssuer {
    /// Canonical settings consumed by Mail, including for hosted domains.
    /// Supply a provider-confirmed mailbox address, not an unverified hint.
    /// This constructs metadata only: validate the mail grant before saving it.
    /// Unsupported issuers and malformed addresses return `None`.
    pub fn mail_config(self, email: &str) -> Option<accounts::email::AccountConfig> {
        if !valid_mailbox(email) {
            return None;
        }
        let (imap, smtp, port) = match self {
            Self::Google => ("imap.gmail.com", "smtp.gmail.com", 465),
            Self::Microsoft => ("outlook.office365.com", "smtp.office365.com", 587),
            _ => return None,
        };
        Some(accounts::email::AccountConfig {
            email: email.into(),
            username: email.into(),
            imap_host: imap.into(),
            smtp_host: smtp.into(),
            smtp_port: port,
            auth: accounts::email::AuthMethod::OAuth,
            ..Default::default()
        })
    }

    pub const ALL: [Self; 7] = [
        Self::Google,
        Self::Microsoft,
        Self::Dropbox,
        Self::Spotify,
        Self::OpenAi,
        Self::GitHub,
        Self::HuggingFace,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Microsoft => "microsoft",
            Self::Dropbox => "dropbox",
            Self::Spotify => "spotify",
            Self::OpenAi => "openai",
            Self::GitHub => "github",
            Self::HuggingFace => "huggingface",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::Microsoft => "Microsoft",
            Self::Dropbox => "Dropbox",
            Self::Spotify => "Spotify",
            Self::OpenAi => "OpenAI",
            Self::GitHub => "GitHub",
            Self::HuggingFace => "Hugging Face",
        }
    }

    pub fn client_id(self) -> ClientId {
        match self {
            Self::Google => ClientId::new(
                "TILO_OAUTH_GOOGLE_CLIENT_ID",
                option_env!("TILO_OAUTH_GOOGLE_CLIENT_ID"),
            ),
            Self::Microsoft => ClientId::new(
                "TILO_OAUTH_MICROSOFT_CLIENT_ID",
                option_env!("TILO_OAUTH_MICROSOFT_CLIENT_ID"),
            ),
            Self::Spotify => ClientId::new(
                "TILO_OAUTH_SPOTIFY_CLIENT_ID",
                option_env!("TILO_OAUTH_SPOTIFY_CLIENT_ID"),
            ),
            Self::Dropbox => ClientId::new(
                "TILO_OAUTH_DROPBOX_CLIENT_ID",
                option_env!("TILO_OAUTH_DROPBOX_CLIENT_ID"),
            ),
            Self::OpenAi => ClientId::new(
                "TILO_OAUTH_OPENAI_CLIENT_ID",
                option_env!("TILO_OAUTH_OPENAI_CLIENT_ID"),
            ),
            Self::GitHub => ClientId::new(
                "TILO_OAUTH_GITHUB_CLIENT_ID",
                option_env!("TILO_OAUTH_GITHUB_CLIENT_ID"),
            ),
            Self::HuggingFace => ClientId::new(
                "TILO_OAUTH_HUGGINGFACE_CLIENT_ID",
                option_env!("TILO_OAUTH_HUGGINGFACE_CLIENT_ID"),
            ),
        }
    }

    /// The environment variable that overrides the client secret, for the
    /// issuers whose installed-app clients have one (Google).
    pub fn client_secret(self) -> Option<ClientId> {
        match self {
            Self::Google => Some(ClientId::new(
                "TILO_OAUTH_GOOGLE_CLIENT_SECRET",
                option_env!("TILO_OAUTH_GOOGLE_CLIENT_SECRET"),
            )),
            _ => None,
        }
    }

    pub fn endpoints(self) -> Provider {
        let pairs = |values: &[(&str, &str)]| {
            values
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect()
        };
        match self {
            Self::Google => Provider {
                id: self.id().into(),
                label: self.label().into(),
                authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token_endpoint: "https://oauth2.googleapis.com/token".into(),
                revocation: Revocation::Rfc7009 {
                    endpoint: "https://oauth2.googleapis.com/revoke".into(),
                },
                scopes: Vec::new(),
                authorization_params: pairs(&[("access_type", "offline"), ("prompt", "consent")]),
                refresh: Refresh::RefreshToken,
                device_authorization_endpoint: None,
            },
            Self::Microsoft => Provider {
                id: self.id().into(),
                label: self.label().into(),
                authorization_endpoint:
                    "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
                token_endpoint: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
                revocation: Revocation::Unsupported,
                scopes: Vec::new(),
                authorization_params: pairs(&[("prompt", "select_account")]),
                refresh: Refresh::RefreshToken,
                device_authorization_endpoint: None,
            },
            Self::Spotify => Provider {
                id: self.id().into(),
                label: self.label().into(),
                authorization_endpoint: "https://accounts.spotify.com/authorize".into(),
                token_endpoint: "https://accounts.spotify.com/api/token".into(),
                revocation: Revocation::Unsupported,
                scopes: Vec::new(),
                authorization_params: Vec::new(),
                refresh: Refresh::RefreshToken,
                device_authorization_endpoint: None,
            },
            Self::Dropbox => Provider {
                id: self.id().into(),
                label: self.label().into(),
                authorization_endpoint: "https://www.dropbox.com/oauth2/authorize".into(),
                token_endpoint: "https://api.dropboxapi.com/oauth2/token".into(),
                revocation: Revocation::BearerPost {
                    endpoint: "https://api.dropboxapi.com/2/auth/token/revoke".into(),
                },
                scopes: Vec::new(),
                authorization_params: pairs(&[("token_access_type", "offline")]),
                refresh: Refresh::RefreshToken,
                device_authorization_endpoint: None,
            },
            Self::OpenAi => Provider {
                id: self.id().into(),
                label: self.label().into(),
                authorization_endpoint: "https://auth.openai.com/oauth/authorize".into(),
                token_endpoint: "https://auth.openai.com/oauth/token".into(),
                revocation: Revocation::Unsupported,
                scopes: Vec::new(),
                authorization_params: pairs(&[
                    ("id_token_add_organizations", "true"),
                    ("codex_cli_simplified_flow", "true"),
                    ("originator", "codex_cli_rs"),
                ]),
                refresh: Refresh::RefreshToken,
                device_authorization_endpoint: None,
            },
            Self::GitHub => Provider {
                id: self.id().into(),
                label: self.label().into(),
                authorization_endpoint: "https://github.com/login/oauth/authorize".into(),
                token_endpoint: "https://github.com/login/oauth/access_token".into(),
                revocation: Revocation::Unsupported,
                scopes: Vec::new(),
                authorization_params: Vec::new(),
                refresh: Refresh::RefreshToken,
                device_authorization_endpoint: Some("https://github.com/login/device/code".into()),
            },
            Self::HuggingFace => Provider {
                id: self.id().into(),
                label: self.label().into(),
                authorization_endpoint: "https://huggingface.co/oauth/authorize".into(),
                token_endpoint: "https://huggingface.co/oauth/token".into(),
                revocation: Revocation::Unsupported,
                scopes: Vec::new(),
                authorization_params: Vec::new(),
                refresh: Refresh::RefreshToken,
                device_authorization_endpoint: Some("https://huggingface.co/oauth/device".into()),
            },
        }
    }
}

/// A provider's web client, as a server that finishes the sign-in needs it:
/// the registration a browser is sent back to the server with, and the
/// endpoints it is sent to and exchanges its code at. Only a server's
/// environment configures one; a build with none signs nobody in this way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebClient {
    pub client_id: String,
    /// Sent on the code exchange where the provider issued one (Google's
    /// web clients are confidential; Microsoft's may be public).
    pub client_secret: Option<String>,
    /// Where the person's browser signs in.
    pub authorize: String,
    /// Where the code is exchanged, and a grant renewed.
    pub token: String,
    /// Where the address the grant belongs to is read.
    pub profile: String,
}

/// The environment variable a provider's web client id is read from.
#[must_use]
pub const fn web_client_id_key(issuer: OAuthIssuer) -> Option<&'static str> {
    match issuer {
        OAuthIssuer::Google => Some("TILO_OAUTH_GOOGLE_WEB_CLIENT_ID"),
        OAuthIssuer::Microsoft => Some("TILO_OAUTH_MICROSOFT_WEB_CLIENT_ID"),
        _ => None,
    }
}

/// The environment variable a provider's web client secret is read from.
#[must_use]
pub const fn web_client_secret_key(issuer: OAuthIssuer) -> Option<&'static str> {
    match issuer {
        OAuthIssuer::Google => Some("TILO_OAUTH_GOOGLE_WEB_CLIENT_SECRET"),
        OAuthIssuer::Microsoft => Some("TILO_OAUTH_MICROSOFT_WEB_CLIENT_SECRET"),
        _ => None,
    }
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The provider's web client, from the environment: `None` when its client
/// id is unset, the "not configured" state. Endpoints default to the
/// provider's own and may be pointed elsewhere by
/// `TILO_OAUTH_<PROVIDER>_AUTHORIZE_URL` / `_TOKEN_URL` (a local stack's
/// stand-in); `TILO_OAUTH_<PROVIDER>_PROFILE_URL` does the same for the
/// address.
#[must_use]
pub fn web_client(issuer: OAuthIssuer) -> Option<WebClient> {
    let key = web_client_id_key(issuer)?;
    let client_id = env_value(key)?;
    let client_secret = web_client_secret_key(issuer).and_then(env_value);
    let own = issuer.endpoints();
    let prefix = match issuer {
        OAuthIssuer::Google => "TILO_OAUTH_GOOGLE",
        OAuthIssuer::Microsoft => "TILO_OAUTH_MICROSOFT",
        _ => return None,
    };
    let at =
        |name: &str, default: String| env_value(&format!("{prefix}_{name}")).unwrap_or(default);
    Some(WebClient {
        client_id,
        client_secret,
        authorize: at("AUTHORIZE_URL", own.authorization_endpoint),
        token: at("TOKEN_URL", own.token_endpoint),
        profile: at(
            "PROFILE_URL",
            match issuer {
                OAuthIssuer::Google => {
                    "https://gmail.googleapis.com/gmail/v1/users/me/profile".to_owned()
                }
                _ => "https://graph.microsoft.com/v1.0/me?$select=mail".to_owned(),
            },
        ),
    })
}

/// The scopes a server asks for: the chosen uses' scopes, plus what is
/// needed to learn which address the grant belongs to.
#[must_use]
pub fn web_scopes(issuer: OAuthIssuer, mail: bool, calendar: bool) -> Vec<String> {
    let table = crate::ClientTable::defaults();
    let mut scopes = Vec::new();
    if mail {
        scopes.extend(table.scopes(OAuthUse::mail(issuer)).iter().cloned());
    }
    if calendar {
        scopes.extend(table.scopes(OAuthUse::calendar(issuer)).iter().cloned());
    }
    for scope in match issuer {
        OAuthIssuer::Google => ["openid", "email"].as_slice(),
        OAuthIssuer::Microsoft => ["openid", "email", "User.Read"].as_slice(),
        _ => &[],
    } {
        if !scopes.iter().any(|kept| kept == scope) {
            scopes.push((*scope).to_owned());
        }
    }
    scopes
}

/// The provider's descriptor as a server's sign-in asks it: the web client's
/// endpoints, the provider's authorization parameters, and the scopes of the
/// uses the person chose.
#[must_use]
pub fn web_provider(issuer: OAuthIssuer, scopes: Vec<String>, client: &WebClient) -> Provider {
    let mut descriptor = issuer.endpoints();
    client
        .authorize
        .clone_into(&mut descriptor.authorization_endpoint);
    client.token.clone_into(&mut descriptor.token_endpoint);
    descriptor.scopes = scopes;
    descriptor
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Files,
    Mail,
    Calendar,
    Identity,
    Music,
    MusicStreaming,
    /// Hosted agent models the model gateway calls (Google Vertex AI and the
    /// Gemini API, a `ChatGPT` or Copilot subscription, Hugging Face). A grant
    /// of its own, so the broad scope it may need never rides on a mail or
    /// storage token, and forgetting it leaves those untouched.
    Models,
    ModelDownloads,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OAuthUse {
    pub issuer: OAuthIssuer,
    pub capability: Capability,
}

impl OAuthUse {
    pub const fn new(issuer: OAuthIssuer, capability: Capability) -> Self {
        Self { issuer, capability }
    }

    pub const fn mail(issuer: OAuthIssuer) -> Self {
        Self::new(issuer, Capability::Mail)
    }

    pub const fn calendar(issuer: OAuthIssuer) -> Self {
        Self::new(issuer, Capability::Calendar)
    }

    pub const fn identity(issuer: OAuthIssuer) -> Self {
        Self::new(issuer, Capability::Identity)
    }

    pub fn label(self) -> &'static str {
        match (self.issuer, self.capability) {
            (OAuthIssuer::Spotify, _) => "Spotify",
            (OAuthIssuer::Google, Capability::Models) => "Google Vertex AI",
            (OAuthIssuer::OpenAi, Capability::Models) => "ChatGPT",
            (OAuthIssuer::GitHub, Capability::Models) => "GitHub Copilot",
            (OAuthIssuer::HuggingFace, Capability::Models) => "Hugging Face",
            (_, Capability::Models) => "Agent models",
            (_, Capability::Music | Capability::MusicStreaming) => "Music",
            (OAuthIssuer::Google, Capability::Calendar) => "Google Calendar",
            (_, Capability::Calendar) => "Calendar",
            (_, Capability::Identity | Capability::ModelDownloads)
            | (
                OAuthIssuer::OpenAi | OAuthIssuer::GitHub | OAuthIssuer::HuggingFace,
                Capability::Files | Capability::Mail,
            ) => self.issuer.label(),
            (OAuthIssuer::Google, Capability::Files) => "Google Drive",
            (OAuthIssuer::Google, Capability::Mail) => "Gmail",
            (OAuthIssuer::Microsoft, Capability::Files) => "OneDrive",
            (OAuthIssuer::Microsoft, Capability::Mail) => "Outlook",
            (OAuthIssuer::Dropbox, Capability::Files) => "Dropbox",
            (OAuthIssuer::Dropbox, Capability::Mail) => "Dropbox mail",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OAuthCloud {
    GoogleDrive,
    OneDrive,
    Dropbox,
}

impl OAuthCloud {
    pub const ALL: [Self; 3] = [Self::GoogleDrive, Self::OneDrive, Self::Dropbox];

    pub fn type_id(self) -> &'static str {
        match self {
            Self::GoogleDrive => GOOGLE_DRIVE_TYPE,
            Self::OneDrive => ONEDRIVE_TYPE,
            Self::Dropbox => DROPBOX_TYPE,
        }
    }

    pub fn from_type_id(type_id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.type_id() == type_id)
    }

    pub fn issuer(self) -> OAuthIssuer {
        match self {
            Self::GoogleDrive => OAuthIssuer::Google,
            Self::OneDrive => OAuthIssuer::Microsoft,
            Self::Dropbox => OAuthIssuer::Dropbox,
        }
    }

    pub fn oauth_use(self) -> OAuthUse {
        OAuthUse::new(self.issuer(), Capability::Files)
    }

    pub fn label(self) -> &'static str {
        self.oauth_use().label()
    }

    pub fn sign_in_with(self) -> &'static str {
        self.issuer().label()
    }

    pub fn client_id(self) -> ClientId {
        self.issuer().client_id()
    }

    /// The environment variable that overrides the client secret; see
    /// [`OAuthIssuer::client_secret`].
    pub fn client_secret(self) -> Option<ClientId> {
        self.issuer().client_secret()
    }

    pub fn descriptor(self) -> Provider {
        crate::ClientTable::defaults().descriptor(self.oauth_use())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(provider: OAuthCloud) -> String {
        let descriptor = provider.descriptor();
        tilo_oauth::AuthorizationRequest {
            provider: &descriptor,
            client_id: "cid",
            redirect_uri: "http://127.0.0.1:1/oauth",
            code_challenge: "c",
            state: "s",
            login_hint: None,
        }
        .url()
    }

    tilo_test_env::scenario! {
        /// The provider table: type ids, refresh tokens, mail settings and uses.
        fn providers_keep_their_types_scopes_and_mail_settings_apart;

        phase every_provider_asks_for_a_refresh_token_its_own_way;
        {
            let google = url(OAuthCloud::GoogleDrive);
            assert!(google.contains("access_type=offline") && google.contains("prompt=consent"));
            assert!(google.contains("auth%2Fdrive"));
            let microsoft = url(OAuthCloud::OneDrive);
            assert!(microsoft.starts_with("https://login.microsoftonline.com/common/"));
            assert!(microsoft.contains("offline_access"));
            let dropbox = url(OAuthCloud::Dropbox);
            assert!(dropbox.contains("token_access_type=offline"));
        }

        phase type_ids_round_trip_and_keys_are_distinct;
        {
            for provider in OAuthCloud::ALL {
                assert_eq!(OAuthCloud::from_type_id(provider.type_id()), Some(provider));
            }
            let keys: std::collections::BTreeSet<_> = OAuthIssuer::ALL
                .into_iter()
                .map(|issuer| issuer.client_id().key)
                .collect();
            assert_eq!(keys.len(), OAuthIssuer::ALL.len());
            assert_eq!(OAuthCloud::from_type_id(WEBDAV_TYPE), None);
        }

        phase canonical_mail_settings_support_hosted_domains_without_guessing;
        {
            let config = OAuthIssuer::Microsoft
                .mail_config("ada@hosted.example")
                .unwrap();
            assert_eq!(config.imap_host, "outlook.office365.com");
            assert_eq!(config.imap_port, 993);
            assert_eq!(config.smtp_host, "smtp.office365.com");
            assert_eq!(config.smtp_port, 587);
            assert_eq!(config.username, config.email);
            assert_eq!(config.auth, accounts::email::AuthMethod::OAuth);
            assert!(config.imap_tls && config.smtp_tls);
            assert!(config.validate().is_empty());
            for email in [
                "@example.org",
                "ada@",
                "ada@@example.org",
                "ada@example.org\0",
            ] {
                assert!(OAuthIssuer::Microsoft.mail_config(email).is_none());
            }
            assert!(
                OAuthIssuer::Dropbox
                    .mail_config("ada@example.org")
                    .is_none()
            );
        }

        phase mail_and_files_share_an_issuer_but_not_a_use;
        {
            assert_eq!(OAuthCloud::GoogleDrive.issuer(), OAuthIssuer::Google);
            assert_ne!(
                OAuthCloud::GoogleDrive.oauth_use(),
                OAuthUse::mail(OAuthIssuer::Google)
            );
            assert_eq!(OAuthUse::mail(OAuthIssuer::Google).label(), "Gmail");
            assert_eq!(OAuthUse::mail(OAuthIssuer::Microsoft).label(), "Outlook");
            assert_eq!(
                OAuthUse::new(OAuthIssuer::Google, Capability::Models).label(),
                "Google Vertex AI"
            );
        }
    }
}
