//! Which document a tab is showing.
//!
//! A tab is an open thing, not a view onto one shared thing: two Notes tabs
//! are two notes, and opening a second one never hands you the first one
//! wearing a different pill. This module exists to keep that property.
//!
//! The unit is the **record** ([`ui::record`]): a type, a stable identity
//! and a home room, with one Automerge document each. A tab showing a file
//! is that file's record, via [`ui::widget::Widget::record_intent`]. A
//! scratch tab has no file to be named by, so it is named by the tab that
//! holds it — the tab id, minted once, salted per peer, and persisted with
//! the workspace tree.

use ui::record::{RecordCheckout, RecordId, RecordKind, RecordStore};
use ui::widget::WindowGroup as UiGroup;

/// Record type for a tab whose content is the app's own, unsaved
/// document — a scratch note, an untitled buffer. The shell only needs
/// one name here since it never interprets the content.
pub const TAB_RECORD_KIND: &str = "tab";

/// The record a scratch tab is. Not `RecordId::minted()`: the tab id is
/// already a written-down identity — allocated once, peer-salted, and
/// persisted with the workspace tree — so a second one beside it could
/// only drift.
pub fn scratch_tab_record(tab_id: u64) -> RecordCheckout {
    RecordCheckout::new(RecordId::for_tab(tab_id), RecordKind::new(TAB_RECORD_KIND))
}

/// Called only by the action that minted this new tab identity, before its
/// first widget-tree rebuild. Never infer creation from an absent record.
///
/// # Errors
/// Returns transient loan contention or a definite creation/storage refusal.
pub fn create_scratch_tab_record(
    store: &RecordStore,
    tab_id: u64,
) -> Result<(), ui::record::RecordOpenError> {
    store.create_record(&scratch_tab_record(tab_id))
}

/// Failures belong to the exact fresh creation ticket, never to the saved
/// resource globally. Apply before syncing the newly constructed group.
pub(crate) fn apply_creation_failures(
    group: &mut UiGroup,
    failures: &std::collections::BTreeMap<u64, String>,
) {
    for (tab, reason) in failures {
        if let Some(host) = group.host_for_tab_mut(*tab) {
            host.refuse_record_binding(reason.clone());
        }
    }
}

/// Give every tab in `group` its own document. Called once per group as
/// the shell (re)builds the widget tree. The binding travels with the
/// tile, so a tab dragged into another group keeps its document.
pub fn bind_group_records(group: &mut UiGroup, store: &RecordStore) {
    group.bind_tab_records(store, scratch_tab_record);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent as StateTile, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
    };
    use automerge::{AutoCommit, ReadDoc as _, transaction::Transactable as _};
    use std::collections::HashMap as StdMap;
    use ui::widget::{TileContent as UiTile, Widget};

    const TAB_A: u64 = 11;
    const TAB_B: u64 = 22;
    /// Where these tests write into a tab's document, standing in for what
    /// the tab's app keeps there.
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

    #[test]
    fn tab_documents_hydrate_rebuilt_groups_and_reach_their_writer() {
        // Phase `history_rebuilt_group_waits_for_missing_record_then_accepts_hydration`.
        {
            let store = RecordStore::in_memory();
            let mut group = UiGroup::new(
                1,
                vec![(
                    ui::widget::TabDescriptor {
                        id: TAB_A,
                        title: "Saved document".into(),
                        app_kind: None,
                        icon: None,
                    },
                    UiTile::App(Box::new(word_processor::WordProcessorWidget::new())),
                )],
                TAB_A,
                ui::widget::WindowWidth::Normal,
            );
            let parent = AutoCommit::new();
            bind_group_records(&mut group, &store);
            group.sync(&parent, &automerge::ROOT);
            bind_group_records(&mut group, &store);
            group.sync(&parent, &automerge::ROOT);
            assert!(
                store.open_records().is_empty(),
                "rebuilds cannot create a missing saved record"
            );
            assert!(group.active_host().unwrap().history_owner().is_none());
            // Simulate the existing record arriving from its storage/replication owner.
            store.open_record(&scratch_tab_record(TAB_A));
            write(&store, TAB_A, "hydrated content");
            group.sync(&parent, &automerge::ROOT);
            assert_eq!(written(&store, TAB_A).as_deref(), Some("hydrated content"));
            assert!(group.active_host().unwrap().history_owner().is_some());
            assert!(
                parent.get(automerge::ROOT, KEY).unwrap().is_none(),
                "nothing reaches the shell's own document"
            );
        }

        // Phase `a_frame_offers_every_open_tab_to_its_writer`.
        // The store's writers are debounced and off-thread, so something has
        // to offer them the documents each frame. Read back from a
        // *different* store, so it's the file on disk being checked, not the
        // live copy.
        {
            const PACKAGE: &str = "so.tilo.test-frame-writeback";

            let tmp = tempfile::tempdir().expect("temp dir");

            let store = store_at(tmp.path(), PACKAGE);
            store.create_record(&scratch_tab_record(TAB_A)).unwrap();
            store.create_record(&scratch_tab_record(TAB_B)).unwrap();
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
            reader.open_record(&scratch_tab_record(TAB_A));
            assert_eq!(
                written(&reader, TAB_A).as_deref(),
                Some("written by a frame"),
                "a frame must offer each open tab's document to its writer",
            );
        }
    }

    #[test]
    fn saved_local_app_status_survives_production_frame_binding_without_a_scratch_record() {
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
            ("opening", crate::shell::peer_windows::OPENING.to_string()),
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
                    note: if phase == "interrupted" {
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
            assert!(
                spoken
                    .iter()
                    .any(|(name, description)| name == "Browser" && description == &detail),
                "{phase}: actual notice must describe launch state, got {spoken:?}"
            );
            assert!(
                !spoken
                    .iter()
                    .any(|(name, _)| name == "Document unavailable")
            );
            assert!(
                store.open_records().is_empty(),
                "{phase}: never manufacture a scratch record"
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
        let (mut host, _) = crate::shell::build_tilo_overlay_host_with_records(
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

    /// Two tabs of an app the shell builds itself, each on its own scratch
    /// record.
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
}
