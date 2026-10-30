//! Saved local-app cards and the frame writeback of open records.

use crate::shell::state::FreshId as _;
use crate::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent as StateTile, WindowGroup as StateGroup,
    WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
};
use automerge::{AutoCommit, ReadDoc as _, transaction::Transactable as _};
use std::collections::HashMap as StdMap;
use ui::record::{RecordId, RecordStore};
use ui::widget::Widget;

const TAB_A: u64 = 11;
const TAB_B: u64 = 22;
/// Where these tests write into a record, standing in for what an app
/// keeps there.
const KEY: &str = "written";

fn write(store: &RecordStore, tab: u64, text: &str) {
    store
        .with(&RecordId::for_tab(tab), |doc| {
            doc.put(automerge::ROOT, KEY, text).unwrap();
        })
        .expect("the tab's record is open");
}

fn written(store: &RecordStore, tab: u64) -> Option<String> {
    store
        .read(&RecordId::for_tab(tab), |doc| {
            doc.get(automerge::ROOT, KEY)
                .ok()
                .flatten()
                .and_then(|(value, _)| value.into_string().ok())
        })
        .flatten()
}

fn record(tab: u64) -> ui::record::RecordCheckout {
    ui::record::RecordCheckout::new(RecordId::for_tab(tab), ui::record::RecordKind::new("test"))
}

/// The store's writers are debounced and off-thread, so something has
/// to offer them the documents each frame. Read back from a *different*
/// store, so it's the file on disk being checked, not the live copy.
#[test]
fn a_frame_offers_every_open_record_to_its_writer() {
    const PACKAGE: &str = "so.tilo.test-frame-writeback";

    let tmp = tempfile::tempdir().expect("temp dir");

    let store = store_at(tmp.path(), PACKAGE);
    store.create_record(&record(TAB_A)).unwrap();
    store.create_record(&record(TAB_B)).unwrap();
    let (mut host, _chrome) = crate::shell::build_tilo_overlay_host_with_records(
        tree_with_two_document_tabs(),
        false,
        store.clone(),
    );
    // What a tab's app writes when the user types into it.
    write(&store, TAB_A, "written by a frame");

    let shell_doc = AutoCommit::new();
    host.update(&shell_doc, &automerge::ROOT);
    // Nothing but the frame offers the edit to the debounced writer, and
    // awaiting the writer submits nothing, so the record reaches
    // `Saved` only if the frame did.
    let tab_a = RecordId::for_tab(TAB_A);
    store
        .await_submitted(&tab_a)
        .unwrap_or_else(|error| panic!("the tab's writer failed: {error}"));
    assert_eq!(
        store.save_status(&tab_a).unwrap(),
        ui::record::RecordSaveStatus::Saved,
        "a frame must offer each open tab's document to its writer"
    );

    let reader = store_at(tmp.path(), PACKAGE);
    reader.open_record(&record(TAB_A));
    assert_eq!(
        written(&reader, TAB_A).as_deref(),
        Some("written by a frame"),
        "a frame must offer each open tab's document to its writer",
    );
}

#[test]
fn saved_local_app_status_survives_production_frame_binding() {
    fn descriptions(widget: &dyn Widget, out: &mut Vec<(String, String)>) {
        if let Some(node) = widget.accessibility() {
            out.push((
                node.name.unwrap_or_default(),
                node.description.unwrap_or_default(),
            ));
        }
        widget.for_each_child(&mut |child| descriptions(child, out));
    }
    for (phase, detail) in [
        // The window's place while it arrives: the app's name, busy, and
        // heard as loading.
        (
            "opening",
            ui::i18n::I18nManager::lookup_plain("arriving-loading"),
        ),
        ("failed", crate::shell::i18n::t("launch-failed-stopped")),
        (
            "timeout",
            crate::shell::i18n::t("host-app-did-not-open-body"),
        ),
        (
            "interrupted",
            crate::shell::i18n::t("shell-window-restore-interrupted"),
        ),
    ] {
        let tree = tree_with_two_document_tabs();
        {
            let mut state = tree.write();
            let group = &mut state.desktop.workspaces[0].strips[0].window_groups[0];
            group.tabs.truncate(1);
            group.tabs[0].title = "Browser".into();
            group.tabs[0].tile = StateTile::LocalApp {
                app_id: "so.tilo.browser".into(),
                label: "Browser".into(),
                note: if phase == "interrupted" || phase == "opening" {
                    crate::shell::peer_windows::OPENING.into()
                } else {
                    detail.clone()
                },
            };
            drop(state);
        }
        if phase == "opening" {
            crate::shell::launcher::set_pending_placement(
                "so.tilo.browser",
                crate::shell::tab_binding::Placement::IntoTab(TAB_A),
            );
        }
        let store = RecordStore::in_memory();
        let mut frame = crate::shell::frame::FrameBuilder::with_handle_and_records(
            tree.clone(),
            false,
            (1000, 700),
            store.clone(),
        );
        let mut parent = AutoCommit::new();
        for _ in 0..2 {
            frame.pump_apps(&mut parent, &automerge::ROOT);
            frame.relayout();
        }
        let scene = frame.paint();
        if let Some(directory) = std::env::var_os("TILO_LOCALAPP_NOTICE_EVIDENCE") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            ui::offscreen::OffscreenRenderer::new(1000, 700).render_png(
                &scene,
                frame.theme().tokens.colors.surfaces.base,
                directory.join(format!("{phase}.png")),
            );
        }
        let mut spoken = Vec::new();
        descriptions(frame.host(), &mut spoken);
        drop(frame);
        crate::shell::launcher::clear_pending_placements();
        // While it arrives the place is the app's; once it failed, the
        // failure face says what failed (the tab names the app).
        let named = if phase == "opening" {
            "Browser".to_owned()
        } else {
            crate::shell::i18n::t("shell-window-open-failed")
        };
        assert!(
            spoken
                .iter()
                .any(|(name, description)| *name == named && description == &detail),
            "{phase}: actual notice must describe launch state, got {spoken:?}"
        );
        assert!(
            !spoken
                .iter()
                .any(|(name, _)| *name == ui::widget::record_unavailable_title())
        );
        assert!(
            store.open_records().is_empty(),
            "{phase}: never manufacture a record"
        );
        assert_eq!(
            tree.read().desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id,
            TAB_A
        );
    }
}

#[test]
fn asynchronous_launch_failure_settles_saved_local_card_once() {
    let tree = tree_with_two_document_tabs();
    {
        let mut state = tree.write();
        let group = &mut state.desktop.workspaces[0].strips[0].window_groups[0];
        group.tabs.truncate(1);
        group.tabs[0].tile = StateTile::LocalApp {
            app_id: "so.tilo.browser".into(),
            label: "Browser".into(),
            note: crate::shell::peer_windows::OPENING.into(),
        };
        state.remember_local_app_record(TAB_A, 8123);
    }
    let saved = tree.read().local_app_records.clone();
    let (host, _) = crate::shell::build_tilo_overlay_host_with_records(
        tree.clone(),
        false,
        RecordStore::in_memory(),
    );
    crate::shell::launcher::push_event(crate::shell::launcher::LaunchEvent {
        package_id: "so.tilo.browser".into(),
        display_name: "Browser".into(),
        managed_agent_id: None,
        managed_workspace_id: None,
        kind: crate::shell::launcher::LaunchEventKind::SpawnFailed {
            reason: crate::shell::launcher::LaunchFailure::LauncherUnavailable,
            detail: "synthetic worker refusal".into(),
        },
    });
    assert!(host.drain_launch_events());
    assert!(
        !host.drain_launch_events(),
        "a repeated drain must be inert"
    );
    let state = tree.read();
    let records = state.local_app_records.clone();
    let tab = state.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].clone();
    let pending = crate::shell::peer_windows::any_pending(&state);
    drop(state);
    assert_eq!(records, saved, "saved app association survives failure");
    assert_eq!(tab.id, TAB_A);
    assert!(matches!(&tab.tile, StateTile::LocalApp { note, .. }
            if note == &crate::shell::i18n::t("launch-failed-launcher-unavailable")));
    assert!(
        !pending,
        "the failed saved card must not start a launch every frame"
    );
}

/// Two Word tabs, each a document of its own.
fn tree_with_two_document_tabs() -> WorkspaceTreeHandle {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 4242;
    let document = |id: u64, title: &str| Tab {
        id,
        title: title.into(),
        tile: StateTile::AppPlaceholder {
            kind: AppKind::Other("Word".into()),
            label: title.into(),
        },
        app_title: None,
    };
    let strip = Strip {
        id: strip_id,
        name: "Solo".into(),
        window_groups: vec![StateGroup {
            id: group_id,
            tabs: vec![document(TAB_A, "First"), document(TAB_B, "Second")],
            active_tab: TAB_A,
            width: StateWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    };
    let mut ws = Workspace::new(ws_id, "Home");
    ws.strips = vec![strip];
    let mut active_strip = StdMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = StdMap::new();
    active_group.insert(strip_id, group_id);
    crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree {
        desktop: crate::shell::state::DesktopTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            ..Default::default()
        },
        ..WorkspaceTree::default()
    })
}

/// A store persisting below `data_home` rather than `$XDG_DATA_HOME`,
/// in the non-routable nil room a launcher-less process gets.
fn store_at(data_home: &std::path::Path, package: &str) -> RecordStore {
    RecordStore::new(ui::record::RecordSet::open_in(
        std::rc::Rc::new(app_state::FsStore::new(data_home)),
        package,
        uuid::Uuid::nil().to_string(),
    ))
}

/// A window that did not open asks for its launch again when its Reopen is
/// pressed in the desktop itself, not only in a harness of its own: the
/// press reaches the session as a retry of that exact window.
#[test]
fn reopen_pressed_on_the_desktop_asks_for_the_launch_again() {
    let tree = tree_with_two_document_tabs();
    let detail = crate::shell::i18n::t("launch-failed-launcher-unavailable");
    {
        let mut state = tree.write();
        let group = &mut state.desktop.workspaces[0].strips[0].window_groups[0];
        group.tabs.truncate(1);
        group.tabs[0].tile = StateTile::LocalApp {
            app_id: "so.tilo.lang.notes".into(),
            label: "Notes".into(),
            note: detail,
        };
    }
    let mut frame = crate::shell::frame::FrameBuilder::with_handle_and_records(
        tree.clone(),
        false,
        (1000, 700),
        RecordStore::in_memory(),
    );
    let mut document = AutoCommit::new();
    for _ in 0..2 {
        frame.pump_apps(&mut document, &automerge::ROOT);
        frame.relayout();
    }
    assert!(!frame.paint().encoding().is_empty());
    assert!(!crate::shell::peer_windows::retry_requested());
    let target = format!("reopen-window:{TAB_A}");
    let center = frame
        .widget_registry()
        .get(&target)
        .expect("the card offers Reopen")
        .center();
    let mut focus = None;
    for event in [
        ui::synthetic_input::cursor_moved(center.x, center.y),
        ui::synthetic_input::mouse_press(),
        ui::synthetic_input::mouse_release(),
    ] {
        frame.dispatch_event(
            &event,
            &winit::event::Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            &mut focus,
        );
    }
    frame.pump_apps(&mut document, &automerge::ROOT);
    assert!(
        crate::shell::peer_windows::retry_requested(),
        "pressing Reopen asks for the window's launch again"
    );
    assert!(crate::shell::peer_windows::examine(&mut tree.write()));
    assert!(
        matches!(
            &tree.read().desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile,
            StateTile::LocalApp { note, .. } if note != &crate::shell::i18n::t("launch-failed-launcher-unavailable")
        ),
        "the card gives way to the window's launch"
    );
}
