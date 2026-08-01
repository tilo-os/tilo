//! Resource + action-model status for the database client.
//!
//! The qOS action-model recipe (`docs/action-model-recipe.md`) splits every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`](qos_actions::Action) — a serializable value that
//! deterministically changes the shared Automerge document, so undo and
//! multi-peer replay come for free — while presentation / view state stays plain
//! app code. Applying that split to the database client leaves **nothing** on the
//! action side, and this module records that finding rather than fabricate a
//! saved-queries or settings model just to have actions.
//!
//! ## Everything that changes the world is external
//!
//! The database client is a projection of external systems — a live SQLite,
//! PostgreSQL or MySQL server. Connecting / disconnecting, running a query,
//! browsing a table's rows, introspecting the catalog and the result sets they
//! return are all **driver / network** side effects (see [`crate::engine`],
//! [`crate::engine_pg`], [`crate::engine_mysql`] and the methods on
//! [`crate::session::Session`]). They mutate a database server or fill a
//! transient in-memory [`Session`](crate::session::Session), never the shared
//! CRDT, so by the recipe's rule none of them is an action and all stay as-is. A
//! refused socket surfaces as an honest error, not a replicated document change.
//!
//! ## Saved connections are local config, not CRDT records
//!
//! The one thing that might look like a git-client-style "durable preference that
//! *is* an action" is the list of saved connection profiles. It is **not** an
//! action, because it never enters the shared document: [`Session`] persists
//! profiles to a per-machine JSON file
//! (`$XDG_CONFIG_HOME/qos/db_client/connections.json`, see
//! [`crate::session`]), and the `Session` type's whole API takes **no**
//! `AutoCommit` — connection persistence is filesystem I/O, structurally outside
//! the CRDT. That is deliberate: a profile carries a password (held in memory,
//! `#[serde(skip)]`) and points at a host reachable only from this machine, so
//! it is local configuration, not collaborative data to replicate to a peer. The
//! recipe's rule is *does it mutate the shared document*; this does not.
//!
//! ## The only shared-document state is native text
//!
//! What remains in the Automerge document is exactly one thing: the SQL query
//! buffer, a single Automerge `Text` at `root["editor"]["sql"]` (see
//! [`crate::doc`]). It is edited in place through the syntax-highlighting
//! [`SqlEditor`](crate::editor::SqlEditor) on the toolkit's CRDT-native text path
//! — each keystroke `splice`s the `Text`, so caret, undo/redo and multi-peer
//! convergence all live there. The recipe is explicit that a flowing text body
//! must take the native text path *instead of* the action executor: the `Text`
//! object already replicates and undoes for free, and re-encoding a keystroke as
//! an action would duplicate what it gives.
//!
//! ## Presentation is never an action
//!
//! Which connection is active, which table is selected, the current main-panel
//! tab (Query / Data / Structure), the results-grid sort and scroll, and the
//! responsive sidebar flap are per-peer view state held on the widget. The recipe
//! names "the current selection, an overlay being open" as presentation that must
//! *never* be an action.
//!
//! ## Consequence
//!
//! The database client exports **no** actions and publishes **no** Ctrl+K
//! resource tree: [`Widget::resources`](ui::widget::Widget::resources) stays
//! empty and
//! [`take_committed_action`](ui::widget::Widget::take_committed_action) stays
//! `None` (both toolkit defaults, so no [`ui::ActionHost`] is embedded — there is
//! nothing for it to dispatch, and `main.rs` registers no action decoder). The
//! tests below prove each half of this split on the real production path.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
    use taffy::{NodeId, TaffyTree};
    use uuid::Uuid;
    use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{
        Key, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey, SmolStr,
    };

    use ui::widget::Widget;

    use crate::app::DbClientApp;
    use crate::model::{ConnectionProfile, Engine};
    use crate::session::Session;

    fn device_id() -> DeviceId {
        // winit exposes no public DeviceId constructor; tests only need a value.
        unsafe { std::mem::zeroed() }
    }

    /// A character keypress through the real winit event path (the same trick the
    /// app's own widget tests use for winit's `pub(crate)` `KeyEventExtra`).
    fn char_event(c: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(c));
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(c));
        WindowEvent::KeyboardInput { device_id: device_id(), event: ev, is_synthetic: false }
    }

    fn enter_event() -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Enter);
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput { device_id: device_id(), event: ev, is_synthetic: false }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    /// Pump one event through the production `Widget::handle_event`, returning
    /// whether it was consumed.
    fn send(app: &mut DbClientApp, doc: &mut AutoCommit, ev: &WindowEvent, mods: &Modifiers) -> bool {
        let taffy = TaffyTree::<()>::new();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev, 1.0, mods, doc, &ROOT, &mut handled, &mut focus, NodeId::new(0), &taffy,
        );
        handled
    }

    /// A demo app with its first `sync` run so `root["editor"]["sql"]` is seeded
    /// with the starter query and the editor is bound — the same state the live
    /// app reaches on its first frame with an open connection.
    fn synced() -> (DbClientApp, AutoCommit) {
        let mut app = DbClientApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    fn editor_map(doc: &AutoCommit) -> ObjId {
        match doc.get(&ROOT, "editor").unwrap() {
            Some((Value::Object(ObjType::Map), id)) => id,
            other => panic!("root[\"editor\"] must be an Automerge Map, got {other:?}"),
        }
    }

    fn sql_text(doc: &AutoCommit) -> ObjId {
        let editor = editor_map(doc);
        match doc.get(&editor, "sql").unwrap() {
            Some((Value::Object(ObjType::Text), id)) => id,
            other => panic!("editor[\"sql\"] must be an Automerge Text, got {other:?}"),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("db-client-actions-{tag}-{}", Uuid::new_v4()))
    }

    /// The shared document holds exactly one thing: the SQL editor buffer. There
    /// is no structural CRDT — no saved-connection collection, no saved-queries /
    /// snippets, no per-connection view prefs, no favourites, no settings map — so
    /// there is nothing for an action to mutate beyond the native `Text`.
    #[test]
    fn sql_text_is_the_only_crdt_state() {
        let (_app, doc) = synced();

        // The whole document is a single `editor` map — nothing else at the root.
        let root_keys: Vec<String> = doc.keys(&ROOT).collect();
        assert_eq!(
            root_keys,
            vec!["editor".to_string()],
            "the document holds only the editor map, nothing structural: {root_keys:?}"
        );

        // Inside `editor`, the only key is the `sql` Text buffer.
        let editor = editor_map(&doc);
        let editor_keys: Vec<String> = doc.keys(&editor).collect();
        assert_eq!(
            editor_keys,
            vec!["sql".to_string()],
            "editor map holds only the sql text: {editor_keys:?}"
        );
        match doc.get(&editor, "sql").unwrap() {
            Some((Value::Object(ObjType::Text), _)) => {}
            other => panic!("editor[\"sql\"] must be an Automerge Text, got {other:?}"),
        }

        // No user-authored record collection exists anywhere to hang an action
        // off — proving this isn't a git-client-style "saved prefs are actions".
        for forbidden in [
            "connections", "profiles", "queries", "snippets", "favourites",
            "favorites", "settings", "prefs",
        ] {
            assert!(
                doc.get(&ROOT, forbidden).unwrap().is_none(),
                "document must not carry a `{forbidden}` record collection at the root",
            );
            assert!(
                doc.get(&editor, forbidden).unwrap().is_none(),
                "editor map must not carry a `{forbidden}` record collection",
            );
        }
    }

    /// The collaboration guarantee the native text path already provides — the
    /// same one an action executor would give: a keystroke driven through the
    /// production `Widget::handle_event` splices into `root["editor"]["sql"]`, and
    /// an independent replica that merges the change converges byte-identically
    /// (same head hashes, not merely the same text). So no action is needed.
    #[test]
    fn typing_replicates_byte_identically_on_a_peer() {
        let (mut app, mut doc) = synced();
        let sql = sql_text(&doc);
        doc.commit();
        let base = doc.save();
        let before = doc.text(&sql).unwrap();

        let mut peer = AutoCommit::load(&base).unwrap();
        assert_eq!(doc.get_heads(), peer.get_heads(), "precondition: replicas share a base");

        // No modifiers → the keystroke falls through to the focused SQL editor,
        // which splices it into the bound `sql` Text on the CRDT-native path.
        assert!(
            send(&mut app, &mut doc, &char_event("Z"), &Modifiers::default()),
            "keystroke consumed by the editor"
        );
        doc.commit();
        let after = doc.text(&sql).unwrap();
        assert_eq!(
            after.chars().count(),
            before.chars().count() + 1,
            "the keystroke inserted exactly one character into the shared text"
        );

        peer.merge(&mut doc).unwrap();
        assert_eq!(doc.get_heads(), peer.get_heads(), "byte-identical convergence after merge");
        assert_eq!(peer.text(&sql).unwrap(), after, "the peer sees the same query buffer");
    }

    /// Running a query is **external**: the production Ctrl+Enter path executes
    /// the buffer against the live engine and fills the in-memory result set, but
    /// writes nothing to the shared document — proving query execution is a driver
    /// side effect, not a document mutation, so it is not an action.
    #[test]
    fn running_a_query_writes_nothing_to_the_document() {
        let (mut app, mut doc) = synced();
        doc.commit();
        let heads_before = doc.get_heads();

        // Ctrl+Enter runs the seeded query against the demo's sample SQLite engine.
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        assert!(
            send(&mut app, &mut doc, &enter_event(), &ctrl),
            "Ctrl+Enter consumed by the run shortcut"
        );

        assert_eq!(
            doc.get_heads(),
            heads_before,
            "running a query mutates the engine's result set, never the shared document",
        );
    }

    /// The query genuinely executes against a real engine (the "external" side is
    /// a real side effect, not a silent no-op): a fresh sample session opens the
    /// bundled SQLite catalog and its default query returns a non-empty row set.
    /// This runs entirely off the `Session` API, which takes no `AutoCommit` —
    /// the live result path is structurally outside the CRDT.
    #[test]
    fn query_execution_is_real_and_document_free() {
        let session = Session::with_sample();
        assert!(session.engine.is_some(), "the sample engine opened for real");
        let result = session.query_result.as_ref().expect("the default query ran");
        assert!(result.is_select, "the default query returned a row set");
        assert!(!result.rows.is_empty(), "the row set is non-empty (a real query executed)");
    }

    /// Saved connection profiles persist to a per-machine JSON file, **not** the
    /// CRDT: the production `Session::add_connection` path opens a real local
    /// SQLite file, writes `connections.json` to the data directory, and a fresh
    /// session over the same directory reloads it from disk. The `Session` API
    /// never receives an `AutoCommit`, so this durable state is filesystem I/O,
    /// structurally outside the shared document — hence not an action.
    #[test]
    fn saved_connections_persist_outside_the_crdt() {
        let dir = temp_dir("persist");
        let db_file = dir.join("app.db");
        std::fs::create_dir_all(&dir).unwrap();

        let mut session = Session::load_from(dir.clone());
        let mut profile = ConnectionProfile::new("Local app DB", Engine::Sqlite);
        profile.file_path = db_file.to_string_lossy().into_owned();
        session.add_connection(profile);

        // Persistence sink is a JSON file on disk, not a CRDT change.
        let json = dir.join("connections.json");
        assert!(json.exists(), "the profile persisted to a local JSON file, not the document");

        // A brand-new session over the same directory reloads it from the file.
        let reloaded = Session::load_from(dir);
        assert_eq!(reloaded.connections.len(), 1, "saved connection reloads from disk");
        assert_eq!(reloaded.connections[0].name, "Local app DB");
    }

    /// The honest steady state: the database client fabricates no action or
    /// resource surface. Exercised on the live widget *after* real interaction (a
    /// keystroke into the editor and a stray click), `Widget::resources` is empty
    /// (nothing to publish to Ctrl+K) and no action-host undo boundary is recorded
    /// — both toolkit defaults, because no action is ever dispatched.
    #[test]
    fn exports_no_actions_and_no_resource_tree() {
        let (mut app, mut doc) = synced();

        // Real event path: type a character, then a stray click on the surface.
        send(&mut app, &mut doc, &char_event("x"), &Modifiers::default());
        send(&mut app, &mut doc, &left_press(), &Modifiers::default());

        assert!(
            Widget::resources(&app, &doc).is_empty(),
            "the client publishes no Ctrl+K resource tree — its only shared state is native SQL text",
        );
        assert!(
            app.take_committed_action().is_none(),
            "no action is dispatched, so no action-host undo boundary is recorded",
        );
    }
}
