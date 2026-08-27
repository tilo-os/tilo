//! Which document a tab is showing.
//!
//! A tab is an open thing, not a view onto one shared thing. Two Notes
//! tabs are two notes; opening a second one does not hand you the first
//! one again wearing a different pill. That is what a person means by
//! opening a second window, and it is the property this module exists to
//! keep.
//!
//! The unit is the **record** ([`ui::record`]): a type, a stable identity
//! and a home room, with one Automerge document each. A tab showing a
//! file is that file's record — the app says so through
//! [`ui::widget::Widget::record_intent`], because only the app knows
//! which file it opened. A scratch tab has no file to be named by, so it
//! is named by the tab that holds it: the tab id is minted once, salted
//! per peer, and persisted with the workspace tree, which is exactly the
//! "stable identity that survives a restart" a record needs.

use ui::record::{RecordCheckout, RecordId, RecordKind, RecordStore};
use ui::widget::WindowGroup as UiGroup;

/// Record type for a tab whose content is the app's own, unsaved
/// document — a scratch note, an untitled buffer. App-owned vocabulary,
/// per [`ui::record::RecordKind`]; the shell only needs one name for
/// "whatever this tab holds" because it never interprets the content.
pub const TAB_RECORD_KIND: &str = "tab";

/// The record a scratch tab is.
///
/// Not `RecordId::minted()`: a minted id would have to be written down
/// somewhere to be found again next launch, and the tab id already is
/// that written-down identity — it is allocated once, carries the peer
/// salt that keeps two machines from colliding, and is persisted with
/// the workspace tree. A second identity beside it could only drift.
pub fn scratch_tab_record(tab_id: u64) -> RecordCheckout {
    RecordCheckout::new(
        RecordId::for_tab(tab_id),
        RecordKind::new(TAB_RECORD_KIND),
    )
}

/// Give every tab in `group` its own document.
///
/// Called once per group as the shell builds (or rebuilds) the widget
/// tree from the workspace tree. The binding travels with the tile, so a
/// tab dragged into another group keeps its document — the tab is the
/// thing that was opened, and moving it does not make it a different
/// thing.
pub fn bind_group_records(group: &mut UiGroup, store: &RecordStore) {
    group.bind_tab_records(store, |tab_id| scratch_tab_record(tab_id));
}

/// The body a tab's app wrote, read out of that tab's own document.
///
/// The one place anything outside the widget tree looks up a tab's
/// content — in-app search above all.
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
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent as StateTile,
        WindowGroup as StateGroup, WindowWidth as StateWidth, Workspace, WorkspaceId,
        WorkspaceTree, WorkspaceTreeHandle,
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
                let event = key_press(Key::Character(winit::keyboard::SmolStr::new(
                    c.to_string(),
                )));
                self.dispatch(doc, &event, &Modifiers::default());
            }
        }

        fn dispatch(&mut self, doc: &mut AutoCommit, event: &WindowEvent, mods: &Modifiers) {
            let mut handled = false;
            let mut focus_target = None;
            self.host.handle_event(
                event,
                1.0,
                mods,
                doc,
                &automerge::ROOT,
                &mut handled,
                &mut focus_target,
                self.node,
                &self.taffy,
            );
        }
    }

    /// A `KeyboardInput` winit itself would have delivered. `KeyEvent`'s
    /// platform payload is `pub(crate)` in winit, so the zero-init
    /// workaround `ui::headless` and the shots driver already use is the
    /// only way to build one outside that crate.
    fn key_press(key: Key) -> WindowEvent {
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
        ev.text = match &key {
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
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            ..WorkspaceTree::default()
        }))
    }

    /// Every Notes tab must own its body: a word typed into one must not
    /// be found in the other.
    #[test]
    fn a_word_typed_into_one_notes_tab_is_not_found_in_the_other() {
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
            vec![TAB_B.to_string()],
            "a word typed into one Notes tab must not be found in the other; \
             both tabs matching means both are showing one body",
        );
    }

    /// Two tabs, two documents — asserted where the content actually
    /// lives, so a tab's body cannot be "right on screen" while the
    /// document behind it is shared.
    #[test]
    fn each_tab_writes_into_its_own_document() {
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

    /// Undo follows the tab. An `UndoEntry` is a pair of change hashes
    /// inside one document's change graph, so a stack shared across tabs
    /// would revert against the wrong history.
    #[test]
    fn undo_in_one_tab_leaves_the_other_alone() {
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
        let z = key_press(Key::Character(winit::keyboard::SmolStr::new("z")));
        b.dispatch(&mut shared, &z, &ctrl);

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

    /// Undo in the tab that typed **first**.
    ///
    /// The direction is the whole point: undoing the tab that typed
    /// *last* would pass even against a single stack shared by every
    /// tab, since the topmost entry happens to be the right one anyway.
    /// Undoing the tab that typed first is the case that separates them —
    /// on a shared stack the topmost entry would be the *other* tab's.
    #[test]
    fn undo_in_the_tab_that_typed_first_still_reverts_that_tab() {
        let store = RecordStore::in_memory();
        let mut shared = AutoCommit::new();
        let mut a = Tile::notes(TAB_A, &store);
        let mut b = Tile::notes(TAB_B, &store);
        a.type_str(&mut shared, "alpha");
        b.type_str(&mut shared, "beta");
        a.host.update(&mut shared, &automerge::ROOT);
        b.host.update(&mut shared, &automerge::ROOT);

        let ctrl = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        let z = key_press(Key::Character(winit::keyboard::SmolStr::new("z")));
        a.dispatch(&mut shared, &z, &ctrl);

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

    /// Two tabs, distinct content, restart: each comes back with its own.
    ///
    /// A scratch tab has no file to be found by, so this is the whole of
    /// whether its content survives being shut down — and it is why the
    /// tab id, which is persisted with the window tree, is what names the
    /// record.
    #[test]
    #[serial_test::serial(record_state_dir)]
    fn both_tabs_come_back_with_their_own_content_after_a_restart() {
        let tmp = tempfile::tempdir().expect("temp dir");
        // SAFETY: serialised on `record_state_dir`; no other thread reads
        // these between the writes here and the reads below.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("QOS_PACKAGE_ID");
        }
        const PACKAGE: &str = "dev.qos.test-tab-documents";

        {
            let store = RecordStore::open(PACKAGE);
            let mut shared = AutoCommit::new();
            let mut a = Tile::notes(TAB_A, &store);
            let mut b = Tile::notes(TAB_B, &store);
            a.type_str(&mut shared, "first tab");
            b.type_str(&mut shared, "second tab");
            store.flush();
        }

        // A fresh session: same tab ids out of the persisted window tree,
        // nothing else carried over.
        let store = RecordStore::open(PACKAGE);
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

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    /// A frame is what puts a tab's document on disk.
    ///
    /// The store's writers are debounced and off-thread, so something has
    /// to offer them the documents each frame or a long session would type
    /// and write nothing. Asserted through the shell's own per-frame
    /// `update` and read back from a *different* store, so it is the file
    /// on disk being checked and not the live copy.
    #[test]
    #[serial_test::serial(record_state_dir)]
    fn a_frame_offers_every_open_tab_to_its_writer() {
        let tmp = tempfile::tempdir().expect("temp dir");
        // SAFETY: serialised on `record_state_dir`.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("QOS_PACKAGE_ID");
        }
        const PACKAGE: &str = "dev.qos.test-frame-writeback";

        let store = RecordStore::open(PACKAGE);
        let (mut host, _chrome) = crate::shell::build_nirios_overlay_host_with_records(
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
        // The writer debounces (`app_state::DEFAULT_DEBOUNCE`, 400 ms) and
        // waits on its own thread; give it longer than that to land.
        std::thread::sleep(std::time::Duration::from_millis(900));

        let reader = RecordStore::open(PACKAGE);
        reader.open_record(&scratch_tab_record(TAB_A));
        assert_eq!(
            tab_body(&reader, TAB_A, BODY_KEY).as_deref(),
            Some("written by a frame"),
            "a frame must offer each open tab's document to its writer",
        );

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    /// Two tabs of one app must not report the same presence identity —
    /// a peer's caret is a position in *a* document, and the wrong key
    /// paints it in the wrong tab.
    #[test]
    fn two_tabs_of_one_app_have_distinct_presence_keys() {
        let store = RecordStore::in_memory();
        let a = Tile::notes(TAB_A, &store);
        let b = Tile::notes(TAB_B, &store);
        let ka = a.host.presence_key();
        let kb = b.host.presence_key();
        assert!(ka.is_some(), "a hosted tab has a presence identity");
        assert_ne!(
            ka, kb,
            "two tabs of one app must not share a presence key",
        );

        // And the key the *body* publishes under, which is the one a peer's
        // caret is looked up by (`EditorApp::draw` reads
        // `PresenceCtx::carets` with it). Same app, same field, two
        // documents — so the record has to be part of the key or the caret
        // is drawn in both tabs at once.
        let body_key = |t: &Tile| match t.host.content() {
            UiTile::App(w) => w.presence_key(),
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
