//! The community site this computer is configured with, and the desk's own way into the workspace
//! directory: which person a join is made as, and what the sheet shows of it.
//! What a site lists and what its door grants is [`tilo_workspace_directory`]'s: one implementation
//! for this session and for a browser page. Here is what is this computer's own — the
//! administrator's or the vendor's `community.toml` , the identity a join is made as, and the
//! workspaces the desk already holds.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tilo_desktop_surface::DesktopHandle;
use tilo_sheets::directory::Directory;
use tilo_workspace_directory::Person;

pub(crate) use tilo_workspace_directory::{Joined, Listed, Store};

/// The vendor's community site.
const VENDOR_CONFIG: &str = "/usr/share/tilo/community.toml";
/// The administrator's, which wins.
const SYSTEM_CONFIG: &str = "/etc/tilo/community.toml";
/// Overrides both paths, for a test or a development session.
pub(crate) const CONFIG_ENV: &str = "TILO_COMMUNITY_CONFIG";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommunityToml {
    /// The site whose home workspace a computer is in by default; empty for none.
    #[serde(default)]
    site: String,
    /// Where a report about Tilo itself goes, when this computer offers one.
    #[serde(default)]
    feedback: Option<crate::feedback::Destination>,
}

/// What this computer is told: the community site, and where reports go.
#[derive(Default)]
struct Configured {
    site: Option<url::Url>,
    feedback: Option<crate::feedback::Destination>,
}

/// The community site configured for this computer, if any.
pub(crate) fn community_site() -> Option<url::Url> {
    configured().site
}

/// Where a report about Tilo itself goes, if this computer is told.
pub(crate) fn feedback() -> Option<crate::feedback::Destination> {
    configured().feedback
}

/// The paths this computer's configuration is read from, in the order they answer: the
/// administrator's file, else the vendor's, unless the environment names the one file to read.
fn config_paths() -> Vec<PathBuf> {
    match std::env::var_os(CONFIG_ENV) {
        Some(path) => vec![PathBuf::from(path)],
        None => vec![PathBuf::from(SYSTEM_CONFIG), PathBuf::from(VENDOR_CONFIG)],
    }
}

/// What this computer's configuration says.
fn configured() -> Configured {
    configured_from(config_paths().iter().map(PathBuf::as_path))
}

/// What the first readable file of `paths` says. A file that does not parse is reported and
/// skipped.
fn configured_from<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Configured {
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
        let site = if config.site.trim().is_empty() {
            None
        } else {
            match tilo_workspace_directory::site_entry(&config.site) {
                Ok(site) => Some(site),
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "community site is not a web address");
                    None
                }
            }
        };
        return Configured {
            site,
            feedback: config.feedback,
        };
    }
    Configured::default()
}

/// Read the directories again: the community site's, and `site` 's when the person named one.
pub(crate) fn ask(store: &Store, site: Option<String>) {
    store.read(community_site(), site);
}

/// Join `listing`, which the sheet knows as `key`, as this computer's person.
///
/// # Errors
/// Why no door was asked: this computer has no identity to join as, or no
/// task to ask.
pub(crate) fn join(store: &Store, key: &str, listing: &Listed) -> Result<(), String> {
    let person = person().inspect_err(|reason| {
        tracing::info!(
            target: tilo_startup_metrics::JOIN_FLOW_TARGET,
            workspace = %listing.entry.workspace,
            key = %key,
            site = %listing.host,
            %reason,
            outcome = "no person to join as",
            "join: this computer has no person to join as"
        );
    })?;
    store.join(key, listing, &person)
}

/// Join the home workspace of the community `site` as this computer's person,
/// with nobody at the directory asking: what the site's `entry.json` names as
/// its home, through the same door a listing's Join asks.
///
/// # Errors
/// Why nothing was asked: this computer has no identity to join as, or no
/// task to ask.
pub(crate) fn join_community(store: &Store, site: &url::Url) -> Result<(), String> {
    store.join_community(site, &person()?)
}

/// Join what the person pasted into the sheet as this computer's person.
///
/// # Errors
/// Why nothing was asked: this computer has no identity to join as.
pub(crate) fn take(store: &Store, text: &str) -> Result<(), String> {
    store.take(text, &person()?)
}

/// The workspace an invitation for this computer joins, without redeeming
/// it.
///
/// # Errors
/// Why the invitation names no workspace for this computer: it is for someone
/// else, has expired, or is not an invitation.
pub(crate) fn invited_workspace(text: &str) -> Result<String, String> {
    tilo_workspace_directory::invited_workspace(text, &person()?)
}

/// This computer's person: the one its device certificate names.
///
/// # Errors
/// A computer with no identity of its own joins nowhere as anybody.
fn person() -> Result<Person, String> {
    let identity = crate::sync::device_identity().ok_or_else(|| {
        "this computer has no identity yet, so it cannot join a workspace".to_owned()
    })?;
    Ok(Person {
        user: identity.device_cert.user_public_key,
        name: own_person_name(ui::local_identity::load().map(|identity| identity.display_name)),
    })
}

/// The name this person goes by in a workspace: the one they chose, else their login account's. A
/// workspace, a public one above all, learns nothing of this person's devices.
pub(crate) fn own_person_name(chosen: Option<String>) -> String {
    crate::platform::person::chosen_name(chosen)
        .unwrap_or_else(tilo_identity_host::provisioning::login_account_display_name)
}

pub(crate) use crate::platform::person::desk_person_name;

/// What the sheet shows: the workspaces this computer holds, and what the sites' directories list.
pub(crate) fn view(store: &Store, handle: &DesktopHandle) -> Directory {
    let held = handle.read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| tilo_workspace_directory::Held {
                workspace: workspace.id().to_string(),
                key: workspace.slot.0.to_string(),
                name: workspace.name.clone(),
            })
            .collect::<Vec<_>>()
    });
    store.view(&held)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A site named is a community to join; an empty one is none, and a file that is not there or
    /// does not parse leaves the next one to answer.
    #[test]
    fn an_empty_site_is_no_community() {
        let dir = tempfile::tempdir().expect("a directory for the configurations");
        let write = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).expect("a configuration");
            path
        };
        let empty = write("empty.toml", "site = \"\"\n");
        let blank = write("blank.toml", "site = \"   \"\n");
        let broken = write("broken.toml", "site = \n");
        let named = write("named.toml", "site = \"tilo.sh\"\n");
        let missing = dir.path().join("missing.toml");
        let site_of = |paths: &[&Path]| configured_from(paths.iter().copied()).site;
        assert_eq!(
            site_of(&[named.as_path()]).map(|site| site.to_string()),
            Some("https://tilo.sh/".to_owned()),
            "a named site is the community"
        );
        assert_eq!(
            site_of(&[empty.as_path()]),
            None,
            "an administrator naming no site asks for no community"
        );
        assert_eq!(site_of(&[blank.as_path()]), None, "and a blank one is none");
        assert_eq!(
            site_of(&[empty.as_path(), named.as_path()]),
            None,
            "the first file that reads answers, empty or not"
        );
        assert_eq!(
            site_of(&[missing.as_path(), broken.as_path(), named.as_path()])
                .map(|site| site.to_string()),
            Some("https://tilo.sh/".to_owned()),
            "a file that is not there or does not parse leaves the next one to answer"
        );
    }

    /// A computer told where reports go names the workspace and the conversation, and one told
    /// nothing offers no report: a report with nowhere to go is not a report.
    #[test]
    fn a_report_goes_where_the_config_names_or_nowhere() {
        let dir = tempfile::tempdir().expect("a directory for the configurations");
        let write = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).expect("a configuration");
            path
        };
        let told = write(
            "told.toml",
            "site = \"tilo.sh\"\n[feedback]\nworkspace = \"w_tilo\"\nchannel = \"c_feedback\"\n",
        );
        let untold = write("untold.toml", "site = \"tilo.sh\"\n");
        let only_a_channel = write("half.toml", "[feedback]\nchannel = \"c_feedback\"\n");
        let configured = configured_from([told.as_path()]);
        assert_eq!(
            configured.feedback,
            Some(crate::feedback::Destination {
                workspace: "w_tilo".to_owned(),
                channel: "c_feedback".to_owned(),
            }),
            "the report goes to the workspace's conversation the config names"
        );
        assert!(configured.site.is_some(), "and the site stands beside it");
        assert_eq!(
            configured_from([untold.as_path()]).feedback,
            None,
            "a computer told nothing offers no report"
        );
        assert_eq!(
            configured_from([only_a_channel.as_path()]).feedback,
            None,
            "a destination with no workspace to keep it is no destination"
        );
    }

    #[test]
    fn the_desk_names_the_person_as_it_knows_them() {
        let key = "3f".repeat(32);
        assert_eq!(
            desk_person_name(Some("Ada Lovelace".into()), &key),
            "Ada Lovelace"
        );
        assert_eq!(
            desk_person_name(None, "Pat"),
            "Pat",
            "a computer with no identity goes by the name its session acts as"
        );
        assert_eq!(
            desk_person_name(Some(runtime_identity::UNNAMED_DISPLAY_NAME.into()), "Pat"),
            "Pat",
            "an unnamed identity leaves the session's name standing"
        );
        let account = tilo_identity_host::provisioning::login_account_display_name();
        assert_eq!(
            desk_person_name(None, &key),
            account,
            "a key is never shown as the person's name"
        );
    }
}
