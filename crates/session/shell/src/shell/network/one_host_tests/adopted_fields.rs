//! What a device writes to its own copy of a hosted workspace outlives the
//! room's next commit: every commit the device's owner admits projects the
//! room's document onto the workspace, so whatever was written only to the
//! local copy and never to the room would be gone after it. Each journey
//! writes one of the fields the projection replaces, then lets the room
//! commit a change of its own (an edit to a note) and looks at the field
//! again, and once more when the room's document carries the write.

use std::sync::Arc;

use collab_types::caps::{Access, CapOverride, Scope, Subject};
use tilo_user_identity::DeviceCapabilities;

use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::state::{GovernanceRequest, Workspace, WorkspaceId};

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

    /// Wait until the room's document as this device holds it shows what
    /// `carried` looks for, and nothing this device asked of the room's
    /// host waits any longer.
    fn until_the_room_carries(&self, what: &str, carried: impl Fn(&automerge::AutoCommit) -> bool) {
        until(&[&self.device], what, || {
            let tree = self.device.tree.read();
            (tree.room_outbox.is_empty() && tree.room_docs.get(&self.room).is_some_and(&carried))
                .then_some(())
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

#[test]
#[serial_test::serial]
fn an_effect_receipt_the_hosting_device_records_outlives_the_rooms_next_commit() {
    let hosted = Hosted::start(0x53);
    let invocation = tilo_actions::InvocationId::new();
    let recorded = hosted.device.tree.write().append_workspace_effect_receipt(
        hosted.local,
        invocation,
        &hosted.ada.id(),
        tilo_actions::ActionContext::default(),
        tilo_actions::EffectReceiptStatus::Succeeded,
    );
    assert!(recorded);
    hosted.commit_in_the_room("after the receipt");
    assert!(
        hosted
            .workspace()
            .effect_receipts
            .iter()
            .any(|receipt| receipt.invocation == invocation),
        "the recorded effect is still in the workspace's history"
    );
    hosted.until_the_room_carries("the room's history to hold the effect", |document| {
        tilo_desktop_model::projection::workspace_effect_receipts_of(document)
            .iter()
            .any(|receipt| receipt.invocation == invocation)
    });
    let held = hosted
        .workspace()
        .effect_receipts
        .iter()
        .filter(|receipt| receipt.invocation == invocation)
        .count();
    drop(hosted);
    assert_eq!(held, 1, "the room's history holds the effect once");
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
    let request = GovernanceRequest::GrantSandbox(capability);
    hosted
        .device
        .tree
        .write()
        .request_governance(hosted.room, request);
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
    hosted.until_the_room_carries("the room to take the capability back", |document| {
        !tilo_desktop_model::projection::room_sandbox_of(document)
            .iter()
            .any(|held| held.id == "document")
    });
    let granted = hosted
        .workspace()
        .sandbox
        .iter()
        .any(|held| held.id == "document");
    drop(hosted);
    assert!(!granted, "nor once the room took it back");
}
