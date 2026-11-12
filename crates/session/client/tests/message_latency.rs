//! How long one person's change takes to reach the other person in the same
//! workspace: from the moment the sender's page publishes it to the moment the
//! other page's state holds it. A message in a conversation, and a window one
//! person opens on the workspace's shared desk. Two pages reach one host over
//! its live plane on loopback, as two browsers in a workspace do; nothing of
//! either page's frames is in it. This is the carrier, the host, and the state
//! each page holds.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD, SharedDesktop};
use tilo_desktop_model::{StripId, WindowWidth, Workspace, WorkspaceSlot};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_client::{Cmd, Link, Outbox};

use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{enrol, ready, route, start_host_admitting, start_peer};

/// How long one step of this test may take before it is a failure rather than a
/// slow machine: far above what a page and a host cost on loopback, and far
/// below the wait a polled carrier would add.
const STEP_BOUND: Duration = Duration::from_millis(5_000);

/// How long a change may take to reach the other person. A loaded runner pays
/// for a reconnect or a replay; a page waits for no interval to learn of one.
const BUDGET: Duration = Duration::from_millis(500);

const MESSAGES: usize = 20;

/// Two people's pages in one workspace, each a session of a device of its own:
/// Ada's host serves it, and Bob joins it through its door as the second person
/// in it does.
struct Pages {
    a: Link,
    b: Link,
    _laptop: crate::participant::Participant,
    host: crate::support::TestHost,
    _state: tempfile::TempDir,
    _host_device: crate::support::Device,
    _ada: crate::support::Device,
    _bob: crate::support::Device,
    /// Ada's person, who writes the windows her page opens.
    ada_person: String,
}

impl Pages {
    async fn start() -> Self {
        let (ada_root, _) = UserIdentity::generate("Ada", 1);
        let ada_person = hex::encode(ada_root.verifying_key().to_bytes());
        let (bob_root, _) = UserIdentity::generate("Bob", 2);
        let bob_person = bob_root.verifying_key().to_bytes();
        let host_device = enrol(&ada_root, "home-server", DeviceCapabilities::host());
        let ada = enrol(&ada_root, "ada-laptop", DeviceCapabilities::member());
        let bob = enrol(&bob_root, "bob-laptop", DeviceCapabilities::member());
        let state = tempfile::tempdir().unwrap();
        // Bob is someone else: its operator trust policy admits him, and the
        // workspace's governance admits him only through its own door.
        let host = start_host_admitting(
            &ada_root,
            &host_device,
            state.path(),
            "127.0.0.1:0".parse().unwrap(),
            None,
            &[bob_person],
        )
        .await;
        // Ada's laptop takes part in the workspace's channels, as a member's own
        // device does: it creates their key scope and admits the devices that ask.
        let laptop = crate::participant::Participant::start(&host.relay, &ada_root).await;
        // Ada's page is a device of hers in her own workspace.
        let a = start_peer(
            host.iroh(),
            ada.cert.clone(),
            host_device.cert.clone(),
            ada.key.clone(),
        );
        a.join_placed(crate::support::WORKSPACE, "Workspace")
            .unwrap();
        // Bob's page enters the same workspace through its door, on the
        // invitation its owner issued for him.
        let granted = crate::support::door(
            &host,
            &ada_root,
            &host_device,
            crate::support::WORKSPACE,
            bob_person,
        )
        .await;
        let b = crate::support::enter(
            &host,
            granted,
            &bob.cert,
            &bob.key,
            &host_device.cert,
            crate::support::workspace(),
        )
        .await;
        ready(&[&a, &b]).await;
        for peer in [&a, &b] {
            crate::support::in_workspace(peer, crate::support::workspace()).await;
        }
        Self {
            a,
            b,
            _laptop: laptop,
            host,
            _state: state,
            _host_device: host_device,
            _ada: ada,
            _bob: bob,
            ada_person,
        }
    }
}

/// Wait until `probe` finds what it reads, and answer how long that took since
/// `from`; a step that runs past [`STEP_BOUND`] fails, naming what it waited for.
async fn arrival<T>(from: Instant, what: &str, mut probe: impl FnMut() -> Option<T>) -> Duration {
    loop {
        if probe().is_some() {
            return from.elapsed();
        }
        assert!(
            from.elapsed() < STEP_BOUND,
            "{what} did not arrive within {STEP_BOUND:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_reaches_the_other_page_without_waiting_for_a_poll() {
    let pages = Pages::start().await;
    let (a, b) = (&pages.a, &pages.b);

    let mut took = Vec::new();
    let mut cold = Duration::ZERO;
    let mut doc = AutoCommit::new();
    for index in 0..=MESSAGES {
        let text = format!("message-{index}");
        let message = crate::support::say(&mut doc, &text);
        let sent = Instant::now();
        a.write(route(), &message).unwrap();
        let heard = arrival(sent, &format!("the message {text:?}"), || {
            crate::support::said(b, &route())
                .contains(&text)
                .then_some(())
        })
        .await;
        // The first one founds the record and opens its channel; the rest find
        // the receiver already holding it.
        if index == 0 {
            cold = heard;
        } else {
            took.push(heard);
        }
        // A pause, as between messages a person types, so each one finds
        // the receiver waiting rather than mid-answer.
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    took.sort();
    let p50 = took[took.len() / 2];
    let p95 = took[took.len() * 95 / 100];
    eprintln!("page to page: p50 {p50:?}, p95 {p95:?}, first {cold:?}, all {took:?}");
    // On loopback this is a few milliseconds of sealing and a round trip
    // through the host. A receiver that only learned of it on its next poll
    // would take that poll's interval; the bound is far above the real cost
    // and far below any interval, so a loaded runner does not trip it.
    assert!(
        p50 < Duration::from_millis(150),
        "a message took {p50:?} (p50) from page to page: {took:?}"
    );
    for peer in [a, b] {
        peer.close();
    }
    pages.host.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_window_opened_on_the_shared_desk_reaches_the_other_person() {
    let pages = Pages::start().await;
    let (a, b) = (&pages.a, &pages.b);
    // Both people show the workspace, so both hold its desk's record and the
    // keys it is sealed under: what a page showing a workspace does as it opens.
    for peer in [a, b] {
        peer.send(Cmd::OpenRoute(desk())).unwrap();
        crate::support::wait_keyed(peer, &desk()).await;
    }
    let before = desk_of(b);
    assert!(
        before.tree.workspaces.iter().all(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .all(|group| group.tabs.is_empty())
        }),
        "Bob's desk shows the workspace as it is: no window of Ada's on it yet"
    );

    // Ada opens one window after another on the workspace's shared desk: what
    // her page publishes when a person opens an app there.
    let mut took = Vec::new();
    for (index, (window, tab)) in WINDOWS.into_iter().enumerate() {
        let label = format!("Notes {}", index + 1);
        let opened = Instant::now();
        a.write(
            desk(),
            &window_opened(a, &pages.ada_person, "so.tilo.notes", &label, (window, tab)),
        )
        .unwrap();
        took.push(
            arrival(opened, &format!("Ada's {label:?} on Bob's desk"), || {
                shows(&desk_of(b), tab).then_some(())
            })
            .await,
        );
        // The next window is opened on the desk as the workspace holds it here,
        // so her own page holds this one before she opens the next.
        let echoed = Instant::now();
        arrival(echoed, &format!("Ada's own page to hold {label:?}"), || {
            shows(&desk_of(a), tab).then_some(())
        })
        .await;
    }
    took.sort();
    let p50 = took[took.len() / 2];
    eprintln!("a window on the shared desk reaches the other person: {took:?}, p50 {p50:?}");
    assert!(
        p50 < BUDGET,
        "a window took {p50:?} (p50) to reach the other person's desk: {took:?}"
    );
    for peer in [a, b] {
        peer.close();
    }
    pages.host.relay.shutdown().await;
}

/// The route of the workspace's shared desktop.
fn desk() -> DocumentRoute {
    DocumentRoute {
        app_id: DESKTOP_APP.into(),
        cluster_id: crate::support::WORKSPACE.into(),
        record: DESKTOP_RECORD.into(),
    }
}

/// The desk `peer` holds of the workspace as its model reads it, when it holds
/// the record at all.
fn desk_of(peer: &Link) -> SharedDesktop {
    crate::support::record_of(peer, &desk())
        .and_then(|doc| tilo_desktop_model::shared_record::read(&doc, None))
        .unwrap_or_default()
}

/// Whether `desk` shows the window `tab`.
fn shows(desk: &SharedDesktop, tab: u64) -> bool {
    desk.tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .any(|held| held.id == tab)
}

/// The strip the windows this test opens stand in, and each window and its tab:
/// a desk of one workspace has one strip, and every window in it is its own.
const STRIP: StripId = StripId(1);
const WINDOWS: [(u64, u64); 5] = [(100, 2), (102, 4), (104, 6), (106, 8), (108, 10)];

/// The change Ada's page publishes when she opens `app`, under `label`, on the
/// workspace's shared desk as the window `(window, tab)`, in the shape a
/// workspace's record holds: the desk as this device holds it with her window in
/// it, written by her.
fn window_opened(
    peer: &Link,
    person: &str,
    app: &str,
    label: &str,
    (window, tab): (u64, u64),
) -> DocumentMutation {
    let mut doc = crate::support::record_of(peer, &desk()).unwrap_or_default();
    doc.set_actor(automerge::ActorId::from(person.as_bytes().to_vec()));
    let heads = doc.get_heads();
    let mut desk = tilo_desktop_model::shared_record::read(&doc, None).unwrap_or_default();
    let identity = tilo_desktop_model::WorkspaceId::parse(crate::support::WORKSPACE).unwrap();
    let slot = WorkspaceSlot::of_workspace(identity);
    if !desk.tree.workspaces.iter().any(|held| held.slot == slot) {
        desk.tree.workspaces.push(Workspace::in_workspace(
            slot,
            identity,
            "Workspace".to_owned(),
        ));
    }
    let workspace = desk
        .tree
        .workspaces
        .iter_mut()
        .find(|held| held.slot == slot)
        .expect("the workspace the window opens in");
    if workspace.strips.is_empty() {
        workspace.strips.push(Strip::new(STRIP, "Workspace"));
    }
    let strip = workspace
        .strips
        .first_mut()
        .expect("the strip the window opens in");
    strip.window_groups.push(WindowGroup {
        id: window,
        tabs: vec![Tab {
            id: tab,
            title: label.to_owned(),
            tile: TileContent::LocalApp {
                app_id: app.to_owned(),
                label: label.to_owned(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    desk.authors.insert(tab, person.to_owned());
    tilo_desktop_model::shared_record::write(&mut doc, &desk).expect("the desk is writable");
    tilo_desktop_model::shared_record::commit_change(
        &mut doc,
        person,
        tilo_clock::unix_now_ms_i64(),
        None,
    );
    if heads.is_empty() {
        DocumentMutation::Snapshot(doc.save())
    } else {
        DocumentMutation::Incremental(doc.save_after(&heads))
    }
}
