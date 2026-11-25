//! A server stopped hard (`podman rm -f`) right after it told a member it kept a
//! write. Its answer names the server's own durable copy, not the workspace's
//! copy of the record, which it writes through within a second: the next start
//! replays the server's copy into the workspace's, so the write is in the
//! record's history all the same.
use tilo_peer_state::direct::Kept;

use super::*;

/// Copy what `from` holds into `to`, as a hard stop leaves it on disk.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            // A file removed since it was listed is not part of this state.
            let _ = std::fs::copy(entry.path(), &target);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_the_server_committed_survives_a_hard_stop_before_the_governor_keeps_it() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let workspace = create_and_enter(&server, &laptop, "Lounge").await;
    let bob_link =
        super::sealed_documents::joined(&server, &laptop, &workspace, &bob, &phone, [7; 32])
            .await
            .unwrap();
    let alice_link = Linked::open(&server, &laptop, workspace.workspace_id)
        .await
        .expect("the workspace admits Alice's laptop");
    crate::common::receives(&alice_link, |peer| {
        role(peer, phone.device_pubkey()) == Some(collab_types::Role::Editor)
    })
    .await;
    crate::common::wait_until_subscribed(&server, workspace.workspace_id, &phone.device_pubkey())
        .await;

    // Alice's first write makes the record, and the host holds it.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "welcome").unwrap();
    doc.commit_edit("fixture", "topic", &laptop.cert.user_public_key);
    alice_link
        .publish(|peer| {
            peer.publication_mut().publish_incremental(
                APP,
                "",
                doc.save_after(&[]),
                &laptop.sk,
                SystemTime::now(),
            )
        })
        .expect("Alice's write goes out on her link");
    crate::common::receives(&bob_link, |peer| {
        text(peer, "topic").as_deref() == Some("welcome")
    })
    .await;

    // Bob's page writes and the server answers that it keeps the write; the server is
    // stopped hard at that moment, before the workspace's copy is kept.
    let mut answers = bob_link.answers();
    bob_link
        .publish(|peer| {
            Ok(write(
                peer,
                "reply",
                "committed",
                &phone.sk,
                &phone.cert.user_public_key,
            ))
        })
        .expect("Bob's write goes out on his link");
    tilo_test_env::within("the server keeps Bob's write", async {
        loop {
            match answers.recv().await {
                Ok(answered) => {
                    if answered.kept == Kept::Durable {
                        return;
                    }
                }
                Err(error) => panic!("the link ended before the server kept Bob's write: {error}"),
            }
        }
    })
    .await;
    let stopped = tempfile::tempdir().unwrap();
    copy_dir(state.path(), stopped.path());

    // The next start holds Bob's write in the workspace's copy of the record.
    let reopened = start(stopped.path(), &alice, &host).await;
    let workspaces = reopened.workspaces().unwrap();
    tilo_test_env::within("the reopened host holds Bob's kept write", async {
        loop {
            let held = workspaces
                .resource_document(&workspace.workspace, APP, "")
                .await
                .unwrap()
                .and_then(|bytes| AutoCommit::load(&bytes).ok())
                .and_then(|doc| {
                    let (value, _) = doc.get(ROOT, "reply").ok().flatten()?;
                    value.into_string().ok()
                });
            if held.as_deref() == Some("committed") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    reopened.shutdown().await;
    drop(bob_link);
    drop(alice_link);
    server.shutdown().await;
}
