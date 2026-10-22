//! Chrome's peer-sync requests never hold a frame.
//!
//! Minting an invitation waits for a relay's answer, redeeming one opens the
//! native owner, finishing an enrollment publishes the state file. In a
//! session they run on the network's worker, and the frame pump applies
//! each outcome once it is back: to the invitation card, which says it is
//! working meanwhile, or as the answer of the action that asked.
use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::WorkspaceTree;
use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, Mutex},
};
use tilo_files::native::atomic::test_support::{
    Checkpoint, ProvisionCheckpoint, with_observer, with_provision_observer,
};
use ui::widget::Container;

fn host_with(tree: WorkspaceTree) -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
    let chrome = ChromeState::new(tree);
    let host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(chrome.clone());
    (host, chrome)
}

/// A runner like the session network's: one worker thread, jobs in order.
fn worker_thread() -> PeerSyncRunner {
    let (jobs, queue) = std::sync::mpsc::channel::<PeerSyncJob>();
    std::thread::spawn(move || {
        for job in queue {
            job();
        }
    });
    Rc::new(move |job| {
        jobs.send(job)
            .map_err(|std::sync::mpsc::SendError(job)| job)
    })
}

/// Pump `host` until `done`, as the loop does after the worker woke it.
fn pump_until(host: &mut ShellOverlayHost, what: &str, done: impl Fn(&ShellOverlayHost) -> bool) {
    tilo_test_env::wait_until(what, || {
        host.pump_peer_sync();
        done(host)
    });
}

/// A relay that answers only once the test releases it.
fn held_relay() -> (
    std::sync::mpsc::Sender<()>,
    impl Fn() + Send + Sync + 'static,
) {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = Mutex::new(released);
    let wait = move || {
        let _ = released
            .lock()
            .expect("the relay")
            .recv_timeout(tilo_test_env::hang_bound());
    };
    (release, wait)
}

#[test]
fn a_slow_invitation_never_holds_a_frame_and_lands_on_the_card() {
    let (mut host, chrome) = host_with(WorkspaceTree::bootstrap_demo());
    let active = chrome.borrow().tree().desktop.active;
    let loop_thread = std::thread::current().id();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let (release, relay) = held_relay();
    host.set_peer_sync_worker(PeerSyncWorker {
        site_rooms: Arc::new(Vec::new),
        site_joins: Arc::new(Vec::new),
        arrivals: Arc::new(Vec::new),
        arrive: Arc::new(|_| {}),
        call: Arc::new(move |workspace, request| {
            recorded.lock().expect("the call log").push((
                std::thread::current().id(),
                workspace,
                request.clone(),
            ));
            match request {
                PeerSyncRequest::IssueInviteFor { .. } => {
                    relay();
                    Ok(PeerSyncResponse::Message("tok".into()))
                }
                PeerSyncRequest::ListPendingInvitations => {
                    Ok(PeerSyncResponse::PendingInvitations(Vec::new()))
                }
                _ => Err("not here".into()),
            }
        }),
        run: worker_thread(),
    });
    let invite = PeerSyncRequest::IssueInviteFor {
        name: "Bob".into(),
        role: crate::network::invite::InviteRole::Editor,
        recipient_user_public_key: [7; 32],
    };

    // The relay is held until the end: a press or pump that waited on it
    // would not return before then.
    host.run_peer_sync_request(&invite, false);
    for _ in 0..4 {
        host.pump_peer_sync();
    }
    // A second press while the first invitation is still out.
    host.run_peer_sync_request(&invite, false);

    let card = host.peer_sync_snapshot();
    assert_eq!(
        card.status,
        Some(crate::shell::i18n::t("peer-sync-working")),
        "the card does not say it is working while the invitation is minted"
    );
    assert!(
        card.invite_token.is_none(),
        "a token appeared before it was minted"
    );

    release.send(()).expect("the relay is waiting");
    pump_until(&mut host, "the minted invitation", |host| {
        host.peer_sync_snapshot().invite_token.is_some()
    });
    assert_eq!(
        host.peer_sync_snapshot().invite_token.as_deref(),
        Some("tok")
    );
    let calls = calls.lock().expect("the call log").clone();
    let minted = calls
        .iter()
        .filter(|(_, _, request)| matches!(request, PeerSyncRequest::IssueInviteFor { .. }))
        .count();
    assert_eq!(
        minted, 1,
        "a second press while the first was out minted twice"
    );
    assert!(
        calls.iter().all(|(thread, _, _)| *thread != loop_thread),
        "a peer-sync request ran on the frame loop"
    );
    assert!(
        calls.iter().all(|(_, workspace, _)| *workspace == active),
        "a request ran for another workspace than the one it was made in"
    );
}

#[test]
fn an_invitation_action_answers_once_its_request_is_back() {
    let (mut host, chrome) = host_with(WorkspaceTree::bootstrap_demo());
    let workspace = chrome.borrow().tree().desktop.active;
    let (answer, relay) = held_relay();
    host.set_peer_sync_worker(PeerSyncWorker {
        site_rooms: Arc::new(Vec::new),
        site_joins: Arc::new(Vec::new),
        arrivals: Arc::new(Vec::new),
        arrive: Arc::new(|_| {}),
        call: Arc::new(move |_, request| match request {
            PeerSyncRequest::IssueInvitation(_) => {
                // Stands for the relay: it answers when the test says so.
                relay();
                Ok(PeerSyncResponse::Message("tok".into()))
            }
            PeerSyncRequest::ListPendingInvitations => {
                Ok(PeerSyncResponse::PendingInvitations(Vec::new()))
            }
            _ => Err("not here".into()),
        }),
        run: worker_thread(),
    });
    let request_id = chrome
        .borrow_mut()
        .begin_peer_sync_request(PeerSyncAction::Network {
            workspace,
            request: PeerSyncRequest::IssueInvitation(InvitationTerms {
                role: crate::network::invite::InviteRole::Viewer,
                expires_in_seconds: 600,
                invitee_name: None,
                recipient_user_public_key: None,
            }),
        });

    // The relay is held: a pump that waited on it would not return.
    host.pump_peer_sync();
    assert!(
        chrome
            .borrow_mut()
            .take_peer_sync_request_result(request_id)
            .is_none(),
        "the action was answered before its invitation was minted"
    );

    answer.send(()).expect("the relay is waiting");
    let result = tilo_test_env::wait_for("the action's answer", || {
        host.pump_peer_sync();
        chrome
            .borrow_mut()
            .take_peer_sync_request_result(request_id)
    });
    assert_eq!(result, Ok(serde_json::json!({ "invite": "tok" })));
}

#[test]
fn redeeming_an_invitation_opens_the_owner_on_the_worker_not_the_loop() {
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.persist_path = Some(directory.path().join("state.json"));
    let (mut host, chrome) = host_with(tree);
    let tree = chrome.borrow().tree_handle();
    let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
    let network = Arc::new(network);
    network.start_peer_sync_worker();
    host.set_peer_sync_worker(PeerSyncWorker::for_network(&network));
    // Another open of the state file's writer is in progress: redeeming
    // opens the owner, which waits behind it.
    let (release, holder) = crate::shell::native_workspace_owner::test_open_in_progress(
        &tree,
        tilo_test_env::hang_bound(),
    );

    // An invitation that verifies: text that does not is refused before
    // any workspace is made for it, without the owner.
    let invitation = tilo_peer_state::join::encode_invitation_text(
        tilo_joined_room::testing::raw_fixture()
            .0
            .verified_join()
            .signed_invitation(),
    );

    let syncs = Rc::new(Cell::new(0_usize));
    let directories = Rc::clone(&syncs);
    let data = Rc::clone(&syncs);
    // The other open is held until after these pumps: one that waited on
    // the owner would not return before it is released.
    with_provision_observer(
        move |event| {
            if event.checkpoint == ProvisionCheckpoint::BeforeSync {
                directories.set(directories.get().saturating_add(1));
            }
        },
        || {
            with_observer(
                move |event| {
                    if event.checkpoint == Checkpoint::BeforeDataFlush {
                        data.set(data.get().saturating_add(1));
                    }
                },
                || {
                    host.run_peer_sync_request(&PeerSyncRequest::AcceptInvite(invitation), false);
                    for _ in 0..8 {
                        host.pump_peer_sync();
                    }
                },
            );
        },
    );

    assert_eq!(
        syncs.get(),
        0,
        "redeeming an invitation synced the state file on the frame loop"
    );
    assert_eq!(
        host.peer_sync_snapshot().status,
        Some(crate::shell::i18n::t("peer-sync-working")),
        "the redemption finished before the owner it needs had opened"
    );

    drop(release);
    holder.join().expect("the other open");
    pump_until(&mut host, "the redemption's answer", |host| {
        host.peer_sync_snapshot().status != Some(crate::shell::i18n::t("peer-sync-working"))
    });
    assert!(
        network.opened_native_owner().is_some(),
        "the redemption did not open the owner on the worker"
    );
    drop(network);
    // This device holds no custody to redeem with, which the worker found
    // only once the owner had opened.
    assert_eq!(
        host.peer_sync_snapshot().status,
        Some(crate::shell::i18n::t_args(
            "peer-sync-join-refused",
            &[(
                "error",
                String::from("native device custody unavailable").into()
            )],
        )),
        "the card does not carry the redemption's refusal"
    );
}
