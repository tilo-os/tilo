//! The door an app's own operations are reached through.
//!
//! qOS's claim is that apps expose resources and actions over CRDT state
//! the system owns, so acting on an app is a system service rather than
//! something each app reinvents. The model was there and the way in was
//! not: a resource tree published by a hosted app was readable only from
//! inside that app, so anything else — an agent, another person's session,
//! a script — could open a terminal and not touch the document on screen.
//!
//! These exercise the way in, through the same
//! [`ShellOverlayHost::action_catalog`] the `actions_enumerate` /
//! `actions_invoke` control-socket ops call, with the production provider
//! registry, real app widgets, their real per-tab documents, and the real
//! capability gate. Nothing here substitutes a fixture for a production
//! path: the widgets are the ones the shell builds for a Word tab and a
//! Files tab, and the catalogue is built by the function the socket
//! dispatcher builds it with.
//!
//! `scripts/headless-examples/app-action-door.py` drives the same two ops
//! over the socket itself against a live compositor, which is the half a
//! Rust test cannot reach without a display.

use std::collections::HashMap as StdMap;
use std::sync::Arc;

use automerge::{AutoCommit, ReadDoc, ROOT};
use parking_lot::RwLock;

use shell::shell::chrome_state::Chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::search::action_api::{ActionCatalog, ArgValue, ResourceRef};
use shell::shell::search::providers::app_resources;
use shell::shell::search::visibility::CapabilityVisibility;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent as StateTile, WindowGroup as StateGroup,
    WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
};
use ui::record::RecordStore;
use ui::widget::Widget;

const WORD_TAB: u64 = 71;
const FILES_TAB: u64 = 72;

/// The person driving the session. Their id is what an invoked action is
/// attributed to, so it has to be something a test can recognise in the
/// document's history afterwards.
fn caller() -> String {
    shell::shell::state::local_user_id()
}

/// A workspace with one group holding a Word tab and a Files tab, with
/// `active` focused. Both tiles resolve to the real in-process app widgets
/// (see `shell::shell::build_workspace_root`), not stubs.
fn tree(active: u64) -> WorkspaceTreeHandle {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 909;
    let tab = |id: u64, title: &str, kind: AppKind| Tab {
        id,
        title: title.into(),
        tile: StateTile::AppPlaceholder {
            kind,
            label: title.into(),
        },
    };
    let strip = Strip {
        id: strip_id,
        name: "Solo".into(),
        window_groups: vec![StateGroup {
            id: group_id,
            tabs: vec![
                tab(WORD_TAB, "Report", AppKind::Other("Word".into())),
                tab(FILES_TAB, "Files", AppKind::FileExplorer),
            ],
            active_tab: active,
            width: StateWidth::Normal,
            maximized: false,
        }],
    };
    let mut ws = Workspace::new(ws_id, "Home");
    ws.set_owner(&caller());
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

/// A live session focused on `active`, driven far enough that the hosted
/// apps have taken their documents through a first `sync` — which is what
/// a running app has done and an unmounted widget has not.
struct Session {
    host: ShellOverlayHost,
    chrome: Chrome,
    doc: AutoCommit,
    store: RecordStore,
}

impl Session {
    fn focused_on(active: u64) -> Self {
        let store = RecordStore::in_memory();
        let (mut host, chrome) =
            shell::shell::build_nirios_overlay_host_with_records(tree(active), false, store.clone());
        let mut doc = AutoCommit::new();
        // One frame. Apps seed their documents in `sync`, so before this
        // the word processor has no block for a caret to be in and
        // publishes nothing — which is the app being unmounted, not the
        // door being empty.
        host.sync(&mut doc, &ROOT);
        Self {
            host,
            chrome,
            doc,
            store,
        }
    }

    /// Build a catalogue exactly as the control socket's dispatcher does:
    /// through the capability gate, over the production registry.
    fn catalog_as(&self, person: &str, query: &str) -> ActionCatalog {
        let st = self.chrome.borrow();
        let tree = st.tree();
        let workspace = tree
            .active_workspace()
            .expect("the session has an active workspace");
        let actor = workspace.actor_for_person(&person.to_string());
        let gate = CapabilityVisibility::new(workspace, actor);
        self.host
            .action_catalog(query, &st, &self.doc, &ROOT, &gate)
    }

    fn catalog(&self, query: &str) -> ActionCatalog {
        self.catalog_as(&caller(), query)
    }

    /// Every app-exported address the caller can currently see.
    fn addresses(&self, query: &str) -> Vec<String> {
        self.catalog(query)
            .enumerate()
            .into_iter()
            .filter(|a| a.resource.provider == app_resources::PROVIDER_ID)
            .map(|a| a.resource.id)
            .collect()
    }

    /// Invoke one address the way the socket does: re-resolve against a
    /// freshly built catalogue, then run it.
    fn invoke(
        &mut self,
        id: &str,
        args: Vec<(String, ArgValue)>,
    ) -> Result<(), shell::shell::search::action_api::ActionError> {
        let catalog = self.catalog("");
        let selected = catalog
            .select(&ResourceRef::new(app_resources::PROVIDER_ID, id))
            .unwrap_or_else(|| panic!("address {id} is not in the catalogue"));
        let chrome = self.chrome.clone();
        selected.invoke(&mut self.doc, &ROOT, &chrome, args)
    }

    /// The document the focused app is actually on.
    ///
    /// Taken straight out of the store rather than from anything the door
    /// returned, so every assertion below reads the state back from where
    /// the app keeps it.
    fn app_doc<R>(&self, f: impl FnOnce(&mut AutoCommit) -> R) -> R {
        let id = ui::record::RecordId::for_tab(self.focused_tab());
        self.store
            .with(&id, f)
            .expect("the focused tab has a record document")
    }

    fn focused_tab(&self) -> u64 {
        self.chrome
            .borrow()
            .active_tab_id()
            .expect("a tab is focused")
    }
}

/// The actor every change in `doc` was committed under, newest last.
fn change_actors(doc: &mut AutoCommit) -> Vec<Vec<u8>> {
    doc.get_changes(&[])
        .iter()
        .map(|c| c.actor_id().to_bytes().to_vec())
        .collect()
}

/// 1. A caller that never sees the screen asks what the running app has
///    open, and gets the tree the app itself publishes.
///
/// Addressed by ids and labels only — no coordinate, no index into a
/// rendered list — which is the property that makes the surface drivable
/// by something with no display.
#[test]
fn an_out_of_process_caller_enumerates_a_running_apps_resources() {
    let session = Session::focused_on(WORD_TAB);
    let listed = session.addresses("");
    assert!(
        !listed.is_empty(),
        "the focused app publishes a resource tree; the door listed nothing: {listed:?}"
    );
    // The word processor's own vocabulary, reached without the word
    // processor having grown any remote-control surface of its own.
    assert!(
        listed
            .iter()
            .any(|id| id == "word-processor.document#insert-table"),
        "the document's own exported action must be addressable, got {listed:?}"
    );
    for id in &listed {
        assert!(
            app_resources::decode_address(id).is_some(),
            "every address must parse back into (resource, action): {id}"
        );
        assert!(
            !id.chars().any(|c| c.is_ascii_digit() && id.starts_with(c)),
            "an address must not lead with a positional index: {id}"
        );
    }

    // The descriptors say what an invocation must carry, so an argument is
    // never guessed from a label.
    let catalog = session.catalog("");
    let described = catalog
        .enumerate()
        .into_iter()
        .find(|a| a.resource.id == "word-processor.document#insert-table")
        .expect("the enumerated address is described");
    assert!(
        described.params.is_empty(),
        "insert-table takes nothing, and says so"
    );
    assert!(
        described.subtitle.is_some_and(|s| s.contains("word_processor")),
        "the descriptor names the app's own resource kind"
    );
}

/// 2. Addressing one of those operations changes the app's document.
///
/// Read back out of the record the app is actually on, not out of the
/// reply that asked for it.
#[test]
fn an_addressed_action_changes_the_apps_document() {
    let mut session = Session::focused_on(WORD_TAB);
    let before = session.app_doc(|d| d.get_heads());

    session
        .invoke("word-processor.document#insert-table", Vec::new())
        .expect("the addressed action ran");

    let after = session.app_doc(|d| d.get_heads());
    assert_ne!(
        before, after,
        "invoking an address must move the app's own document"
    );
    // And it is the app's document that moved, with the app's own schema
    // in it — not some scratch document the shell happened to carry.
    let blocks = session.app_doc(|d| d.get(&ROOT, "blocks").ok().flatten().is_some());
    assert!(blocks, "the change landed in the word processor's document");
}

/// 3. The change is attributed to the caller, not to the app.
///
/// A participant reaching an app from outside is still a participant: the
/// room's history has to name them the way it names someone who typed.
#[test]
fn an_action_invoked_through_the_door_is_attributed_to_the_caller() {
    let mut session = Session::focused_on(WORD_TAB);
    let before = session.app_doc(change_actors);

    session
        .invoke("word-processor.document#insert-table", Vec::new())
        .expect("the addressed action ran");

    let after = session.app_doc(change_actors);
    assert!(
        after.len() > before.len(),
        "the invocation committed a change"
    );
    let me = caller().into_bytes();
    assert!(
        after[before.len()..].iter().any(|a| *a == me),
        "the change the door made must commit under the caller's actor \
         ({:?}), not an anonymous one; got {:?}",
        String::from_utf8_lossy(&me),
        after[before.len()..]
            .iter()
            .map(|a| String::from_utf8_lossy(a).to_string())
            .collect::<Vec<_>>()
    );
}

/// 4. A caller the room has not granted anything is refused, and nothing
///    moves.
///
/// The refusal is absence: a denied operation never becomes a descriptor,
/// so it cannot be enumerated and its address resolves to nothing. That is
/// what stops the gate being advisory — there is no "allowed" flag on a
/// row for a caller to ignore.
#[test]
fn a_caller_without_the_capability_is_refused_and_nothing_changes() {
    let session = Session::focused_on(WORD_TAB);
    let before = session.app_doc(|d| d.get_heads());

    let catalog = session.catalog_as("mallory", "");
    let visible: Vec<String> = catalog
        .enumerate()
        .into_iter()
        .filter(|a| a.resource.provider == app_resources::PROVIDER_ID)
        .map(|a| a.resource.id)
        .collect();
    assert!(
        visible.is_empty(),
        "a caller with no grant in this room must not be shown the app's \
         operations, got {visible:?}"
    );
    assert!(
        catalog
            .select(&ResourceRef::new(
                app_resources::PROVIDER_ID,
                "word-processor.document#insert-table"
            ))
            .is_none(),
        "and must not be able to resolve one it was never shown"
    );
    assert_eq!(
        before,
        session.app_doc(|d| d.get_heads()),
        "a refused caller must leave the document untouched"
    );
}

/// 5. Asking what is possible does nothing.
///
/// Surveying is what a caller exploring an unfamiliar app does constantly,
/// so it has to be free — including of the second-order effects an
/// enumeration could have: opening a palette, bumping a revision, seeding
/// a document.
#[test]
fn enumeration_fires_no_side_effects() {
    let session = Session::focused_on(WORD_TAB);
    let doc_before = session.app_doc(|d| d.get_heads());
    let chrome_before = {
        let st = session.chrome.borrow();
        (st.revision, st.overlays.clone(), st.search_scope)
    };

    for _ in 0..3 {
        let listed = session.addresses("");
        assert!(!listed.is_empty(), "the survey answered");
    }

    assert_eq!(
        doc_before,
        session.app_doc(|d| d.get_heads()),
        "surveying must not write to the app's document"
    );
    let chrome_after = {
        let st = session.chrome.borrow();
        (st.revision, st.overlays.clone(), st.search_scope)
    };
    assert_eq!(
        chrome_before, chrome_after,
        "surveying must not open anything or bump a revision"
    );
}

/// 6. The same door serves a directory tree.
///
/// The file explorer's resources are paths, not blocks and offsets, and
/// its one action takes an argument where the word processor's took none.
/// If anything in the door had quietly assumed a rich-text document, this
/// is where it would show.
#[test]
fn the_same_door_works_for_a_directory_tree_and_a_document() {
    let mut session = Session::focused_on(FILES_TAB);
    let listed = session.addresses("");
    assert!(
        !listed.is_empty(),
        "the file explorer publishes its open directory; got {listed:?}"
    );

    let rename = listed
        .iter()
        .find(|id| id.starts_with("files.entry:") && id.ends_with("#rename"))
        .unwrap_or_else(|| {
            panic!("an entry in the open directory must be addressable, got {listed:?}")
        })
        .clone();
    // The address carries an absolute path, `.` and `/` and all, and still
    // round-trips — the shape a document-only address scheme would have
    // lost.
    let parsed = app_resources::decode_address(&rename).expect("the entry's address parses");
    assert!(
        parsed.resource.contains('/'),
        "a file entry is addressed by its path: {}",
        parsed.resource
    );
    assert_eq!(parsed.action, "rename");

    // It advertises the argument it collects, which the word processor's
    // action did not — so a caller with no screen learns the difference
    // from the descriptor rather than by trying.
    let catalog = session.catalog("");
    let described = catalog
        .enumerate()
        .into_iter()
        .find(|a| a.resource.id == rename)
        .expect("the entry's action is described");
    assert_eq!(
        described.params.len(),
        1,
        "rename collects one argument and says so"
    );

    // Missing the argument is refused before anything runs.
    assert!(
        session.invoke(&rename, Vec::new()).is_err(),
        "an invocation that does not satisfy the declared parameter must be refused"
    );

    session
        .invoke(
            &rename,
            vec![("text".to_string(), ArgValue::Text("renamed.md".into()))],
        )
        .expect("the addressed rename ran");

    let buffer = session.app_doc(|d| match d.get(&ROOT, "files.rename_query") {
        Ok(Some((automerge::Value::Object(automerge::ObjType::Text), id))) => d.text(&id).ok(),
        _ => None,
    });
    assert_eq!(
        buffer.as_deref(),
        Some("renamed.md"),
        "the file explorer's own shared state carries what the door asked for"
    );
}
