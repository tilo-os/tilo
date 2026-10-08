//! A terminal original committed reader must withdraw collaboration health.
use super::*;
use tilo_actions::Action as _;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mounted_code_withdraws_health_after_terminal_committed_read() {
    trace_publication_hops();
    Box::pin(super::super::actual_exchange(
        false,
        true,
        Some(Journey::BlockedRead),
    ))
    .await;
}

#[expect(
    clippy::future_not_send,
    reason = "the original mounted UI uses Rc on its caller thread"
)]
pub(super) async fn run(device: &Device, target: &mut Mounted) {
    tilo_test_env::within(
        "the original native read gate before writer contention",
        async {
            loop {
                target.frame();
                if phase(target)["committed_read_ready"] == true {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        },
    )
    .await;
    let owner = target.network.native_owner().unwrap();
    let tree = crate::shell::native_workspace_owner::test_tree_of(&owner);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let _guard = tree.write();
        held_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2600));
    });
    tilo_test_env::recv("actual native workspace writer acquisition", &held_rx);
    // Do not tick ShellNetwork while its actual workspace writer is held.
    tokio::time::sleep(std::time::Duration::from_millis(2400)).await;
    let observed = phase(target);
    eprintln!(
        "controlled blocked read: ready={} connected={}",
        observed["committed_read_ready"],
        target.code.app.collaboration_is_connected()
    );
    writer.join().unwrap();
    assert_eq!(
        observed["committed_read_ready"], false,
        "the real stalled RPC must withdraw committed read readiness"
    );
    let expected = format!("{BODY}// committed after the workspace writer releases\n");
    commit_edit(device, &owner, &expected);
    assert!(has_committed_body(&owner, &expected));
    target.frame();
    assert!(
        !target.code.app.collaboration_is_connected(),
        "a terminal committed reader must not advertise a healthy collaboration route"
    );
    assert!(
        target
            .code
            .app
            .controlled_document_merge_phase_for_test()
            .is_empty(),
        "the model must retire the terminal route's original document-owner bindings"
    );
    let snapshot = target
        .code
        .app
        .controlled_native_input_snapshot_for_test()
        .unwrap();
    assert_eq!(
        snapshot["body_hash"],
        hex::encode(Sha256::digest(BODY.as_bytes())),
        "an uncertain expired capture cannot admit a later native edit"
    );
}

fn phase(target: &Mounted) -> serde_json::Value {
    let phases = target.code.app.controlled_document_merge_phase_for_test();
    assert_eq!(
        phases.len(),
        1,
        "the original document owner remains observable"
    );
    phases.into_iter().next().unwrap()
}

fn commit_edit(device: &Device, owner: &NativeWorkspaceOwner, expected: &str) {
    use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
    let room = RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    let now = SystemTime::now();
    let message = owner
        .with_current(room, |view| {
            let mut document =
                automerge::AutoCommit::load(&view.peer().document(APP, cold_code::RECORD).unwrap())
                    .unwrap();
            document.set_actor(automerge::ActorId::from(
                device.identity.device_cert.device_public_key.to_vec(),
            ));
            let before = document.get_heads();
            code_editor::actions::text_edit_action(FILE.into(), BODY, expected)
                .apply(&mut document)
                .unwrap();
            view.peer().clone().publish_incremental(
                APP,
                cold_code::RECORD,
                document.save_after(&before),
                &device.identity.device_signing_key,
                now,
            )
        })
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("native edit must be signed app data")
    };
    owner
        .commit_document(
            OwnerCommitRequest {
                room: room.to_string(),
                operation: CommitOperation::Document {
                    app: APP.into(),
                    record: cold_code::RECORD.into(),
                    envelope,
                },
            },
            now,
        )
        .unwrap();
}
