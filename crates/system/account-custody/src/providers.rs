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
use tilo_oauth::{ClientId, Provider, Revocation};

/// Account type id of a Google Drive account.
pub const GOOGLE_DRIVE_TYPE: &str = accounts::cloud_types::GOOGLE_DRIVE;
/// Account type id of a `OneDrive` account (personal or work/school).
pub const ONEDRIVE_TYPE: &str = accounts::cloud_types::ONEDRIVE;
/// Account type id of a Dropbox account.
pub const DROPBOX_TYPE: &str = accounts::cloud_types::DROPBOX;
/// Account type id of an `iCloud` Drive account.
pub const ICLOUD_DRIVE_TYPE: &str = accounts::cloud_types::ICLOUD_DRIVE;
/// Account type id of a Nextcloud account.
pub const NEXTCLOUD_TYPE: &str = accounts::cloud_types::NEXTCLOUD;
/// Account type id of a plain `WebDAV` account.
pub const WEBDAV_TYPE: &str = accounts::cloud_types::WEBDAV;

/// A provider that registered Tilo as an installed `OAuth` client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OAuthIssuer {
    Google,
    Microsoft,
    Dropbox,
    Spotify,
}

impl OAuthIssuer {
    pub const ALL: [Self; 4] = [Self::Google, Self::Microsoft, Self::Dropbox, Self::Spotify];

    /// Stable identifier, also the issuer's table in `oauth-clients.toml`.
    pub fn id(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Microsoft => "microsoft",
            Self::Dropbox => "dropbox",
            Self::Spotify => "spotify",
        }
    }

    /// Who the person signs in with.
    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::Microsoft => "Microsoft",
            Self::Dropbox => "Dropbox",
            Self::Spotify => "Spotify",
        }
    }

    /// The environment variable that overrides the client id, read at
    /// runtime and then at build time.
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
            Self::Microsoft | Self::Dropbox | Self::Spotify => None,
        }
    }

    /// The issuer's endpoints and the authorization parameters that make it
    /// issue a refresh token, without scopes: those come from the use (see
    /// [`crate::ClientTable::descriptor`]).
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
                // Without both, Google issues no refresh token on a repeat
                // consent.
                authorization_params: pairs(&[("access_type", "offline"), ("prompt", "consent")]),
            },
            // The `common` tenant accepts both personal Microsoft accounts
            // and work or school accounts; `offline_access` (in each use's
            // scopes) yields the refresh token.
            Self::Microsoft => Provider {
                id: self.id().into(),
                label: self.label().into(),
                authorization_endpoint:
                    "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
                token_endpoint: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
                revocation: Revocation::Unsupported,
                scopes: Vec::new(),
                authorization_params: pairs(&[("prompt", "select_account")]),
            },
            Self::Spotify => Provider {
                id: self.id().into(),
                label: self.label().into(),
                authorization_endpoint: "https://accounts.spotify.com/authorize".into(),
                token_endpoint: "https://accounts.spotify.com/api/token".into(),
                revocation: Revocation::Unsupported,
                scopes: Vec::new(),
                authorization_params: Vec::new(),
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
            },
        }
    }
}

/// What a grant is used for. Each has its own scopes per issuer in
/// `oauth-clients.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Cloud storage the storage daemon mounts.
    Files,
    /// Mail over IMAP and SMTP (`XOAUTH2`).
    Mail,
    Music,
    MusicStreaming,
    /// Hosted agent models the model gateway calls (Google Vertex AI). A
    /// grant of its own, so the broad cloud scope it needs never rides on a
    /// mail or storage token, and forgetting it leaves those untouched.
    Models,
}

/// One use of an issuer's client: who signs the person in, and what for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OAuthUse {
    pub issuer: OAuthIssuer,
    pub capability: Capability,
}

impl OAuthUse {
    pub const fn new(issuer: OAuthIssuer, capability: Capability) -> Self {
        Self { issuer, capability }
    }

    /// Mail from `issuer` (Gmail, Outlook).
    pub const fn mail(issuer: OAuthIssuer) -> Self {
        Self::new(issuer, Capability::Mail)
    }

    /// The product name shown to the person.
    pub fn label(self) -> &'static str {
        match (self.issuer, self.capability) {
            (OAuthIssuer::Spotify, _) => "Spotify",
            (OAuthIssuer::Google, Capability::Models) => "Google Vertex AI",
            (_, Capability::Models) => "Agent models",
            (_, Capability::Music | Capability::MusicStreaming) => "Music",
            (OAuthIssuer::Google, Capability::Files) => "Google Drive",
            (OAuthIssuer::Google, Capability::Mail) => "Gmail",
            (OAuthIssuer::Microsoft, Capability::Files) => "OneDrive",
            (OAuthIssuer::Microsoft, Capability::Mail) => "Outlook",
            (OAuthIssuer::Dropbox, Capability::Files) => "Dropbox",
            (OAuthIssuer::Dropbox, Capability::Mail) => "Dropbox mail",
        }
    }
}

/// A cloud storage provider that signs in with `OAuth`.
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

    /// Whose client this provider signs in through.
    pub fn issuer(self) -> OAuthIssuer {
        match self {
            Self::GoogleDrive => OAuthIssuer::Google,
            Self::OneDrive => OAuthIssuer::Microsoft,
            Self::Dropbox => OAuthIssuer::Dropbox,
        }
    }

    /// The use a grant for this provider is for.
    pub fn oauth_use(self) -> OAuthUse {
        OAuthUse::new(self.issuer(), Capability::Files)
    }

    pub fn label(self) -> &'static str {
        self.oauth_use().label()
    }

    /// Who the person signs in with.
    pub fn sign_in_with(self) -> &'static str {
        self.issuer().label()
    }

    /// The environment variable that overrides the client id; see
    /// [`OAuthIssuer::client_id`].
    pub fn client_id(self) -> ClientId {
        self.issuer().client_id()
    }

    /// The environment variable that overrides the client secret; see
    /// [`OAuthIssuer::client_secret`].
    pub fn client_secret(self) -> Option<ClientId> {
        self.issuer().client_secret()
    }

    /// The provider descriptor with the committed storage scopes.
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

    #[test]
    fn every_provider_asks_for_a_refresh_token_its_own_way() {
        let google = url(OAuthCloud::GoogleDrive);
        assert!(google.contains("access_type=offline") && google.contains("prompt=consent"));
        assert!(google.contains("auth%2Fdrive"));
        let microsoft = url(OAuthCloud::OneDrive);
        assert!(microsoft.starts_with("https://login.microsoftonline.com/common/"));
        assert!(microsoft.contains("offline_access"));
        let dropbox = url(OAuthCloud::Dropbox);
        assert!(dropbox.contains("token_access_type=offline"));
    }

    #[test]
    fn type_ids_round_trip_and_keys_are_distinct() {
        for provider in OAuthCloud::ALL {
            assert_eq!(OAuthCloud::from_type_id(provider.type_id()), Some(provider));
        }
        let keys: std::collections::BTreeSet<_> = OAuthIssuer::ALL
            .into_iter()
            .map(|issuer| issuer.client_id().key)
            .collect();
        assert_eq!(keys.len(), 4);
        assert_eq!(OAuthCloud::from_type_id(WEBDAV_TYPE), None);
    }

    #[test]
    fn mail_and_files_share_an_issuer_but_not_a_use() {
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
