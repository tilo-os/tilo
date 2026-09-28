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

/// The body a tab's app wrote, read out of that tab's own document — the
/// one place outside the widget tree that looks up a tab's content
/// (in-app search above all).
pub fn tab_body(store: &RecordStore, tab_id: u64, body_key: &str) -> Option<String> {
    store.read(&RecordId::for_tab(tab_id), |doc| {
        crate::shell::crdt_text::read_body(doc, &automerge::ROOT, body_key)
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::notes_app::{BODY_KEY, NotesApp};
    use crate::shell::search::providers::notes::NotesProvider;
    use crate::shell::search::{ResourceKind, SearchContext, SearchProvider};
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent as StateTile, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
    };
    use automerge::AutoCommit;
    use parking_lot::RwLock;
    use std::collections::HashMap as StdMap;
    use std::sync::Arc;
    use taffy::{NodeId, TaffyTree};
    use ui::theme::Theme;
    use ui::widget::{TileContent as UiTile, TileHost, Widget};
    use winit::event::{ElementState, Modifiers, WindowEvent};
    use winit::keyboard::Key;

    const TAB_A: u64 = 11;
    const TAB_B: u64 = 22;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn tab_documents_hydrate_rebuilt_groups_and_reach_their_writer() {
        // Phase `history_rebuilt_group_waits_for_missing_record_then_accepts_hydration`.
        {
            let store = RecordStore::in_memory();
            let mut group = UiGroup::new(
                1,
                vec![(
                    ui::widget::TabDescriptor {
                        id: TAB_A,
                        title: "Saved note".into(),
                        app_kind: None,
                        icon: None,
                    },
                    UiTile::App(Box::new(NotesApp::new())),
                )],
                TAB_A,
                ui::widget::WindowWidth::Normal,
            );
            let mut parent = AutoCommit::new();
            bind_group_records(&mut group, &store);
            group.sync(&mut parent, &automerge::ROOT);
            bind_group_records(&mut group, &store);
            group.sync(&mut parent, &automerge::ROOT);
            assert!(
                store.open_records().is_empty(),
                "rebuilds cannot create a missing saved record"
            );
            assert!(group.active_host().unwrap().history_owner().is_none());
            // Simulate the existing record arriving from its storage/replication owner.
            store.open_record(&scratch_tab_record(TAB_A));
            store.with(&RecordId::for_tab(TAB_A), |doc| {
                NotesApp::with_seed("hydrated content").sync(doc, &automerge::ROOT);
            });
            group.sync(&mut parent, &automerge::ROOT);
            assert_eq!(
                tab_body(&store, TAB_A, BODY_KEY).as_deref(),
                Some("hydrated content")
            );
            assert!(group.active_host().unwrap().history_owner().is_some());
            assert!(
                crate::shell::crdt_text::read_body(&parent, &automerge::ROOT, BODY_KEY).is_none()
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
                tree_with_two_note_tabs(),
                false,
                store.clone(),
            );
            // What a tab's app writes when the user types into it.
            store.with(&RecordId::for_tab(TAB_A), |doc| {
                let mut app = NotesApp::with_seed("written by a frame");
                app.sync(doc, &automerge::ROOT);
            });

            let mut shell_doc = AutoCommit::new();
            host.update(&mut shell_doc, &automerge::ROOT);
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
                tab_body(&reader, TAB_A, BODY_KEY).as_deref(),
                Some("written by a frame"),
                "a frame must offer each open tab's document to its writer",
            );
        }
    }

    /// One hosted tile, laid out on its own taffy tree so keystrokes
    /// reach the app through `TileHost::handle_event` — the production
    /// dispatch, not a direct poke at the widget.
    struct Tile {
        host: TileHost,
        taffy: TaffyTree<()>,
        node: NodeId,
    }

    impl Tile {
        fn notes(tab_id: u64, store: &RecordStore) -> Self {
            store.open_record(&scratch_tab_record(tab_id));
            let mut host = TileHost::new(UiTile::App(Box::new(NotesApp::new())));
            host.bind_record(store.clone(), scratch_tab_record(tab_id));
            let mut taffy = TaffyTree::new();
            let mut node_map = std::collections::HashMap::new();
            let node = host.build_layout(&mut taffy, &Theme::default(), &mut node_map);
            if let UiTile::App(w) = host.content_mut() {
                w.set_focus(true);
            }
            Self { host, taffy, node }
        }

        /// Type `s` one character at a time, exactly as a keyboard does.
        fn type_str(&mut self, doc: &mut AutoCommit, s: &str) {
            self.host.sync(doc, &automerge::ROOT);
            for c in s.chars() {
                let event = key_press(&Key::Character(winit::keyboard::SmolStr::new(
                    c.to_string(),
                )));
                self.dispatch(doc, &event, Modifiers::default());
            }
        }

        fn dispatch(&mut self, doc: &mut AutoCommit, event: &WindowEvent, mods: Modifiers) {
            let mut handled = false;
            let mut focus_target = None;
            self.host.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: doc,
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: self.node,
                    taffy: &self.taffy,
                },
            );
        }
    }

    /// A `KeyboardInput` winit itself would have delivered. `KeyEvent`'s
    /// platform payload is `pub(crate)` in winit, so this is the same
    /// zero-init workaround `ui::headless` and the shots driver use.
    fn key_press(key: &Key) -> WindowEvent {
        // SAFETY: every field the read paths touch is overwritten below;
        // the platform payload is an all-zero-valid Option-shaped struct
        // on Linux and nothing downstream of `handle_event` reads it.
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = key.clone();
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = match key {
            Key::Character(c) => Some(c.clone()),
            _ => None,
        };
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn tree_with_two_note_tabs() -> WorkspaceTreeHandle {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id: GroupId = 4242;
        let note = |id: u64, title: &str| Tab {
            id,
            title: title.into(),
            tile: StateTile::AppPlaceholder {
                kind: AppKind::Other("Notes".into()),
                label: title.into(),
            },
        };
        let strip = Strip {
            id: strip_id,
            name: "Solo".into(),
            window_groups: vec![StateGroup {
                id: group_id,
                tabs: vec![note(TAB_A, "First"), note(TAB_B, "Second")],
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
        Arc::new(RwLock::new(WorkspaceTree {
            desktop: crate::shell::state::DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                active_strip,
                active_group,
                ..Default::default()
            },
            ..WorkspaceTree::default()
        }))
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn each_tab_of_one_app_owns_its_document_undo_and_presence() {
        // Phase `a_word_typed_into_one_notes_tab_is_not_found_in_the_other`.
        // Every Notes tab must own its body: a word typed into one must not
        // be found in the other.
        {
            let store = RecordStore::in_memory();
            let chrome = ChromeState::new_with_handle(tree_with_two_note_tabs());
            chrome.borrow_mut().set_records(store.clone());
            let mut shared = AutoCommit::new();

            let mut a = Tile::notes(TAB_A, &store);
            let mut b = Tile::notes(TAB_B, &store);
            a.type_str(&mut shared, "alpha");
            b.type_str(&mut shared, "xylophone");

            let chrome_st = chrome.borrow();
            let hits = NotesProvider::new().query(SearchContext {
                needle: "xylophone",
                wanted: &[ResourceKind::Note],
                chrome: &chrome_st,
                doc: &shared,
                doc_root: &automerge::ROOT,
            });
            let ids: Vec<String> = hits.iter().map(|r| r.id.clone()).collect();
            assert_eq!(
                ids,
                vec![ui::record::RecordId::for_tab(TAB_B).to_string()],
                "a word typed into one Notes tab must not be found in the other; \
                 both tabs matching means both are showing one body",
            );
        }

        // Phase `each_tab_writes_into_its_own_document`.
        // Asserted where the content actually lives, so a tab's body cannot
        // be "right on screen" while the document behind it is shared.
        {
            let store = RecordStore::in_memory();
            let mut shared = AutoCommit::new();
            let mut a = Tile::notes(TAB_A, &store);
            let mut b = Tile::notes(TAB_B, &store);
            a.type_str(&mut shared, "alpha");
            b.type_str(&mut shared, "beta");

            assert_eq!(
                tab_body(&store, TAB_A, BODY_KEY).as_deref(),
                Some("alpha"),
                "tab A's document holds what was typed into tab A",
            );
            assert_eq!(
                tab_body(&store, TAB_B, BODY_KEY).as_deref(),
                Some("beta"),
                "and tab B's holds its own",
            );
            assert_eq!(
                crate::shell::crdt_text::read_body(&shared, &automerge::ROOT, BODY_KEY),
                None,
                "nothing was written into the shell's own document",
            );
        }

        // Phase `undo_in_one_tab_leaves_the_other_alone`.
        // An `UndoEntry` is a pair of change hashes inside one document's
        // change graph, so a stack shared across tabs would revert against
        // the wrong history.
        {
            let store = RecordStore::in_memory();
            let mut shared = AutoCommit::new();
            let mut a = Tile::notes(TAB_A, &store);
            let mut b = Tile::notes(TAB_B, &store);
            a.type_str(&mut shared, "alpha");
            b.type_str(&mut shared, "beta");
            // Close the typing bursts the way an idle frame does.
            a.host.update(&mut shared, &automerge::ROOT);
            b.host.update(&mut shared, &automerge::ROOT);

            let ctrl = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
            let z = key_press(&Key::Character(winit::keyboard::SmolStr::new("z")));
            b.dispatch(&mut shared, &z, ctrl);

            assert_eq!(
                tab_body(&store, TAB_B, BODY_KEY).as_deref(),
                Some(""),
                "undo in tab B reverts tab B's own typing",
            );
            assert_eq!(
                tab_body(&store, TAB_A, BODY_KEY).as_deref(),
                Some("alpha"),
                "and leaves tab A untouched",
            );
        }

        // Phase `undo_in_the_tab_that_typed_first_still_reverts_that_tab`.
        // Undo in the tab that typed **first**: undoing whoever typed last
        // would pass even on a single shared stack (the topmost entry happens
        // to be right anyway). Undoing the tab that typed first is the case
        // that actually separates a shared stack from a per-tab one.
        {
            let store = RecordStore::in_memory();
            let mut shared = AutoCommit::new();
            let mut a = Tile::notes(TAB_A, &store);
            let mut b = Tile::notes(TAB_B, &store);
            a.type_str(&mut shared, "alpha");
            b.type_str(&mut shared, "beta");
            a.host.update(&mut shared, &automerge::ROOT);
            b.host.update(&mut shared, &automerge::ROOT);

            let ctrl = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
            let z = key_press(&Key::Character(winit::keyboard::SmolStr::new("z")));
            a.dispatch(&mut shared, &z, ctrl);

            assert_eq!(
                tab_body(&store, TAB_A, BODY_KEY).as_deref(),
                Some(""),
                "undo in tab A reverts tab A's own typing, even though tab B \
                 typed more recently",
            );
            assert_eq!(
                tab_body(&store, TAB_B, BODY_KEY).as_deref(),
                Some("beta"),
                "and leaves tab B's text alone",
            );
        }

        // Phase `both_tabs_come_back_with_their_own_content_after_a_restart`.
        {
            const PACKAGE: &str = crate::shell::SHELL_RECORDS_PACKAGE;

            let tmp = tempfile::tempdir().expect("temp dir");

            {
                let store = store_at(tmp.path(), PACKAGE);
                store.create_record(&scratch_tab_record(TAB_A)).unwrap();
                store.create_record(&scratch_tab_record(TAB_B)).unwrap();
                let mut shared = AutoCommit::new();
                let mut a = Tile::notes(TAB_A, &store);
                let mut b = Tile::notes(TAB_B, &store);
                a.type_str(&mut shared, "first tab");
                b.type_str(&mut shared, "second tab");
                assert_eq!(
                    tab_body(&store, TAB_A, BODY_KEY).as_deref(),
                    Some("first tab")
                );
                assert_eq!(
                    tab_body(&store, TAB_B, BODY_KEY).as_deref(),
                    Some("second tab")
                );
                store.try_flush().expect("both tab records reached storage");
            }

            // A fresh session: same tab ids out of the persisted window tree,
            // nothing else carried over.
            let store = store_at(tmp.path(), PACKAGE);
            let mut shared = AutoCommit::new();
            let mut a = Tile::notes(TAB_A, &store);
            let mut b = Tile::notes(TAB_B, &store);
            a.host.sync(&mut shared, &automerge::ROOT);
            b.host.sync(&mut shared, &automerge::ROOT);

            assert_eq!(
                tab_body(&store, TAB_A, BODY_KEY).as_deref(),
                Some("first tab"),
                "the first tab came back with its own content",
            );
            assert_eq!(
                tab_body(&store, TAB_B, BODY_KEY).as_deref(),
                Some("second tab"),
                "and the second with its own, not the first's",
            );
        }

        // Phase `two_tabs_of_one_app_have_distinct_presence_keys`.
        // A peer's caret is a position in *a* document; the wrong key paints
        // it in the wrong tab.
        {
            let store = RecordStore::in_memory();
            let a = Tile::notes(TAB_A, &store);
            let b = Tile::notes(TAB_B, &store);
            let ka = ui::presence::capability::presence_key(&a.host);
            let kb = ui::presence::capability::presence_key(&b.host);
            assert!(ka.is_some(), "a hosted tab has a presence identity");
            assert_ne!(ka, kb, "two tabs of one app must not share a presence key");

            // Same for the key the *body* publishes under (what
            // `EditorApp::draw` looks caret up by): the record must be part
            // of it or the caret is drawn in both tabs at once.
            let body_key = |t: &Tile| match t.host.content() {
                UiTile::App(w) => ui::presence::capability::presence_key(w.as_ref()),
                _ => None,
            };
            let ba = body_key(&a).expect("the notes body publishes presence");
            let bb = body_key(&b).expect("and so does the other tab's");
            assert_ne!(
                ba, bb,
                "two tabs' bodies must not publish under one presence key",
            );
        }
    }

    /// A scratch tab has no file to be found by, so this is the whole of
    /// whether its content survives a restart — and why the tab id,
    /// persisted with the window tree, is what names the record.
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
