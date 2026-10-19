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
    time::{Duration, Instant},
};
use tilo_files::native::atomic::test_support::{
    Checkpoint, ProvisionCheckpoint, with_observer, with_provision_observer,
};
use ui::widget::Container;

/// One frame at 60 Hz: what one pump of the loop may take.
const FRAME: Duration = Duration::from_millis(16);

/// A relay that takes its time to answer.
const SLOW_RELAY: Duration = Duration::from_millis(250);

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
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("deadline");
    while !done(host) {
        assert!(Instant::now() < deadline, "{what} never came back");
        host.pump_peer_sync();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn timed<T>(call: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = call();
    (value, started.elapsed())
}

#[test]
fn a_slow_invitation_never_holds_a_frame_and_lands_on_the_card() {
    let (mut host, chrome) = host_with(WorkspaceTree::bootstrap_demo());
    let active = chrome.borrow().tree().desktop.active;
    let loop_thread = std::thread::current().id();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
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
                    std::thread::sleep(SLOW_RELAY);
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

    let ((), pressed) = timed(|| host.run_peer_sync_request(&invite, false));
    let slowest_pump = (0..4)
        .map(|_| timed(|| host.pump_peer_sync()).1)
        .max()
        .unwrap_or_default();
    // A second press while the first invitation is still out.
    host.run_peer_sync_request(&invite, false);

    assert!(
        pressed <= FRAME && slowest_pump <= FRAME,
        "minting an invitation held the frame loop: press {pressed:?}, pump {slowest_pump:?}, over one frame ({FRAME:?})"
    );
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
    let (answer, answered) = std::sync::mpsc::channel::<()>();
    let answered = Mutex::new(answered);
    host.set_peer_sync_worker(PeerSyncWorker {
        site_rooms: Arc::new(Vec::new),
        site_joins: Arc::new(Vec::new),
        arrivals: Arc::new(Vec::new),
        arrive: Arc::new(|_| {}),
        call: Arc::new(move |_, request| match request {
            PeerSyncRequest::IssueInvitation(_) => {
                // Stands for the relay: it answers when the test says so.
                let _ = answered
                    .lock()
                    .expect("the relay")
                    .recv_timeout(Duration::from_secs(30));
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

    let ((), took) = timed(|| {
        host.pump_peer_sync();
    });
    assert!(
        took <= FRAME,
        "an invitation action held the frame loop for {took:?}, over one frame ({FRAME:?})"
    );
    assert!(
        chrome
            .borrow_mut()
            .take_peer_sync_request_result(request_id)
            .is_none(),
        "the action was answered before its invitation was minted"
    );

    answer.send(()).expect("the relay is waiting");
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("deadline");
    let result = loop {
        host.pump_peer_sync();
        if let Some(result) = chrome
            .borrow_mut()
            .take_peer_sync_request_result(request_id)
        {
            break result;
        }
        assert!(Instant::now() < deadline, "the action was never answered");
        std::thread::sleep(Duration::from_millis(5));
    };
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
    let (release, holder) =
        crate::shell::native_workspace_owner::test_open_in_progress(&tree, Duration::from_secs(30));

    let syncs = Rc::new(Cell::new(0_usize));
    let directories = Rc::clone(&syncs);
    let data = Rc::clone(&syncs);
    let slowest = with_provision_observer(
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
                    let press = timed(|| {
                        host.run_peer_sync_request(
                            &PeerSyncRequest::AcceptInvite("not an invitation".into()),
                            false,
                        );
                    })
                    .1;
                    (0..8)
                        .map(|_| timed(|| host.pump_peer_sync()).1)
                        .fold(press, Duration::max)
                },
            )
        },
    );

    assert_eq!(
        syncs.get(),
        0,
        "redeeming an invitation synced the state file on the frame loop"
    );
    assert!(
        slowest <= FRAME,
        "redeeming an invitation held the frame loop for {slowest:?} while the owner was opening, over one frame ({FRAME:?})"
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
