//! The daemon's shortcut library, driven as the broker drives it: a shortcut saved through it
//! lands in its workspace, where the workspace lists it, a delete removes it, and what this
//! device cannot store is refused.

use std::sync::Arc;

use tilo_action_policy::MemberId;
use tilo_app_host::automation::ShortcutChange;
use tilo_app_host::services::{
    ShortcutLibraryRequest, ShortcutLibraryServices, WorkspaceAuthority,
};
use tilo_desktop_model::WorkspaceId;

use super::Shortcuts;
use crate::device::workspaces::ServiceWorkspaces;

/// One change for the library to store, as an app's call carries it.
fn change(change: serde_json::Value) -> ShortcutChange {
    serde_json::from_value(change).expect("a shortcut change")
}

/// The request the broker makes for `change` in `workspace`, saved by `actor`.
fn saving(workspace: &str, actor: &MemberId, change: ShortcutChange) -> ShortcutLibraryRequest {
    ShortcutLibraryRequest {
        workspace: workspace.to_owned(),
        actor: actor.clone().into(),
        acting_for: None,
        change,
        context: tilo_actions::ActionContext::default(),
        invocation: tilo_actions::InvocationId::new(),
    }
}

/// A shortcut called `name`, saved as `id`, with `triggers`.
fn shortcut(id: &str, name: &str, triggers: &serde_json::Value) -> ShortcutChange {
    change(serde_json::json!({
        "save": { "id": id, "name": name, "steps": [], "triggers": triggers },
    }))
}

/// A library over a workspace this device hosts, and who saves in it here.
struct Held {
    workspaces: Arc<ServiceWorkspaces>,
    library: Shortcuts,
    placed: WorkspaceId,
    /// The person this device acts for, as its shortcuts are saved by them.
    person: String,
    _directory: tempfile::TempDir,
}

impl Held {
    async fn new() -> Self {
        let placed = WorkspaceId::new();
        let (workspaces, directory) = ServiceWorkspaces::test_hosting(placed).await;
        let library = Shortcuts::new(
            workspaces.copies().clone(),
            workspaces.device_cert(),
            workspaces.signer(),
        )
        .expect("a runtime for shortcut runs");
        let person = hex::encode(workspaces.device_cert().user_public_key);
        Self {
            workspaces,
            library,
            placed,
            person,
            _directory: directory,
        }
    }

    fn actor(&self) -> MemberId {
        MemberId::person(self.person.clone())
    }

    /// Store `change` as `actor` saves it, as the broker asks the library to.
    async fn save(&self, actor: &MemberId, change: ShortcutChange) -> Result<(), String> {
        self.library
            .store(saving(&self.placed.to_string(), actor, change))
            .await
    }

    /// The workspace's shortcuts now, each as it holds it: what the broker lists.
    fn listed(&self) -> Vec<(String, String, Option<String>)> {
        let described = self
            .workspaces
            .workspace(self.placed)
            .expect("the fixture's workspace is held here");
        described
            .shortcuts
            .iter()
            .map(|saved| {
                let saved = saved.decode().expect("a saved shortcut reads back");
                (saved.id, saved.name, saved.saved_by)
            })
            .collect()
    }
}

/// A shortcut saved through this device's library is one of its workspace's settings, listed
/// there as saved by this device's person; an update replaces it and a delete removes it.
#[tokio::test]
async fn a_shortcut_saved_through_the_daemons_library_is_stored_in_its_workspace() {
    let held = Held::new().await;
    let actor = held.actor();
    let nothing = serde_json::json!([]);

    // Save: the changeless shortcut lands in the workspace, signed by this device as its
    // saver's own.
    assert_eq!(
        held.save(&actor, shortcut("tidy", "Tidy", &nothing)).await,
        Ok(())
    );
    assert_eq!(
        held.listed(),
        vec![("tidy".into(), "Tidy".into(), Some(held.person.clone()))],
        "the workspace lists the shortcut the library saved"
    );
    let described = held
        .workspaces
        .workspace(held.placed)
        .expect("a held workspace");
    let saved = described.shortcuts[0].decode().expect("it reads back");
    let revocations = held
        .workspaces
        .copies()
        .workspace_revocations(&held.placed.to_string())
        .expect("the workspace's revocations");
    assert_eq!(
        tilo_peer_state::workspace::proven_saver(
            &held.placed.to_string(),
            &saved,
            &revocations,
            tilo_clock::unix_now_ns(),
        )
        .as_deref(),
        Some(held.person.as_str()),
        "the shortcut carries this device's proof of who saved it"
    );

    // Update: saving the same id again replaces it, as an app's `update` does.
    assert_eq!(
        held.save(&actor, shortcut("tidy", "Tidier", &nothing))
            .await,
        Ok(())
    );
    assert_eq!(
        held.listed(),
        vec![("tidy".into(), "Tidier".into(), Some(held.person.clone()))],
        "one shortcut stands under the id, as it was saved last"
    );

    // A triggered shortcut is stored too, its trigger watched here.
    assert_eq!(
        held.save(
            &actor,
            shortcut(
                "palette",
                "Palette",
                &serde_json::json!([{ "trigger": "keyboard", "chord": "Super+Shift+K" }]),
            ),
        )
        .await,
        Ok(())
    );
    assert_eq!(held.listed().len(), 2);

    // Delete: the shortcut leaves the workspace, and deleting it again says there is none.
    assert_eq!(
        held.save(&actor, change(serde_json::json!({ "delete": "tidy" })))
            .await,
        Ok(())
    );
    assert_eq!(
        held.listed(),
        vec![(
            "palette".into(),
            "Palette".into(),
            Some(held.person.clone())
        )],
        "the deleted shortcut is gone and the other stays"
    );
    let missing = held
        .save(&actor, change(serde_json::json!({ "delete": "tidy" })))
        .await;
    assert!(
        missing
            .as_ref()
            .is_err_and(|why| why.contains("there is no shortcut `tidy`")),
        "{missing:?}"
    );
}

/// The library refuses a change another person's device made, a trigger this device cannot
/// watch, and a workspace this device does not hold.
#[tokio::test]
async fn the_library_refuses_what_this_device_cannot_store() {
    let held = Held::new().await;
    let actor = held.actor();
    let nothing = serde_json::json!([]);

    // A shortcut is saved from its saver's own device: nobody else's change lands.
    let elsewhere = held
        .save(
            &MemberId::person(hex::encode([7; 32])),
            shortcut("theirs", "Theirs", &nothing),
        )
        .await;
    assert!(
        elsewhere
            .as_ref()
            .is_err_and(|why| why == "a shortcut is saved from its saver's own device"),
        "{elsewhere:?}"
    );
    assert!(held.listed().is_empty(), "nothing was stored");

    // A trigger this device cannot watch is refused before it is saved.
    let unwatchable = held
        .save(
            &actor,
            shortcut(
                "often",
                "Often",
                &serde_json::json!([
                    { "trigger": "schedule", "schedule": { "every": "interval", "seconds": 0 } }
                ]),
            ),
        )
        .await;
    assert!(
        unwatchable
            .as_ref()
            .is_err_and(|why| why.contains("repeats at most once every")),
        "{unwatchable:?}"
    );
    assert!(held.listed().is_empty(), "nothing was stored");

    // A workspace this device does not hold takes no shortcut of its person.
    let unheld = held
        .library
        .store(saving(
            &WorkspaceId::new().to_string(),
            &actor,
            shortcut("elsewhere", "Elsewhere", &nothing),
        ))
        .await;
    assert!(unheld.is_err(), "{unheld:?}");
}

/// The library hands the broker this device's saved chords, the runtime a shortcut's run
/// executes on, and the desktop's own name for a fixed shortcut.
#[tokio::test]
async fn the_library_serves_the_devices_chords_its_run_runtime_and_fixed_labels() {
    let held = Held::new().await;

    assert_eq!(
        held.library.keymap(),
        settings_store::hardware::ShortcutConfig::load(),
        "the chords already taken on this device, as its settings keep them"
    );
    let runs = held.library.runtime().expect("the library runs shortcuts");
    assert_eq!(
        runs.spawn(async { 7 }).await.expect("the run ends"),
        7,
        "a shortcut's run executes on the library's runtime"
    );
    let fixed = settings_store::keyboard_shortcuts::fixed_shortcut("fixed-screenshot")
        .expect("the desktop's screenshot shortcut is fixed");
    assert_eq!(
        held.library.fixed_shortcut_label(fixed),
        fixed.id,
        "a fixed shortcut is named by its id where the desktop fixes no label"
    );
}
