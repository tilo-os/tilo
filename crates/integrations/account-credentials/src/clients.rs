//! Tilo's public OAuth client registrations, one per issuer and shared by every app that signs in
//! with it: what its developer console issued Tilo as an installed app, and the scopes of each use
//! (`files` for cloud storage, `mail` for IMAP and SMTP, `models` for hosted agent models). None is
//! confidential: an installed app keeps no secret (RFC 8252 §8.5), and PKCE binds each code to its
//! device. `TILO_OAUTH_<ISSUER>_CLIENT_ID` (and `TILO_OAUTH_GOOGLE_CLIENT_SECRET`) overrides an id
//! at runtime, then at build time; an empty id with none is "not configured". A sign-in redirects
//! to `http://127.0.0.1:<port>/oauth` unless the entry names its issuer's host and path.

use tilo_oauth::{Provider, Redirect, RedirectPorts, Refresh, Registration};

use crate::providers::{Capability, OAuthCloud, OAuthIssuer, OAuthUse};

pub const COPILOT_TOKEN_ENDPOINT: &str = "https://api.github.com/copilot_internal/v2/token";

/// The scopes each use of one client asks for; an empty list means this issuer
/// is not used for that.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scopes {
    pub files: &'static [&'static str],
    pub mail: &'static [&'static str],
    pub calendar: &'static [&'static str],
    pub identity: &'static [&'static str],
    pub music: &'static [&'static str],
    pub music_streaming: &'static [&'static str],
    pub models: &'static [&'static str],
    pub model_downloads: &'static [&'static str],
}

impl Scopes {
    pub const NONE: Self = Self {
        files: &[],
        mail: &[],
        calendar: &[],
        identity: &[],
        music: &[],
        music_streaming: &[],
        models: &[],
        model_downloads: &[],
    };

    pub const fn of(&self, capability: Capability) -> &'static [&'static str] {
        match capability {
            Capability::Files => self.files,
            Capability::Mail => self.mail,
            Capability::Calendar => self.calendar,
            Capability::Identity => self.identity,
            Capability::Music => self.music,
            Capability::MusicStreaming => self.music_streaming,
            Capability::Models => self.models,
            Capability::ModelDownloads => self.model_downloads,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientEntry {
    pub client_id: &'static str,
    pub client_secret: &'static str,
    /// Empty: any free loopback port.
    pub redirect_ports: &'static [u16],
    /// The redirect's host when the issuer registered one other than `127.0.0.1`;
    /// empty for that.
    pub redirect_host: &'static str,
    /// The redirect's path when the issuer registered one other than `/oauth`;
    /// empty for that.
    pub redirect_path: &'static str,
    pub paste_code_fallback: bool,
    pub scopes: Scopes,
}

impl ClientEntry {
    pub const NONE: Self = Self {
        client_id: "",
        client_secret: "",
        redirect_ports: &[],
        redirect_host: "",
        redirect_path: "",
        paste_code_fallback: false,
        scopes: Scopes::NONE,
    };
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientTable {
    pub google: ClientEntry,
    pub microsoft: ClientEntry,
    pub dropbox: ClientEntry,
    pub spotify: ClientEntry,
    pub openai: ClientEntry,
    pub github: ClientEntry,
    pub huggingface: ClientEntry,
}

/// The registrations this build signs in with.
static BUILT_IN: ClientTable = ClientTable {
    // Google Cloud console, OAuth client of type "Desktop app". Desktop clients accept a loopback
    // redirect on any port and path; nothing is registered.
    google: ClientEntry {
        client_id: "765058527434-b02qovqoi7lb7cbq872s8ec5ueaovjru.apps.googleusercontent.com",
        // Google's token endpoint requires the secret it issues to Desktop clients on the code
        // exchange and every refresh.
        client_secret: "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",
        scopes: Scopes {
            files: &["https://www.googleapis.com/auth/drive"],
            // Full IMAP and SMTP access; Gmail offers no narrower scope for either.
            mail: &["https://mail.google.com/"],
            // Google's CalDAV endpoint requires the calendar scope (not calendar.events). No
            // Drive, Gmail or Contacts permission is needed to read/write calendars.
            calendar: &["https://www.googleapis.com/auth/calendar"],
            // Vertex AI's OpenAI-compatible endpoint and listing the person's Cloud projects;
            // Vertex AI offers no narrower OAuth scope. Granted on its own, never together with
            // `files` or `mail`.
            models: &["https://www.googleapis.com/auth/cloud-platform"],
            ..Scopes::NONE
        },
        ..ClientEntry::NONE
    },
    // Microsoft Entra app registration, "Accounts in any organizational directory and personal
    // Microsoft accounts", public client. Registered redirect URI (platform "Mobile and desktop
    // applications", added through the manifest's `replyUrlsWithType` as type `InstalledClient`,
    // because the portal refuses http loopback URIs): http://127.0.0.1/oauth. Microsoft ignores
    // the port of a loopback redirect but matches its host and path.
    microsoft: ClientEntry {
        client_id: "4859a21d-e6df-4983-8725-2dc20060103b",
        scopes: Scopes {
            // Graph identity and Exchange mail need different audience access tokens. Consent to
            // identity separately, then request mail; never combine their scopes.
            identity: &["offline_access", "User.Read"],
            // `offline_access` makes Microsoft issue the refresh token.
            files: &["offline_access", "Files.ReadWrite.All", "User.Read"],
            // Exchange Online's IMAP and SMTP AUTH resources, not Microsoft Graph.
            mail: &[
                "offline_access",
                "https://outlook.office.com/IMAP.AccessAsUser.All",
                "https://outlook.office.com/SMTP.Send",
            ],
            ..Scopes::NONE
        },
        ..ClientEntry::NONE
    },
    // Dropbox App Console app key, PKCE without an app secret. Dropbox matches redirect URIs
    // exactly, port included, so each port below is registered as http://127.0.0.1:<port>/oauth.
    // When all of them are taken, Dropbox shows the code on its own page and Settings asks for it
    // to be pasted.
    dropbox: ClientEntry {
        client_id: "uzpownxibaxmz18",
        redirect_ports: &[47651, 47652, 47653],
        paste_code_fallback: true,
        scopes: Scopes {
            files: &[
                "account_info.read",
                "files.metadata.read",
                "files.metadata.write",
                "files.content.read",
                "files.content.write",
            ],
            ..Scopes::NONE
        },
        ..ClientEntry::NONE
    },
    // Public PKCE registration supplied by the distributor; never borrow another application's
    // identity. Register http://127.0.0.1:47654/oauth exactly.
    spotify: ClientEntry {
        client_id: "fc7bc68792734157a2c9857cba652df7",
        redirect_ports: &[47654],
        scopes: Scopes {
            music: &[
                "user-follow-read",
                "user-follow-modify",
                "user-read-private",
                "user-library-read",
                "user-library-modify",
                "playlist-read-private",
                "playlist-read-collaborative",
                "playlist-modify-private",
                "playlist-modify-public",
                "user-read-playback-state",
                "user-modify-playback-state",
            ],
            music_streaming: &["streaming", "user-read-private"],
            ..Scopes::NONE
        },
        ..ClientEntry::NONE
    },
    // Signing in with a ChatGPT account, to use its subscription for agent models. OpenAI offers
    // no client registration for ChatGPT subscriptions to other applications, so this is the
    // public client of OpenAI's own Codex CLI, used the way Codex uses it; the person signs in on
    // OpenAI's page and sees that client named. OpenAI matches its registered redirect exactly,
    // host and path included. An unofficial integration: OpenAI may change or close it at any
    // time. The `nightly-live-chatgpt` check signs nothing in but watches that this client, its
    // redirect and the backend still answer as expected, and Settings says when a sign-in or a
    // request stops working.
    openai: ClientEntry {
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
        redirect_ports: &[1455],
        redirect_host: "localhost",
        redirect_path: "/auth/callback",
        scopes: Scopes {
            // `offline_access` yields the refresh token.
            models: &["openid", "profile", "email", "offline_access"],
            ..Scopes::NONE
        },
        ..ClientEntry::NONE
    },
    // Signing in with GitHub, to use a Copilot subscription for agent models. GitHub issues
    // Copilot tokens only to the editors it supports, so this is the public client id GitHub's
    // Copilot editor extensions sign in with, by device code (no redirect is registered).
    // Unofficial, like the ChatGPT sign-in above.
    github: ClientEntry {
        client_id: "Iv1.b507a08c87ecfe98",
        scopes: Scopes {
            models: &["read:user"],
            ..Scopes::NONE
        },
        ..ClientEntry::NONE
    },
    // Hugging Face OAuth app, public client (no secret). Registered redirect URIs:
    // http://localhost/callback and http://127.0.0.1/callback; Hugging Face accepts any port on a
    // loopback redirect. Its scopes allow reading the person's profile and the gated models they
    // were granted, for the model manager's downloads. It has no `inference-api` scope, so hosted
    // Hugging Face inference still takes an access token typed as an API key.
    huggingface: ClientEntry {
        client_id: "3c6f0f73-efd1-47c9-ab21-55f84fcd7dab",
        redirect_path: "/callback",
        scopes: Scopes {
            model_downloads: &["openid", "profile", "gated-repos"],
            ..Scopes::NONE
        },
        ..ClientEntry::NONE
    },
};

impl ClientTable {
    /// The registrations this build signs in with.
    pub fn defaults() -> &'static Self {
        &BUILT_IN
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

    /// The scopes `oauth_use` asks for; empty when this table does not configure
    /// that use.
    pub fn scopes(&self, oauth_use: OAuthUse) -> &'static [&'static str] {
        self.client(oauth_use.issuer)
            .scopes
            .of(oauth_use.capability)
    }

    /// The issuer's descriptor with the scopes of `oauth_use`. A GitHub grant for
    /// models is a Copilot subscription's: it is renewed by trading the GitHub token
    /// for a Copilot token.
    pub fn descriptor(&self, oauth_use: OAuthUse) -> Provider {
        let refresh = match (oauth_use.issuer, oauth_use.capability) {
            (OAuthIssuer::GitHub, Capability::Models) => Refresh::CopilotToken {
                endpoint: COPILOT_TOKEN_ENDPOINT.into(),
            },
            _ => Refresh::RefreshToken,
        };
        Provider {
            scopes: self
                .scopes(oauth_use)
                .iter()
                .map(|scope| (*scope).to_owned())
                .collect(),
            refresh,
            ..oauth_use.issuer.endpoints()
        }
    }

    /// The registration of `issuer`: environment overrides first, then this table.
    /// `None` when no client id is known, the "not configured" state.
    pub fn client_registration(&self, issuer: OAuthIssuer) -> Option<Registration> {
        let entry = self.client(issuer);
        let non_empty = |value: &str| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        let client_id = issuer
            .client_id()
            .resolve()
            .or_else(|| non_empty(entry.client_id))?;
        let client_secret = issuer
            .client_secret()
            .and_then(|key| key.resolve())
            .or_else(|| non_empty(entry.client_secret));
        Some(Registration {
            client_id,
            client_secret,
            redirect_ports: if entry.redirect_ports.is_empty() {
                RedirectPorts::Any
            } else {
                RedirectPorts::Registered(entry.redirect_ports.to_vec())
            },
            redirect: {
                let default = Redirect::default();
                Redirect {
                    host: non_empty(entry.redirect_host).unwrap_or(default.host),
                    path: non_empty(entry.redirect_path).unwrap_or(default.path),
                }
            },
            paste_code_fallback: entry.paste_code_fallback,
        })
    }

    pub fn registration(&self, cloud: OAuthCloud) -> Option<Registration> {
        self.client_registration(cloud.issuer())
    }

    /// The descriptor and registration of `oauth_use`, `None` when the issuer has
    /// no client id or the use has no scopes.
    pub fn config(&self, oauth_use: OAuthUse) -> Option<(Provider, Registration)> {
        if self.scopes(oauth_use).is_empty() {
            return None;
        }
        let registration = self.client_registration(oauth_use.issuer)?;
        Some((self.descriptor(oauth_use), registration))
    }

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

    tilo_test_env::scenario! {
        /// `ClientTable`: the committed table, its scopes, and what an entry or the environment configures.
        fn the_client_table_configures_each_provider_and_use;

        phase the_committed_table_configures_every_cloud_and_use;
        {
            let table = ClientTable::defaults();
            for cloud in OAuthCloud::ALL {
                let (provider, client) = table.config(cloud.oauth_use()).expect("configured");
                assert!(!provider.scopes.is_empty(), "{cloud:?}");
                assert!(!client.client_id.is_empty(), "{cloud:?}");
            }
            // Google and Microsoft match loopback redirects without the port; Dropbox
            // matches exactly and so needs registered ports, or the code pasted instead.
            assert!(!table.dropbox.redirect_ports.is_empty());
            assert!(table.dropbox.paste_code_fallback);
            // Vertex AI asks for the cloud scope alone, in its own grant: none of the Google
            // uses shares a scope with another.
            let vertex = OAuthUse::new(OAuthIssuer::Google, Capability::Models);
            for other in [GMAIL, OAuthCloud::GoogleDrive.oauth_use()] {
                assert!(
                    table
                        .scopes(other)
                        .iter()
                        .all(|scope| !table.scopes(vertex).contains(scope))
                );
            }
            // Dropbox is storage only.
            assert!(!table.is_configured(OAuthUse::mail(OAuthIssuer::Dropbox)));
            let (gmail, gmail_client) = table.config(GMAIL).unwrap();
            let (drive, drive_client) = table.config(OAuthCloud::GoogleDrive.oauth_use()).unwrap();
            assert_eq!(gmail_client, drive_client);
            assert_eq!(gmail.token_endpoint, drive.token_endpoint);
            assert_ne!(gmail.scopes, drive.scopes);
        }

        phase the_model_subscriptions_sign_in_as_their_issuers_require;
        {
            let table = ClientTable::defaults();
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
            // A GitHub grant for anything else is refreshed as usual.
            assert_eq!(
                table
                    .descriptor(OAuthUse::identity(OAuthIssuer::GitHub))
                    .refresh,
                Refresh::RefreshToken
            );

            // Hugging Face signs in for downloads only, with a device code where there is no
            // browser.
            let downloads = OAuthUse::new(OAuthIssuer::HuggingFace, Capability::ModelDownloads);
            let (provider, client) = table.config(downloads).expect("Hugging Face is configured");
            assert!(provider.device_authorization_endpoint.is_some());
            assert_eq!(client.redirect.path, "/callback");
            assert!(!table.is_configured(OAuthUse::new(OAuthIssuer::HuggingFace, Capability::Models)));
        }

        phase an_empty_entry_is_not_configured_and_a_filled_one_is;
        {
            let mut table = ClientTable::default();
            table.google.client_id = "id.apps.googleusercontent.com";
            table.google.client_secret = "desktop-secret";
            table.google.scopes.mail = &["https://mail.google.com/"];
            table.dropbox.client_id = "key";
            table.dropbox.redirect_ports = &[1, 2];
            table.dropbox.paste_code_fallback = true;
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

        phase the_environment_overrides_the_table;
        {
            let mut table = ClientTable::default();
            table.google.client_id = "from-table";
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_OAUTH_GOOGLE_CLIENT_ID", "from-env")
                .set("TILO_OAUTH_GOOGLE_CLIENT_SECRET", "secret-from-env");
            let google = table.registration(OAuthCloud::GoogleDrive).unwrap();
            assert_eq!(google.client_id, "from-env");
            assert_eq!(google.secret(), Some("secret-from-env"));
        }
    }
}
