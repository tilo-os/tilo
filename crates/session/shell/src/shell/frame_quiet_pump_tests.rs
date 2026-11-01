//! The model pump's launchpad settle and what a wake that changes nothing
//! costs it.

use super::*;
use automerge::ReadDoc as _;

/// A desktop whose one window is a peer's, in a workspace whose authority
/// has not arrived here yet.
fn awaiting_its_workspace() -> WorkspaceTree {
    use crate::shell::state::{
        Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId, next_id,
    };
    let mut workspace = Workspace::new(WorkspaceId::from(next_id()), "shared");
    let tab = next_id();
    let mut strip = Strip::new(StripId::from(next_id()), "Main");
    strip.window_groups.push(WindowGroup {
        id: next_id(),
        active_tab: tab,
        tabs: vec![Tab {
            id: tab,
            title: "Draft".into(),
            tile: TileContent::PeerWindow {
                app_id: "so.tilo.notes".into(),
                label: "Available on another device".into(),
                note: String::new(),
                missing: None,
            },
            app_title: None,
        }],
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    workspace.strips.push(strip);
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.desktop.workspaces = vec![workspace];
    tree
}

/// A peer's window that waits for its workspace's authority waits for the
/// room's news, and that news wakes the loop: the frame sets no deadline of
/// its own for it. A 250 ms poll for it woke an untouched desktop four times
/// a second, each a whole loop turn, for as long as the window waited.
#[test]
fn a_window_waiting_for_its_room_sets_no_wake_deadline() {
    let mut frame = FrameBuilder::with_tree(awaiting_its_workspace(), (1280, 800));
    for _ in 0..3 {
        frame.pump_room_contents();
    }
    assert!(
        crate::shell::peer_windows::any_pending(&frame.chrome().tree()),
        "the window still waits for its workspace"
    );
    assert_eq!(
        frame.peer_windows_poll(),
        None,
        "a window waiting for its room's news asks for no round of its own"
    );
}

/// A wake that changes nothing still runs the model pump. On the
/// populated desktop that pump must announce no tree write (every
/// generation-gated phase of the loop would walk the tree again on the
/// next wake) and must not probe the widget document for launchpad
/// requests it already found absent. A request and a real tree write
/// still land on the pump after them.
#[test]
fn a_quiet_pump_announces_no_tree_write_and_reads_no_launchpad_request() {
    let harvests = |phases: &[tilo_resource::profiling::Sample]| {
        phases
            .iter()
            .find(|sample| sample.phase == "shell.launchpad_harvest")
            .map_or(0, |sample| sample.calls)
    };
    // The top bar's minute would be a real change between two pumps.
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let mut frame = crate::shell::test_support::PopulatedShellFixture::new(
        (1280, 800),
        ui::theme::Theme::dark(),
    );
    frame.relayout();
    let _ = frame.paint_with_damage();
    // The demo's Notes window saves its startup record; the store's commit,
    // landing whenever its thread gets to it, would change the window from
    // Saving to Saved under one of the quiet pumps.
    let mut doc = AutoCommit::new();
    frame.settle(&mut doc);

    let tree = frame.chrome().tree_handle();
    let generation = tree.generation();
    let revision = frame.chrome_revision();
    let rested_at = std::time::Instant::now();
    let waking = frame.wake_deadline_owners();
    let due = frame
        .next_wake_deadline()
        .map(|due| due.saturating_duration_since(rested_at));
    let ((), phases) = tilo_resource::profiling::observe(|| {
        for pump in 0..20 {
            assert!(
                !frame.pump_apps(&mut doc, &automerge::ROOT),
                "nothing changed on a quiet desktop: pump {pump}, {:?} after it came to rest, \
                 which waited on {waking:?} in {due:?}",
                rested_at.elapsed()
            );
        }
    });
    assert_eq!(
        tree.generation(),
        generation,
        "a quiet pump announced a tree write"
    );
    assert_eq!(frame.chrome_revision(), revision);
    assert_eq!(
        harvests(&phases),
        0,
        "a quiet pump read the launchpad requests again: {phases:?}"
    );

    // The document moved: the next pump reads it and takes the request.
    doc.put(
        &automerge::ROOT,
        tilo_desktop::launchpad::EMPTY_WORKSPACE_OPEN_APP,
        true,
    )
    .expect("queued launcher request");
    let (changed, phases) =
        tilo_resource::profiling::observe(|| frame.pump_apps(&mut doc, &automerge::ROOT));
    assert!(changed, "the taken request repaints");
    assert!(harvests(&phases) > 0, "{phases:?}");
    assert!(frame.host.palette_visible(), "the launcher opened");

    // A write to the tree still announces itself.
    let before = tree.generation();
    frame.with_chrome(|chrome| {
        chrome.add_workspace("quiet pump");
    });
    drop(frame);
    assert_ne!(tree.generation(), before, "a real tree write is announced");
    drop(clock);
}

#[test]
fn frame_pump_settles_a_launchpad_request_once() {
    let mut frame = FrameBuilder::new((1280, 800));
    let mut doc = AutoCommit::new();
    doc.put(
        &automerge::ROOT,
        tilo_desktop::launchpad::EMPTY_WORKSPACE_OPEN_APP,
        true,
    )
    .expect("queued launcher request");

    assert!(
        frame.pump_apps(&mut doc, &automerge::ROOT),
        "a settled action requests repaint"
    );
    assert!(
        frame.host.palette_visible(),
        "the production frame pump opens the launcher"
    );
    assert!(
        matches!(
            doc.get(&automerge::ROOT, tilo_desktop::launchpad::EMPTY_WORKSPACE_OPEN_APP),
            Ok(Some((automerge::Value::Scalar(value), _)))
                if matches!(value.as_ref(), automerge::ScalarValue::Boolean(false))
        ),
        "typed settlement consumes the request"
    );
    doc.put(&automerge::ROOT, "pending-observer-witness", "uncommitted")
        .expect("pending fixture edit");
    assert!(doc.pending_ops() > 0);
    let revision = (doc.base_heads(), doc.pending_ops());
    frame.pump_apps(&mut doc, &automerge::ROOT);
    drop(frame);
    assert_eq!(
        revision,
        (doc.base_heads(), doc.pending_ops()),
        "the next pump does not repeat the request"
    );
}
