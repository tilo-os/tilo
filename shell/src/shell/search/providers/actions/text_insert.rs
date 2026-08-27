//! The exported action that writes into the focused text document.
//!
//! Every other row this provider exports moves chrome around a document;
//! this one changes the document itself, so it has to be *told* the
//! text — it arrives as a bound
//! [`ActionArgs`](crate::shell::search::action_api::ActionArgs).
//!
//! Lands in the focused tab's **own** record document, reached through
//! [`crate::shell::tab_documents`] — not the shell's document, which is
//! the mistake that once made two Notes tabs show each other's text.
//!
//! Splices via [`CrdtTextBody::splice`] rather than read-concatenate-
//! write-back: a whole-body write costs O(len) ops and O(len²)
//! tombstones per session, and converges badly (two peers appending at
//! once clobber each other, where two narrow splices merge). See
//! [`crate::shell::crdt_text`].
//!
//! `at` is a character offset, not a byte offset — a caller driving
//! this by address cannot see the buffer, so byte offsets would make
//! "insert at position 5" a coin flip on any document with an accent.

use crate::shell::chrome_state::ChromeState;
use crate::shell::crdt_text::CrdtTextBody;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::providers::text_app::tile_hosts_app;
use ui::record::RecordId;

/// Stable address of this action within the `actions` provider.
pub const ACTION_ID: &str = "text.insert";

pub const PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "text",
        ParamType::Text,
        "The text to insert. Inserted verbatim; no newline is added.",
    ),
    ActionParam::optional(
        "at",
        ParamType::Integer,
        "Character offset to insert at. Defaults to the end of the \
         document. Negative values, and values past the end, are clamped \
         into the buffer.",
    ),
];

/// One text tile the shell hosts: which app, and the Automerge key its
/// body lives under.
const TEXT_TILES: &[(&str, &str)] = &[
    ("Notes", crate::shell::notes_app::BODY_KEY),
    ("Editor", crate::shell::editor_app::BODY_KEY),
];

/// The focused tab, if it hosts one of the shell's text apps.
///
/// Returns the tab id and the body key its app writes under. `None` when
/// nothing is focused, or when the focused tab is a terminal / file
/// explorer / foreign surface — there is no text document to write into,
/// and inventing one somewhere else would be worse than refusing.
fn focused_text_tab(st: &ChromeState) -> Option<(u64, &'static str)> {
    let tab_id = st.active_tab_id()?;
    let tree = st.tree();
    for w in &tree.workspaces {
        for s in &w.strips {
            for g in &s.window_groups {
                for t in &g.tabs {
                    if t.id != tab_id {
                        continue;
                    }
                    return TEXT_TILES
                        .iter()
                        .find(|(app, _)| tile_hosts_app(&t.tile, app))
                        .map(|(_, key)| (t.id, *key));
                }
            }
        }
    }
    None
}

/// Insert `text` into the focused text tile's document.
pub fn apply(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    // `bind` already proved this is present and is text, so its absence
    // here would be a bug in the binding rather than a caller error.
    let text = args.text("text").ok_or_else(|| {
        ActionError::Refused("the bound arguments carry no `text`".to_string())
    })?;
    let Some((tab_id, body_key)) = focused_text_tab(st) else {
        return Err(ActionError::Refused(
            "the focused tab is not a text document; focus a Notes or Editor \
             tab first (its address is enumerated under the `notes` or \
             `editor` provider)"
                .to_string(),
        ));
    };

    let record = RecordId::for_tab(tab_id);
    let store = st.records().clone();
    // The tile whose document this is has it open already; a tab that has
    // never been rendered has not. Opening is idempotent and is what makes
    // the action work on a session that has not painted yet.
    store.open_record(&crate::shell::tab_documents::scratch_tab_record(tab_id));

    let spliced = store.with(&record, |doc| {
        let root = automerge::ROOT;
        // `sync` fills the mirror from the CRDT, which is what makes the
        // offset arithmetic below refer to the text that is actually
        // there rather than to an empty buffer. The CRDT is the source of
        // truth; this body is a throwaway view onto it for one edit.
        let mut body = CrdtTextBody::new(body_key);
        body.sync(doc, &root);

        let char_len = body.text.chars().count();
        let at_chars = match args.integer("at") {
            Some(n) if n < 0 => 0,
            Some(n) => (n as usize).min(char_len),
            None => char_len,
        };
        let at_bytes = body.char_index_to_byte(at_chars);
        body.splice(doc, &root, at_bytes..at_bytes, text);
        body.text.chars().count()
    });

    match spliced {
        Some(_) => Ok(()),
        // `with` returns `None` only when the record is not open, and we
        // just opened it — so this is a store that refused, not a missing
        // tab. Reported rather than silently dropped: an edit that did not
        // happen must not answer like one that did.
        None => Err(ActionError::Refused(format!(
            "the document for tab {tab_id} could not be opened"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::Chrome;
    use crate::shell::notes_app::BODY_KEY;
    use crate::shell::search::action_api::ArgValue;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
    };
    use crate::shell::tab_documents::tab_body;
    use parking_lot::RwLock;
    use std::collections::HashMap as StdMap;
    use std::sync::Arc;
    use ui::record::RecordStore;

    const TAB: u64 = 77;

    /// A session whose focused tab hosts `app`.
    fn chrome_focused_on(app: &str) -> (Chrome, RecordStore) {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id: GroupId = 4242;
        let strip = Strip {
            id: strip_id,
            name: "Solo".into(),
            window_groups: vec![StateGroup {
                id: group_id,
                tabs: vec![Tab {
                    id: TAB,
                    title: app.into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other(app.into()),
                        label: app.into(),
                    },
                }],
                active_tab: TAB,
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
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            ..WorkspaceTree::default()
        }));
        let chrome = crate::shell::chrome_state::ChromeState::new_with_handle(handle);
        let store = RecordStore::in_memory();
        chrome.borrow_mut().set_records(store.clone());
        (chrome, store)
    }

    fn args(pairs: Vec<(&str, ArgValue)>) -> ActionArgs {
        ActionArgs::bind(
            PARAMS,
            pairs.into_iter().map(|(k, v)| (k.to_string(), v)),
        )
        .expect("the test's arguments must satisfy the declared parameters")
    }

    fn insert(chrome: &Chrome, a: ActionArgs) -> Result<(), ActionError> {
        apply(&mut chrome.borrow_mut(), &a)
    }

    /// The edit lands in the **focused tab's own document**, which is
    /// where the app that renders it looks. Asserted through
    /// `tab_body` — the same read the shell's own search does — so the
    /// text cannot be "written" somewhere nothing reads.
    #[test]
    fn the_insert_lands_in_the_focused_tabs_document() {
        let (chrome, store) = chrome_focused_on("Notes");
        insert(&chrome, args(vec![("text", ArgValue::Text("hello".into()))]))
            .expect("inserting into a focused Notes tab must work");
        assert_eq!(tab_body(&store, TAB, BODY_KEY).as_deref(), Some("hello"));
    }

    /// Omitting `at` appends. Supplying it inserts there, in
    /// **characters** — a caller that cannot see the buffer must not
    /// have to reason about UTF-8 widths.
    #[test]
    fn the_offset_is_a_character_offset_and_defaults_to_the_end() {
        let (chrome, store) = chrome_focused_on("Notes");
        insert(&chrome, args(vec![("text", ArgValue::Text("aβc".into()))])).unwrap();
        // β is two bytes; inserting at character 2 must land between β
        // and c, not in the middle of β.
        insert(
            &chrome,
            args(vec![
                ("text", ArgValue::Text("X".into())),
                ("at", ArgValue::Integer(2)),
            ]),
        )
        .unwrap();
        assert_eq!(tab_body(&store, TAB, BODY_KEY).as_deref(), Some("aβXc"));

        // No `at` appends to the end of what is there now.
        insert(&chrome, args(vec![("text", ArgValue::Text("!".into()))])).unwrap();
        assert_eq!(tab_body(&store, TAB, BODY_KEY).as_deref(), Some("aβXc!"));
    }

    /// An offset outside the buffer is clamped rather than refused or
    /// panicking. A caller reasoning about a document it cannot see will
    /// sometimes be a character behind.
    #[test]
    fn an_out_of_range_offset_is_clamped_into_the_buffer() {
        let (chrome, store) = chrome_focused_on("Notes");
        insert(&chrome, args(vec![("text", ArgValue::Text("abc".into()))])).unwrap();
        insert(
            &chrome,
            args(vec![
                ("text", ArgValue::Text("Z".into())),
                ("at", ArgValue::Integer(999)),
            ]),
        )
        .unwrap();
        insert(
            &chrome,
            args(vec![
                ("text", ArgValue::Text("A".into())),
                ("at", ArgValue::Integer(-5)),
            ]),
        )
        .unwrap();
        assert_eq!(tab_body(&store, TAB, BODY_KEY).as_deref(), Some("AabcZ"));
    }

    /// The whole point of routing through `CrdtTextBody::splice`: one
    /// inserted character costs one Automerge element, not a rewrite of
    /// the document.
    ///
    /// Proven by counting elements in the `Text` object rather than by
    /// reading the string back — the string is identical either way, and
    /// the difference between a splice and a whole-body rewrite is
    /// exactly the thing a string comparison cannot see.
    #[test]
    fn an_insert_costs_one_element_per_character_not_a_rewrite() {
        use automerge::{ObjType, ReadDoc, Value};

        let (chrome, store) = chrome_focused_on("Notes");
        insert(
            &chrome,
            args(vec![("text", ArgValue::Text("x".repeat(64)))]),
        )
        .unwrap();

        let record = RecordId::for_tab(TAB);
        let before = store
            .read(&record, |doc| {
                let (_, id) = doc.get(&automerge::ROOT, BODY_KEY).unwrap().unwrap();
                doc.length(&id)
            })
            .unwrap();

        insert(&chrome, args(vec![("text", ArgValue::Text("y".into()))])).unwrap();

        let after = store
            .read(&record, |doc| {
                let (val, id) = doc.get(&automerge::ROOT, BODY_KEY).unwrap().unwrap();
                assert!(matches!(val, Value::Object(ObjType::Text)));
                doc.length(&id)
            })
            .unwrap();
        assert_eq!(
            after,
            before + 1,
            "a one-character insert must add exactly one element; a \
             whole-body rewrite would add {before}",
        );
    }

    /// Editor tabs are the same machinery under a different body key, so
    /// the action reaches them too — and writes under *their* key, not
    /// Notes'.
    #[test]
    fn an_editor_tab_is_written_under_its_own_body_key() {
        let (chrome, store) = chrome_focused_on("Editor");
        insert(&chrome, args(vec![("text", ArgValue::Text("fn main".into()))])).unwrap();
        assert_eq!(
            tab_body(&store, TAB, crate::shell::editor_app::BODY_KEY).as_deref(),
            Some("fn main"),
        );
        assert_eq!(
            tab_body(&store, TAB, BODY_KEY),
            None,
            "an Editor tab must not be written under the Notes key",
        );
    }

    /// A tab with no text document refuses, and says why. Writing into
    /// some other tab because the focused one had nowhere to put it
    /// would be worse than doing nothing: the caller would be told the
    /// edit landed, and it would have landed somewhere it did not ask
    /// for.
    #[test]
    fn a_tab_that_is_not_a_text_document_is_refused() {
        let (chrome, store) = chrome_focused_on("Terminal");
        let err = insert(&chrome, args(vec![("text", ArgValue::Text("hi".into()))]))
            .expect_err("a terminal has no text document to write into");
        assert!(
            matches!(&err, ActionError::Refused(m) if m.contains("not a text document")),
            "got {err:?}",
        );
        assert_eq!(
            tab_body(&store, TAB, BODY_KEY),
            None,
            "a refusal must leave every document untouched",
        );
    }
}
