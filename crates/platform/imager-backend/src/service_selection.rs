//! Read-only website-session inventory and explicitly selected capture.
//! Native source authority remains inside the backend, never in presentation state.
use crate::account_transfer::Candidate;
use browser_continuity::{Provider, sessions::Service};
use serde::Serialize;
use std::{io, path::Path, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    Cookies,
    #[cfg(feature = "installation")]
    DiscordOrigin,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Readable,
    NeedsAccess,
    Unavailable,
}

/// Value-free size of one recognized service's cookie material. These counts
/// describe website data; they never establish an authenticated account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ServiceCounts {
    pub cookies: u64,
    pub origins: u64,
}
pub struct CookieProfile {
    pub id: String,
    pub label: String,
    /// Registry-owned product identity, not inferred from private profile paths.
    pub application: String,
    pub native_application: bool,
    pub provider: Provider,
    pub material: Material,
    _private: (),
    #[cfg(target_os = "linux")]
    native: Arc<crate::setup::browser_profiles::service_profiles::CookieProfile>,
}
impl CookieProfile {
    /// The opaque reviewed browser profile this source belongs to, when the
    /// source was discovered from one. A native application store keeps its own
    /// identity and returns `None`; no path or label is consulted.
    #[must_use]
    pub fn browser_profile_id(&self) -> Option<&str> {
        if self.native_application {
            return None;
        }
        let id = self
            .id
            .strip_suffix("-discord-origin")
            .unwrap_or(self.id.as_str());
        (id.len() == 64
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then_some(id)
    }
    pub fn presence(&self, service: Service) -> io::Result<Presence> {
        #[cfg(target_os = "linux")]
        {
            use crate::setup::browser_profiles::service_profiles::Presence as Native;
            self.native
                .presence(service)
                .map(|presence| match presence {
                    Native::Readable => Presence::Readable,
                    Native::NeedsAccess => Presence::NeedsAccess,
                    Native::Unavailable => Presence::Unavailable,
                })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = service;
            Err(unsupported())
        }
    }
    /// Count this profile's cookies for one recognized service without reading
    /// values. `None` means this source carries no cookie material for it.
    ///
    /// # Errors
    /// Refuses changed or unsafe source authority and unsupported cookie schemas.
    pub fn counts(&self, service: Service) -> io::Result<Option<ServiceCounts>> {
        #[cfg(target_os = "linux")]
        {
            self.native
                .counts(service)
                .map(|counts| counts.map(|(cookies, origins)| ServiceCounts { cookies, origins }))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = service;
            Err(unsupported())
        }
    }
    pub fn supports_encrypted_cookie_capture(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.native.supports_encrypted_cookie_capture()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
    pub fn validate(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.native.validate()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported())
        }
    }
}
pub struct Discovery {
    pub sources: Vec<Arc<CookieProfile>>,
    pub unavailable: bool,
    #[cfg(target_os = "linux")]
    native: crate::setup::browser_profiles::service_profiles::Discovery,
}
impl Discovery {
    pub fn validate_home(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.native.validate_home()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported())
        }
    }
}
/// Unsupported hosts do not inspect paths or claim that sources were discovered.
pub fn supported() -> bool {
    cfg!(target_os = "linux")
}
pub fn discover(home: &Path, candidates: &[Candidate]) -> io::Result<Discovery> {
    #[cfg(target_os = "linux")]
    {
        crate::setup::browser_profiles::service_profiles::discover(home, candidates).map(wrap)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (home, candidates);
        Err(unsupported())
    }
}
#[cfg(target_os = "linux")]
fn wrap(native: crate::setup::browser_profiles::service_profiles::Discovery) -> Discovery {
    use crate::setup::browser_profiles::service_profiles::Material as Native;
    let sources = native
        .sources
        .iter()
        .map(|source| {
            Arc::new(CookieProfile {
                id: source.id.clone(),
                label: source.label.clone(),
                application: source.application.clone(),
                native_application: source.native_application,
                provider: source.provider,
                _private: (),
                material: match source.material {
                    Native::Cookies => Material::Cookies,
                    #[cfg(feature = "installation")]
                    Native::DiscordOrigin => Material::DiscordOrigin,
                },
                native: Arc::clone(source),
            })
        })
        .collect();
    Discovery {
        sources,
        unavailable: native.unavailable,
        native,
    }
}
/// Capture requires an inventory-owned profile. Public labels cannot mint a source grant.
pub fn capture_selected(
    identity: [u8; 32],
    binding: String,
    sources: &[(Arc<CookieProfile>, Service)],
) -> io::Result<crate::setup::service_sessions::PreparedSessions> {
    #[cfg(target_os = "linux")]
    {
        use crate::setup::service_sessions::{PreparedSessions, SelectedSource};
        let mut grants = Vec::new();
        for (source, service) in sources {
            use crate::setup::browser_profiles::service_profiles::Material as Native;
            grants.push(match source.native.material {
                Native::Cookies => SelectedSource::Cookies(source.native.grant()?, *service),
                #[cfg(feature = "installation")]
                Native::DiscordOrigin => {
                    SelectedSource::DiscordOrigin(source.native.origin_grant()?)
                }
            });
        }
        let captured = PreparedSessions::capture_selected(identity, binding, grants)?;
        for (source, _) in sources {
            source.validate()?;
        }
        Ok(captured)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (identity, binding, sources);
        Err(unsupported())
    }
}
#[cfg(all(target_os = "linux", any(test, feature = "test-support")))]
pub fn discover_without_slack_package(
    home: &Path,
    candidates: &[Candidate],
) -> io::Result<Discovery> {
    crate::setup::browser_profiles::service_profiles::discover_without_slack_package(
        home, candidates,
    )
    .map(wrap)
}
#[cfg(not(target_os = "linux"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Website session capture is unavailable on this platform",
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn recognized_source_counts_cookies_without_claiming_an_account() {
        let home = tempfile::tempdir().unwrap();
        let profile = home.path().join(".mozilla/firefox/first.default");
        fs::create_dir_all(&profile).unwrap();
        fs::write(
            home.path().join(".mozilla/firefox/profiles.ini"),
            "[Profile0]\nName=Personal\nIsRelative=1\nPath=first.default\n",
        )
        .unwrap();
        let cookies = rusqlite::Connection::open(profile.join("cookies.sqlite")).unwrap();
        cookies
            .execute_batch(
                "PRAGMA user_version=17;
                 CREATE TABLE moz_cookies(host TEXT, value TEXT);
                 INSERT INTO moz_cookies VALUES('.slack.com','PRIVATE_COOKIE_SENTINEL');
                 INSERT INTO moz_cookies VALUES('app.slack.com','PRIVATE_COOKIE_SENTINEL');",
            )
            .unwrap();
        drop(cookies);
        let candidates = crate::account_sources::discover(&crate::account_sources::SourceRoots {
            home: home.path().into(),
            config: home.path().join(".config"),
        });
        let discovery = discover(home.path(), &candidates.candidates).unwrap();
        let source = discovery.sources.first().unwrap();
        let counts = source.counts(Service::Slack).unwrap().unwrap();
        assert_eq!(counts.cookies, 2);
        assert_eq!(counts.origins, 2);
        assert!(!format!("{counts:?}").contains("PRIVATE_COOKIE_SENTINEL"));
        assert_eq!(source.counts(Service::Discord).unwrap().unwrap().cookies, 0);
    }
}
