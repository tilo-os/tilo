//! A collaborator whose device speaks another peer protocol version.
//!
//! Two devices sync only at the same [`neighbour_wire::PEER_PROTOCOL_VERSION`]
//! (docs/architecture/compatibility.md): at another, neither reads anything
//! the other says, so nothing arrives half-applied, and each keeps its own
//! changes until they can be exchanged. Whichever device is behind updates:
//! this one starts its system update at once and asks to restart as soon as
//! the update is ready; for a collaborator who is behind, the person is told
//! "Ana needs to update to keep working together". Sync resumes by itself
//! once both speak the same version.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_lock::MutexRecover as _;

use crate::shell::i18n::t_args;
use crate::shell::state::WorkspaceTree;

/// A device that answered at another version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mismatch {
    /// Its transport endpoint id, as the rooms name their peers.
    pub endpoint: String,
    /// The name on its certificate, when it gave one.
    pub device_label: String,
    /// The version it speaks.
    pub theirs: u32,
}

/// Mismatches waiting for the compositor, and those already told this
/// session: each device and version once.
static PENDING: Mutex<Vec<Mismatch>> = Mutex::new(Vec::new());
static TOLD: LazyLock<Mutex<HashSet<(String, u32)>>> = LazyLock::new(Mutex::default);

/// A device spoke `theirs`; this device speaks another version.
pub(crate) fn note(endpoint: String, device_label: String, theirs: u32) {
    if theirs == neighbour_wire::PEER_PROTOCOL_VERSION
        || !TOLD.lock_recover().insert((endpoint.clone(), theirs))
    {
        return;
    }
    tracing::info!(
        endpoint,
        theirs,
        ours = neighbour_wire::PEER_PROTOCOL_VERSION,
        "a collaborator's device speaks another peer protocol version"
    );
    if theirs > neighbour_wire::PEER_PROTOCOL_VERSION {
        crate::shell::system_update::update_now_and_ask();
    }
    PENDING.lock_recover().push(Mismatch {
        endpoint,
        device_label,
        theirs,
    });
}

/// Who `mismatch` is, by the name its rooms know its device under, else by
/// the name on its certificate.
fn name_of(tree: &WorkspaceTree, mismatch: &Mismatch) -> Option<String> {
    tree.room_peer_names
        .values()
        .find_map(|names| names.get(&mismatch.endpoint).cloned())
        .or_else(|| {
            let label = mismatch.device_label.trim();
            (!label.is_empty()).then(|| label.to_owned())
        })
}

/// What to tell the person about `mismatch`.
pub(crate) fn notice(name: Option<&str>, theirs: u32) -> (String, String) {
    let behind = theirs < neighbour_wire::PEER_PROTOCOL_VERSION;
    match (name, behind) {
        (Some(name), true) => (
            t_args(
                "collaborator-needs-update",
                &[("name", name.to_owned().into())],
            ),
            t_args(
                "collaborator-needs-update-body",
                &[("name", name.to_owned().into())],
            ),
        ),
        (None, true) => (
            crate::shell::i18n::t("collaborator-host-needs-update"),
            crate::shell::i18n::t("collaborator-host-needs-update-body"),
        ),
        (Some(name), false) => (
            t_args(
                "collaborator-update-self",
                &[("name", name.to_owned().into())],
            ),
            crate::shell::i18n::t("collaborator-update-self-body"),
        ),
        (None, false) => (
            crate::shell::i18n::t("collaborator-update-self-host"),
            crate::shell::i18n::t("collaborator-update-self-body"),
        ),
    }
}

/// Notices since the last call.
pub(crate) fn drain(tree: &WorkspaceTree) -> Vec<Notification> {
    std::mem::take(&mut *PENDING.lock_recover())
        .into_iter()
        .map(|mismatch| {
            let (title, body) = notice(name_of(tree, &mismatch).as_deref(), mismatch.theirs);
            Notification::new(title, body)
                .with_severity(NotificationSeverity::Info)
                .with_app_name(crate::shell::i18n::t("system-update-app-name"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collaborator_behind_is_named_and_this_device_behind_updates() {
        let ours = neighbour_wire::PEER_PROTOCOL_VERSION;
        let (title, _) = notice(Some("Ana"), ours - 1);
        assert_eq!(title, "Ana needs to update to keep working together");
        let (title, _) = notice(Some("Ana"), ours + 1);
        assert_eq!(title, "Updating Tilo to keep working with Ana");
        let (title, _) = notice(None, ours - 1);
        assert!(!title.is_empty());
    }

    #[test]
    fn each_device_and_version_is_told_once() {
        let endpoint = "told-once-endpoint".to_owned();
        let ours = neighbour_wire::PEER_PROTOCOL_VERSION;
        note(endpoint.clone(), "Ana's laptop".into(), ours - 1);
        note(endpoint.clone(), "Ana's laptop".into(), ours - 1);
        note(endpoint.clone(), "Ana's laptop".into(), ours);
        let told: Vec<_> = PENDING
            .lock_recover()
            .iter()
            .filter(|mismatch| mismatch.endpoint == endpoint)
            .cloned()
            .collect();
        assert_eq!(
            told,
            vec![Mismatch {
                endpoint,
                device_label: "Ana's laptop".into(),
                theirs: ours - 1,
            }]
        );
    }
}
