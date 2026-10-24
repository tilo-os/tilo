//! What a device writes to its own copy of a hosted workspace outlives the
//! room's next commit: every commit the device's owner admits projects the
//! room's document onto the workspace, so whatever was written only to the
//! local copy and never to the room is gone after it. Each journey writes
//! one of the fields the projection replaces, then lets the room commit a
//! change of its own (an edit to a note) and looks at the field again.

use std::sync::Arc;

use collab_types::caps::{Access, CapOverride, Scope, Subject};
use tilo_desktop_model::commands::DesktopCommand;
use tilo_user_identity::DeviceCapabilities;

use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::state::{GovernanceRequest, Workspace, WorkspaceId};
use crate::shell::workspace_governance::apply_governance_operation;

/// One device of Ada's hosting a workspace it holds a note in.
struct Hosted {
    device: Device,
    local: WorkspaceId,
    room: tilo_desktop_model::RoomId,
    ada: Person,
    _wakes: Arc<Wakes>,
    _dir: tempfile::TempDir,
    _home: tilo_test_env::TestHome,
}

impl Hosted {
    fn start(seed: u8) -> Self {
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let dir = scratch();
        let wakes = Arc::new(Wakes::default());
        let ada = Person::new(seed);
        home.env().set("TILO_USER_ID", ada.id());
        let device = Device::start(
            &ada,
            "Ada's desktop",
            &dir.path().join("desktop"),
            u16::from(seed) << 8,
            DeviceCapabilities::primary(),
            &wakes,
        );
        let (local, room) = hosted_workspace(&device, &[], "Projects");
        device.open_notes_first(room);
        until(&[&device], "the note's record to be held", || {
            device.holds_note(room).then_some(())
        });
        Self {
            device,
            local,
            room,
            ada,
            _wakes: wakes,
            _dir: dir,
            _home: home,
        }
    }

    /// The workspace as this device lists it.
    fn workspace(&self) -> Workspace {
        self.device
            .tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == self.local)
            .cloned()
            .expect("the device lists the workspace")
    }

    /// Wait until the workspace shows what `shown` looks for.
    fn until_shown(&self, what: &str, shown: impl Fn(&Workspace) -> bool) {
        until(&[&self.device], what, || {
            shown(&self.workspace()).then_some(())
        });
    }

    /// Save `shortcut` in the room as Ada, through the shortcuts library as
    /// the door saves one, and wait until the workspace lists it.
    fn save_shortcut(&self, shortcut: tilo_automation::Shortcut) {
        let actor = hex::encode(self.ada.user());
        let id = shortcut.id.clone();
        let saved = tilo_automation::Shortcut {
            saved_by: Some(actor.clone()),
            ..shortcut
        };
        let net = Arc::clone(self.device.net());
        let saving = crate::shell::broker_services::NativeShortcutLibrary::commit(
            self.device.tree.clone(),
            Some(net),
            self.room,
            actor,
            tilo_automation::ShortcutChange::Save(Box::new(saved)),
        );
        self.device
            .answer("the shortcut's save", saving)
            .unwrap_or_else(|error| panic!("Ada saves the shortcut: {error}"));
        self.until_shown("the saved shortcut to be listed", |workspace| {
            workspace.shortcuts.iter().any(|held| held.id == id)
        });
    }

    /// Let the room commit a change of its own and wait until this device
    /// holds it.
    fn commit_in_the_room(&self, text: &str) {
        self.device.write_note(self.room, text);
        until(&[&self.device], "the room's own commit to be held", || {
            (self.device.note(self.room).as_deref() == Some(text)).then_some(())
        });
    }
}

fn shortcut() -> tilo_automation::Shortcut {
    let hourly = tilo_automation::TriggerSpec::Schedule {
        schedule: tilo_automation::Schedule::Interval { seconds: 3600 },
        catch_up: tilo_automation::CatchUp::default(),
    };
    serde_json::from_value(serde_json::json!({
        "id": "tidy",
        "name": "Tidy",
        "steps": [],
        "triggers": [hourly],
    }))
    .expect("shortcut JSON")
}

#[test]
#[serial_test::serial]
fn a_shortcut_saved_on_the_hosting_device_outlives_the_rooms_next_commit() {
    let hosted = Hosted::start(0x51);
    hosted.save_shortcut(shortcut());
    hosted.commit_in_the_room("after the save");
    let ids: Vec<_> = hosted
        .workspace()
        .shortcuts
        .iter()
        .map(|shortcut| shortcut.id.clone())
        .collect();
    assert_eq!(ids, ["tidy"], "the saved shortcut is still listed");
}

#[test]
#[serial_test::serial]
fn a_fire_time_the_hosting_device_claims_outlives_the_rooms_next_commit() {
    let hosted = Hosted::start(0x52);
    let claim = tilo_automation::ScheduleClaim {
        shortcut: "tidy".into(),
        trigger: 0,
        due_ms: 1_800_000_000_000,
        device: hex::encode(hosted.device.cert().device_public_key),
    };
    hosted.save_shortcut(shortcut());
    assert!(
        hosted
            .device
            .tree
            .write()
            .claim_schedule(hosted.room, &claim)
    );
    hosted.commit_in_the_room("after the claim");
    assert_eq!(
        hosted.workspace().schedule_claims,
        [claim],
        "the claimed fire time is still held"
    );
}

#[test]
#[serial_test::serial]
fn an_effect_receipt_the_hosting_device_records_outlives_the_rooms_next_commit() {
    let hosted = Hosted::start(0x53);
    let invocation = tilo_actions::InvocationId::new();
    assert!(hosted.device.tree.write().append_workspace_effect_receipt(
        hosted.local,
        invocation,
        &hosted.ada.id(),
        tilo_actions::ActionContext::default(),
        tilo_actions::EffectReceiptStatus::Succeeded,
    ));
    hosted.commit_in_the_room("after the receipt");
    assert!(
        hosted
            .workspace()
            .effect_receipts
            .iter()
            .any(|receipt| receipt.invocation == invocation),
        "the recorded effect is still in the workspace's history"
    );
}

#[test]
#[serial_test::serial]
fn a_sandbox_capability_taken_back_stays_taken_back_across_the_rooms_next_commit() {
    let hosted = Hosted::start(0x54);
    let capability = CapOverride::grant(
        Subject::app("so.tilo.files"),
        Scope::FsPath("/disposable/document".into()),
        Access::Write,
    )
    .with_id("document");
    hosted.device.tree.write().request_governance(
        hosted.room,
        GovernanceRequest::GrantSandbox(capability.clone()),
    );
    hosted.until_shown("the room to grant the capability", |workspace| {
        workspace.sandbox.iter().any(|held| held.id == "document")
    });
    assert!(
        hosted
            .device
            .tree
            .write()
            .narrow_sandbox(hosted.local, &hosted.ada.id(), "document")
    );
    hosted.commit_in_the_room("after taking it back");
    assert!(
        !hosted
            .workspace()
            .sandbox
            .iter()
            .any(|held| held.id == "document"),
        "the capability taken back is not granted again"
    );
}

#[test]
#[serial_test::serial]
fn a_grant_set_saved_through_the_rooms_governance_reaches_the_room() {
    let hosted = Hosted::start(0x55);
    apply_governance_operation(
        &mut hosted.device.tree.write(),
        hosted.local,
        &hosted.ada.id(),
        tilo_desktop::workspace_settings::GovernanceOperation::SavePreset {
            name: "reviewers".into(),
            description: "Read and comment".into(),
            rules: vec![tilo_action_policy::Rule::new(
                hosted.workspace().node(),
                tilo_action_policy::KindSet::READ,
                tilo_action_policy::Answer::Allow,
            )],
        },
    )
    .expect("the owner may save a grant set");
    hosted.until_shown("the room to hold the grant set", |workspace| {
        workspace
            .presets
            .iter()
            .any(|preset| preset.name == "reviewers")
    });
    hosted.commit_in_the_room("after the grant set");
    assert!(
        hosted
            .workspace()
            .presets
            .iter()
            .any(|preset| preset.name == "reviewers"),
        "the grant set is still saved"
    );
}

#[test]
#[serial_test::serial]
fn a_name_given_through_the_rooms_governance_reaches_the_room() {
    let hosted = Hosted::start(0x56);
    hosted.device.tree.write().request_governance(
        hosted.room,
        GovernanceRequest::Command(DesktopCommand::RenameWorkspace {
            name: "Archive".into(),
        }),
    );
    hosted.until_shown("the room to carry the new name", |workspace| {
        workspace.name == "Archive"
    });
    hosted.commit_in_the_room("after the rename");
    assert_eq!(hosted.workspace().name, "Archive");
}

#[test]
#[serial_test::serial]
fn a_grant_given_on_the_hosting_device_reaches_the_rooms_policy() {
    let hosted = Hosted::start(0x57);
    let node = hosted.workspace().node();
    let grant = tilo_action_policy::Grant::new(
        "reader",
        tilo_action_policy::Subject::app("so.tilo.files"),
        node,
        tilo_action_policy::KindSet::ALL,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::person(hosted.ada.id()),
    );
    let id = hosted
        .device
        .tree
        .write()
        .grant_in_workspace(hosted.local, &hosted.ada.id(), grant);
    assert!(id.is_some(), "the owner may grant");
    until(
        &[&hosted.device],
        "the room's policy to hold the grant",
        || {
            hosted
                .device
                .policy(hosted.room)
                .is_some_and(|policy| {
                    policy
                        .grants
                        .iter()
                        .any(|grant| grant.id.as_str() == "reader")
                })
                .then_some(())
        },
    );
    hosted.commit_in_the_room("after the grant");
    assert!(
        hosted
            .workspace()
            .policy
            .grants
            .iter()
            .any(|grant| grant.id.as_str() == "reader"),
        "the grant is still in the workspace's policy"
    );
}
