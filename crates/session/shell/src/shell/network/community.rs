//! The community workspace a Tilo computer is in by default.
//!
//! Which one is configuration of the image, not of the code: the vendor
//! names its site in `/usr/share/tilo/community.toml`, an administrator
//! overrides it in `/etc/tilo/community.toml`, and an empty `site` there
//! turns it off. The session joins it in the background as its person
//! ([`ShellNetwork::join_site`]), without asking: a computer with an
//! identity is in its community the way a page on that site is. A site out
//! of reach is tried again quietly, with growing waits, and never holds the
//! session up or shows an error; a site that lets people in by invitation
//! only is left alone.
//!
//! The person can leave it (the workspace switcher's Leave). That is
//! remembered with the rooms held through sites and never undone silently;
//! Settings offers joining again. A host set up again behind the same site
//! (another device answers there) is joined anew as the same person, as a
//! browser does: nobody is asked to fix a host's state.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use serde::Deserialize;

use super::hosted_sites::{self, SiteError};
use super::{RoomId, ShellNetwork};

/// The vendor's community site.
const VENDOR_CONFIG: &str = "/usr/share/tilo/community.toml";
/// The administrator's, which wins.
const SYSTEM_CONFIG: &str = "/etc/tilo/community.toml";
/// Overrides both paths, for a test or a development session.
pub const CONFIG_ENV: &str = "TILO_COMMUNITY_CONFIG";

/// The first wait after a site was out of reach.
const FIRST_RETRY: Duration = Duration::from_secs(5);
/// The longest wait between tries.
const LONGEST_RETRY: Duration = Duration::from_secs(15 * 60);
/// How often a held community workspace's host is checked for having been
/// set up again.
const CHECK_INTERVAL: Duration = Duration::from_secs(10 * 60);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommunityToml {
    /// The site whose home workspace a computer is in by default; empty for
    /// none.
    #[serde(default)]
    site: String,
}

/// The community site configured for this computer, if any.
pub(crate) fn configured_site() -> Option<url::Url> {
    let paths: Vec<PathBuf> = match std::env::var_os(CONFIG_ENV) {
        Some(path) => vec![PathBuf::from(path)],
        None => vec![PathBuf::from(SYSTEM_CONFIG), PathBuf::from(VENDOR_CONFIG)],
    };
    site_from(paths.iter().map(PathBuf::as_path))
}

/// The site the first readable file of `paths` names. A file that does not
/// parse is reported and skipped.
fn site_from<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Option<url::Url> {
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let config: CommunityToml = match toml::from_str(&text) {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "community config does not parse; skipping it");
                continue;
            }
        };
        if config.site.trim().is_empty() {
            return None;
        }
        return match hosted_sites::site_entry(&config.site) {
            Ok(site) => Some(site),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "community site is not a web address");
                None
            }
        };
    }
    None
}

/// What one pass of keeping the community membership found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    /// Held (or being joined): look again after [`CHECK_INTERVAL`].
    Held,
    /// Not reached now: try again after a growing wait.
    Unreachable,
    /// Nothing to do: no identity yet, the person left, or the site takes
    /// invitations only.
    Idle,
}

impl ShellNetwork {
    /// One pass for the community site `site`: join it unless the person
    /// left it, and join it anew when its host was set up again.
    pub(crate) fn keep_community(&self, site: &url::Url) -> Pass {
        if self.hosted_sites.left(site) {
            return Pass::Idle;
        }
        if let Some(room) = self.hosted_sites.room_of_site(site)
            && self.holds_room(room)
            && !self.host_was_set_up_again(site, room)
        {
            return Pass::Held;
        }
        match self.join_site(site) {
            Ok(_) => Pass::Held,
            Err(SiteError::Unreachable(error)) => {
                tracing::debug!(%error, site = %site, "community site not reached");
                Pass::Unreachable
            }
            Err(error) => {
                tracing::info!(%error, site = %site, "not joining the community workspace");
                Pass::Idle
            }
        }
    }

    pub(super) fn holds_room(&self, room: RoomId) -> bool {
        self.replica.lock().is_ok_and(|replica| {
            replica
                .tree
                .read()
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room == room)
        })
    }

    /// Whether another host than the one `room` was joined at answers for
    /// `site` now: another device at its origin, or another origin the
    /// site names. The workspace of the host before is dropped here, so the
    /// site is joined anew; a site or host not reached is not judged.
    fn host_was_set_up_again(&self, site: &url::Url, room: RoomId) -> bool {
        let Some(hosted) = self.hosted_sites.hosted(room) else {
            return false;
        };
        let (Some(pinned), Ok(runtime)) = (hosted.host.as_deref(), self.runtime_handle()) else {
            return false;
        };
        let Ok(current) = runtime.block_on(hosted_sites::resolve_site(site)) else {
            return false;
        };
        let Ok(http) = hosted_sites::HostHttp::new(&current.origin) else {
            return false;
        };
        let Ok(info) = runtime.block_on(tilo_relay_client::https::host_info(http.as_ref())) else {
            return false;
        };
        if current.origin == hosted.origin
            && hex::encode(info.host_cert.device_public_key) == pinned
        {
            return false;
        }
        tracing::info!(%room, "the community workspace's host was set up again; joining it anew");
        self.drop_hosted_room(room);
        true
    }

    /// Stop holding `room`, a room held through a site, here.
    pub(crate) fn drop_hosted_room(&self, room: RoomId) {
        self.remove_workspace_of(room);
        if let Err(error) = self.hosted_sites.forget(room) {
            tracing::warn!(%error, %room, "a dropped site room is still remembered");
        }
    }

    /// Take the workspace showing `room` off this computer's desktop.
    pub(crate) fn remove_workspace_of(&self, room: RoomId) {
        if let Ok(replica) = self.replica.lock() {
            let mut tree = replica.tree.write();
            let slot = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .map(|workspace| workspace.id);
            if let Some(slot) = slot {
                tree.delete_workspace(slot);
            }
        }
    }

    /// Keep this computer in its configured community workspace for as long
    /// as the network runs, on a thread of its own.
    pub(crate) fn follow_community(self: &Arc<Self>) {
        let Some(site) = configured_site() else {
            return;
        };
        self.hosted_sites.follow_community(&site);
        let net: Weak<Self> = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("tilo-community".into())
            .spawn(move || {
                let mut retry = FIRST_RETRY;
                while let Some(network) = net.upgrade() {
                    let pass = network.keep_community(&site);
                    drop(network);
                    let wait = match pass {
                        Pass::Held | Pass::Idle => {
                            retry = FIRST_RETRY;
                            CHECK_INTERVAL
                        }
                        Pass::Unreachable => {
                            let wait = retry;
                            retry = retry.saturating_mul(2).min(LONGEST_RETRY);
                            wait
                        }
                    };
                    std::thread::park_timeout(wait);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "the community workspace is not followed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_administrators_site_wins_and_an_empty_one_turns_it_off() {
        let dir = tempfile::tempdir().unwrap();
        let vendor = dir.path().join("vendor.toml");
        let system = dir.path().join("system.toml");
        let missing = dir.path().join("missing.toml");
        std::fs::write(&vendor, "site = \"tilo.coffee\"\n").unwrap();
        assert_eq!(
            site_from([missing.as_path(), vendor.as_path()]).map(String::from),
            Some("https://tilo.coffee/".to_owned())
        );
        std::fs::write(&system, "site = \"https://community.example/\"\n").unwrap();
        assert_eq!(
            site_from([system.as_path(), vendor.as_path()]).map(String::from),
            Some("https://community.example/".to_owned())
        );
        std::fs::write(&system, "site = \"\"\n").unwrap();
        assert_eq!(site_from([system.as_path(), vendor.as_path()]), None);
        std::fs::write(&system, "site = [\n").unwrap();
        assert_eq!(
            site_from([system.as_path(), vendor.as_path()]).map(String::from),
            Some("https://tilo.coffee/".to_owned()),
            "a broken file is skipped"
        );
    }
}
