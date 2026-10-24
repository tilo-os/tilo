//! What an app does through the door in a room its device hosts reaches the
//! room's other members: the edits a shortcut's run makes in the app's
//! record arrive on a member's device attributed to the run and undo as one
//! there, and a shortcut saved through the door is the room's.

use automerge::{AutoCommit, ReadDoc as _};
use serde_json::json;
use tilo_actions::remote::ReceiptStatus;

use super::{
    Door, FIXTURE, RECORD, assert_attributed_to, owner_principal, publish_draft, room_workspace,
};
use crate::shell::network::one_host_tests::{Device, until};

/// The app's record in the room as `device` holds it, once it holds it with
/// `ready` true of it.
fn record_on(
    door: &Door,
    device: &Device,
    what: &str,
    ready: impl Fn(&AutoCommit) -> bool,
) -> AutoCommit {
    let owner = device
        .net()
        .native_owner()
        .expect("the device's native owner");
    until(&[&door.hosted.device, device], what, || {
        let bytes = owner
            .with_current(door.room, |view| Ok(view.peer().document(FIXTURE, RECORD)))
            .ok()??;
        let document = AutoCommit::load(&bytes).expect("the record loads");
        ready(&document).then_some(document)
    })
}

#[test]
#[serial_test::serial]
fn a_runs_edits_in_a_room_its_device_hosts_reach_a_member_attributed_and_undo_as_one() {
    let mut door = Door::open();
    let bob = door.hosted.bob_joins(&door.dir.path().join("bob"));

    // The owner saves a shortcut through the door; Bob's device holds it in
    // the room.
    let shortcut = publish_draft("publish-draft", &["draft", "publish"]);
    match door.save(&shortcut).expect("the shortcut is saved") {
        ReceiptStatus::Completed { .. } => {}
        other => panic!("saving completes at once: {other:?}"),
    }
    until(
        &[&door.hosted.device, &bob],
        "the saved shortcut to reach Bob's device",
        || {
            room_workspace(&bob.tree.read(), door.room)
                .shortcuts
                .iter()
                .any(|saved| {
                    saved.id == "publish-draft"
                        && saved.saved_by.as_deref() == Some(owner_principal().as_str())
                })
                .then_some(())
        },
    );

    // Running it edits the app's record; the edits reach Bob's device.
    let resource = tilo_automation::shortcut_resource_id("publish-draft");
    let (run, _) = door
        .invoke(&resource, "run", &json!({ "title": "Launch notes" }))
        .expect("the run is admitted");
    match door.run_to_end(run) {
        ReceiptStatus::Completed { .. } => {}
        other => panic!("the run completes: {other:?}"),
    }
    let mut held = record_on(
        &door,
        &bob,
        "the run's edits to reach Bob's device",
        |record| {
            record
                .get(automerge::ROOT, "published")
                .ok()
                .flatten()
                .is_some()
        },
    );
    assert_eq!(
        held.get(automerge::ROOT, "draft")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok()),
        Some("Launch notes".to_owned()),
        "Bob's device holds the draft the run wrote"
    );
    assert_attributed_to(&mut held, run);

    // Undoing the run on Bob's copy reverts both of its steps there.
    let finished = door.finished_runs();
    drop(bob);
    drop(door);
    assert_eq!(finished.len(), 1, "one run finished");
    let mut workspace_doc = AutoCommit::new();
    crate::shell::automation_host::record_shortcut_run(
        &mut workspace_doc,
        &finished[0],
        &mut [(RECORD.to_string(), &mut held)],
    )
    .expect("the run is recorded");
    workspace_doc.commit();
    let runs = ui::read_runs(&workspace_doc);
    assert_eq!(runs.len(), 1, "exactly one run recorded");
    let report = ui::undo_run(
        &runs[0],
        None,
        &ui::AuthorDirectory::default(),
        &mut [(FIXTURE.to_string(), RECORD.to_string(), &mut held)],
    );
    assert!(report.is_complete(), "the whole run is undone: {report:?}");
    held.commit();
    assert!(held.get(automerge::ROOT, "draft").unwrap().is_none());
    assert!(held.get(automerge::ROOT, "published").unwrap().is_none());
}
