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
        "the original native read gate before authority fencing",
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
    let committed = format!("{BODY}// committed before the native authority is fenced\n");
    commit_edit(device, &owner, BODY, &committed);
    assert!(has_committed_body(&owner, &committed));
    tilo_test_env::within(
        "the original mounted reader to show the committed native edit",
        async {
            loop {
                target.frame();
                if target.has_body(&committed) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        },
    )
    .await;
    let refused = format!("{committed}// refused after the native authority is fenced\n");
    let request = prepare_edit(device, &owner, &committed, &refused);
    assert!(has_committed_body(&owner, &committed));
    owner.fence_all();
    assert_eq!(
        owner
            .commit_document(request, SystemTime::now())
            .unwrap_err(),
        "native owner continuity is fenced",
        "even a genuine signed edit cannot cross a fenced authority"
    );
    tilo_test_env::within(
        "the original reader to withdraw fenced authority health",
        async {
            while target.code.app.collaboration_is_connected() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        },
    )
    .await;
    assert_eq!(
        phase(target)["committed_read_ready"],
        false,
        "the fenced original source cannot retain read readiness"
    );
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
        hex::encode(Sha256::digest(committed.as_bytes())),
        "a fenced original capture keeps the edit committed before the fence and never admits \
         the refused one"
    );
}

pub(super) fn phase(target: &Mounted) -> serde_json::Value {
    let phases = target.code.app.controlled_document_merge_phase_for_test();
    assert_eq!(
        phases.len(),
        1,
        "the original document owner remains observable"
    );
    phases.into_iter().next().unwrap()
}

/// Commit an edit of the Code record from `base` to `expected` through the
/// production caller-signed entry, signed outside the owner turn exactly as
/// the SDK signs an app's write.
pub(super) fn commit_edit(
    device: &Device,
    owner: &NativeWorkspaceOwner,
    base: &str,
    expected: &str,
) {
    owner
        .commit_document(
            prepare_edit(device, owner, base, expected),
            SystemTime::now(),
        )
        .unwrap_or_else(|error| {
            panic!("native mounted edit requires an exact owner admission: {error}")
        });
}

fn prepare_edit(
    device: &Device,
    owner: &NativeWorkspaceOwner,
    base: &str,
    expected: &str,
) -> tilo_peer_state::owner_commit::OwnerCommitRequest {
    let room = RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    let changes = owner
        .with_current(room, |view| {
            let mut document =
                automerge::AutoCommit::load(&view.peer().document(APP, cold_code::RECORD).unwrap())
                    .unwrap();
            document.set_actor(automerge::ActorId::from(
                device.identity.device_cert.device_public_key.to_vec(),
            ));
            let before = document.get_heads();
            code_editor::actions::text_edit_action(FILE, base, expected)
                .apply(&mut document)
                .unwrap();
            Ok(document.save_after(&before))
        })
        .unwrap();
    let message = cold_code::unordered_document_message(
        room,
        changes,
        &device.identity.device_signing_key,
        SystemTime::now(),
    );
    tilo_joined_room::testing::request(&message).unwrap()
}
