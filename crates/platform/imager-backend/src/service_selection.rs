//! Read-only website-session inventory and explicitly selected capture.
//! Native source authority remains inside the backend, never in presentation state.
use crate::account_transfer::Candidate;
use browser_continuity::{Provider, sessions::Service};
use std::{io, path::Path, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    Cookies,
    DiscordOrigin,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Readable,
    NeedsAccess,
    Unavailable,
}
pub struct CookieProfile {
    pub id: String,
    pub label: String,
    pub provider: Provider,
    pub material: Material,
    _private: (),
    #[cfg(target_os = "linux")]
    native: Arc<crate::setup::browser_profiles::service_profiles::CookieProfile>,
}
impl CookieProfile {
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
                provider: source.provider,
                _private: (),
                material: match source.material {
                    Native::Cookies => Material::Cookies,
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
