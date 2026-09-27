//! The `OAuth` cloud storage providers, as data.

use tilo_oauth::{Provider, Revocation};

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

    pub fn label(self) -> &'static str {
        match self {
            Self::GoogleDrive => "Google Drive",
            Self::OneDrive => "OneDrive",
            Self::Dropbox => "Dropbox",
        }
    }

    /// Who the person signs in with.
    pub fn sign_in_with(self) -> &'static str {
        match self {
            Self::GoogleDrive => "Google",
            Self::OneDrive => "Microsoft",
            Self::Dropbox => "Dropbox",
        }
    }

    /// The provider descriptor.
    pub fn descriptor(self) -> Provider {
        let owned = |values: &[&str]| values.iter().map(|value| (*value).to_string()).collect();
        let pairs = |values: &[(&str, &str)]| {
            values
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect()
        };
        match self {
            Self::GoogleDrive => Provider {
                id: "google".into(),
                label: "Google".into(),
                authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token_endpoint: "https://oauth2.googleapis.com/token".into(),
                revocation: Revocation::Rfc7009 {
                    endpoint: "https://oauth2.googleapis.com/revoke".into(),
                },
                scopes: owned(&["https://www.googleapis.com/auth/drive"]),
                // Without both, Google issues no refresh token on a repeat
                // consent.
                authorization_params: pairs(&[("access_type", "offline"), ("prompt", "consent")]),
            },
            // The `common` tenant accepts both personal Microsoft accounts
            // and work or school accounts; `offline_access` yields the
            // refresh token.
            Self::OneDrive => Provider {
                id: "microsoft".into(),
                label: "Microsoft".into(),
                authorization_endpoint:
                    "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
                token_endpoint: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
                revocation: Revocation::Unsupported,
                scopes: owned(&["offline_access", "Files.ReadWrite.All", "User.Read"]),
                authorization_params: pairs(&[("prompt", "select_account")]),
            },
            Self::Dropbox => Provider {
                id: "dropbox".into(),
                label: "Dropbox".into(),
                authorization_endpoint: "https://www.dropbox.com/oauth2/authorize".into(),
                token_endpoint: "https://api.dropboxapi.com/oauth2/token".into(),
                revocation: Revocation::BearerPost {
                    endpoint: "https://api.dropboxapi.com/2/auth/token/revoke".into(),
                },
                scopes: owned(&[
                    "account_info.read",
                    "files.metadata.read",
                    "files.metadata.write",
                    "files.content.read",
                    "files.content.write",
                ]),
                authorization_params: pairs(&[("token_access_type", "offline")]),
            },
        }
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
        let keys: std::collections::BTreeSet<_> = OAuthCloud::ALL
            .into_iter()
            .map(|provider| provider.client_id().key)
            .collect();
        assert_eq!(keys.len(), 3);
        assert_eq!(OAuthCloud::from_type_id(WEBDAV_TYPE), None);
    }
}
