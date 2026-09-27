//! The exported action that writes into an explicitly addressed text document.
//!
//! Lands in the focused tab's **own** record document, reached through
//! [`crate::shell::tab_documents`] — not the shell's document, or two Notes
//! tabs would show each other's text. Splices via [`CrdtTextBody::splice`]
//! rather than read-concatenate-write-back, which costs O(len) ops and
//! O(len²) tombstones per session (see [`crate::shell::crdt_text`]). `at` is
//! a character offset, not a byte offset — a caller driving this by
//! address cannot see the buffer.

use crate::shell::chrome_state::ChromeState;
use crate::shell::crdt_text::CrdtTextBody;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::providers::text_app::tile_hosts_app;
use crate::shell::state::WorkspaceAuthority as _;
use ui::record::RecordId;

/// Stable address of this action within the `actions` provider.
pub const ACTION_ID: &str = "text.insert";

pub const PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "resource",
        ParamType::Resource,
        "The stable tab record address returned by discovery (for example `tab:000000000000004d`).",
    ),
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

/// Which app, and the Automerge key its body lives under.
const TEXT_TILES: &[(&str, &str)] = &[
    ("Notes", crate::shell::notes_app::BODY_KEY),
    ("Editor", crate::shell::editor_app::BODY_KEY),
];

/// Resolve an exact record address to the tab and body key that own it.
/// Focus is deliberately irrelevant: it may change after discovery.
struct AddressedTextTab {
    tab_id: u64,
    body_key: &'static str,
    app_id: String,
    app_label: &'static str,
    user: crate::shell::state::UserId,
}

fn addressed_text_tab(st: &ChromeState, record: &RecordId) -> Option<AddressedTextTab> {
    let tree = st.tree();
    // Found (or not) while the read lock is held; the lock is released
    // below, before this returns to the caller.
    let found = 'search: {
        for w in &tree.desktop.workspaces {
            for s in &w.strips {
                for g in &s.window_groups {
                    for t in &g.tabs {
                        if RecordId::for_tab(t.id) != *record {
                            continue;
                        }
                        let (app_label, body_key) = TEXT_TILES
                            .iter()
                            .find(|(app, _)| tile_hosts_app(&t.tile, app))
                            .copied()?;
                        let app_id = match &t.tile {
                            crate::shell::state::TileContent::AppPlaceholder { kind, .. }
                            | crate::shell::state::TileContent::Stub { kind, .. } => kind.app_id(),
                            _ => break 'search None,
                        };
                        let user = st.local_user_for_room(&w.room);
                        let actor = w.actor_for_person(&user);
                        let resource = crate::shell::state::ResourceRef::Artifact {
                            app: Some(app_id.clone()),
                            kind: collab_types::ArtifactKind::Note,
                            instance: collab_types::document_artifact_id(
                                &app_id,
                                &w.room.to_string(),
                                record.as_str(),
                            ),
                        };
                        if !w.allows(&actor, &resource, crate::shell::state::Verb::Write) {
                            break 'search None;
                        }
                        break 'search Some(AddressedTextTab {
                            tab_id: t.id,
                            body_key,
                            app_id,
                            app_label,
                            user,
                        });
                    }
                }
            }
        }
        None
    };
    drop(tree);
    found
}

/// Insert `text` into the explicitly addressed text tile's document.
pub fn apply(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    // `bind` already proved this is present and is text, so its absence
    // here would be a bug in the binding rather than a caller error.
    let text = args
        .text("text")
        .ok_or_else(|| ActionError::Refused("the bound arguments carry no `text`".to_string()))?;
    let address = args.text("resource").ok_or_else(|| {
        ActionError::Refused("the bound arguments carry no `resource`".to_string())
    })?;
    let record = RecordId::parse(address)
        .ok_or_else(|| ActionError::Refused("the target resource address is invalid".into()))?;
    let Some(target) = addressed_text_tab(st, &record) else {
        return Err(ActionError::Refused(
            "the addressed resource is not a text document, or is stale or inaccessible"
                .to_string(),
        ));
    };

    let store = st.records().clone();
    if store.read(&record, |_| ()).is_none() {
        return Err(ActionError::Refused(format!(
            "the addressed document {record} is not available to this session"
        )));
    }

    let context = tilo_actions::ActionContext::for_resource(
        "Insert text",
        record.as_str(),
        "text.document",
        format!("{} document", target.app_label),
    )
    .with_app(&target.app_id, target.app_label);
    let spliced = store.with_owned_action(
        &record,
        target.tab_id,
        &target.app_id,
        ui::permissions::UserId(target.user.clone()),
        context,
        |doc, root| {
            // `sync` fills the mirror from the CRDT, the source of truth; this
            // body is a throwaway view onto it for one edit.
            let mut body = CrdtTextBody::new(target.body_key);
            body.sync(doc, root);

            let char_len = body.text.chars().count();
            let at_chars = match args.integer("at") {
                Some(n) if n < 0 => 0,
                Some(n) => usize::try_from(n).unwrap_or(usize::MAX).min(char_len),
                None => char_len,
            };
            let at_bytes = body.char_index_to_byte(at_chars);
            body.splice(doc, root, at_bytes..at_bytes, text);
            body.text.chars().count()
        },
    );

    match spliced {
        Ok(_) => Ok(()),
        Err(_) => Err(ActionError::Refused(format!(
            "the document for tab {} could not be opened",
            target.tab_id
        ))),
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::Chrome;
    use crate::shell::notes_app::BODY_KEY;
    use crate::shell::search::action_api::ArgValue;
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
    };
    use crate::shell::tab_documents::tab_body;
    use automerge::ReadDoc as _;
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
        ws.set_owner(FIXTURE_USER);
        ws.strips = vec![strip];
        let mut active_strip = StdMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut active_group = StdMap::new();
        active_group.insert(strip_id, group_id);
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree {
            desktop: crate::shell::state::DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                active_strip,
                active_group,
                ..Default::default()
            },
            ..WorkspaceTree::default()
        }));
        // An explicit identity: the session registry lives under the user's
        // configuration directory, which is not this test's to read.
        let chrome = crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
            handle,
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );
        let store = RecordStore::in_memory();
        store.open_record(&crate::shell::tab_documents::scratch_tab_record(TAB));
        chrome.borrow_mut().set_records(store.clone());
        (chrome, store)
    }

    /// The principal a fixture chrome speaks for.
    const FIXTURE_USER: &str = "shell-fixture";

    fn args(mut pairs: Vec<(&str, ArgValue)>) -> ActionArgs {
        if !pairs.iter().any(|(name, _)| *name == "resource") {
            pairs.push((
                "resource",
                ArgValue::Resource(RecordId::for_tab(TAB).to_string()),
            ));
        }
        ActionArgs::bind(PARAMS, pairs.into_iter().map(|(k, v)| (k.to_string(), v)))
            .expect("the test's arguments must satisfy the declared parameters")
    }

    fn insert(chrome: &Chrome, a: &ActionArgs) -> Result<(), ActionError> {
        apply(&mut chrome.borrow_mut(), a)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn text_insert_writes_only_the_addressed_live_text_document() {
        // Phase `the_insert_lands_in_the_focused_tabs_document`.
        // Asserted through `tab_body`, the same read the shell's own search
        // does, so the text can't be "written" somewhere nothing reads.
        {
            let (chrome, store) = chrome_focused_on("Notes");
            let before_history = store
                .read_history(&RecordId::for_tab(TAB), |_, history| history.revision())
                .expect("open record history");
            insert(
                &chrome,
                &args(vec![("text", ArgValue::Text("hello".into()))]),
            )
            .expect("inserting into a focused Notes tab must work");
            assert_eq!(tab_body(&store, TAB, BODY_KEY).as_deref(), Some("hello"));
            assert_eq!(
                store
                    .read_history(&RecordId::for_tab(TAB), |_, history| history.revision())
                    .expect("open record history"),
                before_history + 1,
                "external text insertion must create one owned history entry"
            );
        }

        // Phase `an_editor_tab_is_written_under_its_own_body_key`.
        // Same machinery under a different body key, not Notes'.
        {
            let (chrome, store) = chrome_focused_on("Editor");
            insert(
                &chrome,
                &args(vec![("text", ArgValue::Text("fn main".into()))]),
            )
            .unwrap();
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

        // Phase `a_tab_that_is_not_a_text_document_is_refused`.
        // Refuses and says why, rather than writing into some other tab.
        {
            let (chrome, store) = chrome_focused_on("Terminal");
            let err = insert(&chrome, &args(vec![("text", ArgValue::Text("hi".into()))]))
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

        // Phase `focus_changes_do_not_redirect_an_addressed_edit`.
        {
            const OTHER_TAB: u64 = 88;
            let (chrome, store) = chrome_focused_on("Notes");
            {
                let state = chrome.borrow_mut();
                let mut tree = state.tree_mut();
                let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
                group.tabs.push(Tab {
                    id: OTHER_TAB,
                    title: "Other note".into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other("Notes".into()),
                        label: "Notes".into(),
                    },
                });
                group.active_tab = OTHER_TAB;
                drop(tree);
            }
            store.open_record(&crate::shell::tab_documents::scratch_tab_record(OTHER_TAB));

            insert(
                &chrome,
                &args(vec![("text", ArgValue::Text("targeted".into()))]),
            )
            .expect("the original explicit target remains valid after focus moves");

            assert_eq!(tab_body(&store, TAB, BODY_KEY).as_deref(), Some("targeted"));
            assert_eq!(
                tab_body(&store, OTHER_TAB, BODY_KEY).as_deref(),
                None,
                "ambient focus must not receive the external edit"
            );
        }

        // Phase `stale_and_unavailable_targets_mutate_no_document`.
        {
            let (chrome, store) = chrome_focused_on("Notes");
            let before = tab_body(&store, TAB, BODY_KEY);
            for target in [RecordId::for_tab(999).to_string(), "file:/not/a/tab".into()] {
                let error = insert(
                    &chrome,
                    &args(vec![
                        ("resource", ArgValue::Resource(target)),
                        ("text", ArgValue::Text("must not land".into())),
                    ]),
                )
                .expect_err("invalid target must be refused");
                assert!(matches!(error, ActionError::Refused(_)));
            }
            assert_eq!(tab_body(&store, TAB, BODY_KEY), before);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn text_insert_grants_authorize_only_their_canonical_target() {
        // Phase `canonical_record_grant_uses_owned_action_provenance`.
        {
            let (chrome, store) = chrome_focused_on("Notes");
            let record = RecordId::for_tab(TAB);
            let before_generation = store
                .action_generation(&record)
                .expect("record action generation")
                .expect("tracked generation");
            let (user, app_id) = {
                let state = chrome.borrow();
                let tree = state.tree();
                let room = tree.desktop.workspaces[0].room;
                drop(tree);
                (
                    state.local_user_for_room(&room),
                    AppKind::Other("Notes".into()).app_id(),
                )
            };
            {
                let state = chrome.borrow();
                let mut tree = state.tree_mut();
                let workspace = &mut tree.desktop.workspaces[0];
                let instance = collab_types::document_artifact_id(
                    &app_id,
                    &workspace.room.to_string(),
                    record.as_str(),
                );
                workspace.owner = None;
                workspace.capabilities.clear();
                workspace.grant_capability(crate::shell::state::CapOverride::grant(
                    crate::shell::state::Subject::person(user.clone()),
                    crate::shell::state::Scope::ResourceInstance(instance),
                    crate::shell::state::Verb::Write,
                ));
                drop(tree);
            }

            insert(
                &chrome,
                &args(vec![("text", ArgValue::Text("owned".into()))]),
            )
            .expect("the canonical resource-instance grant authorizes its record");

            assert_eq!(tab_body(&store, TAB, BODY_KEY).as_deref(), Some("owned"));
            assert_eq!(
                store.action_generation(&record).unwrap(),
                Some(before_generation + 1),
                "external dispatch crosses one record-owned action boundary"
            );
            store
                .read_history(&record, |doc, history| {
                    let entry = history.peek_last_undo().expect("owned undo entry");
                    assert_eq!(
                        entry.author.as_ref().map(|author| author.0.as_str()),
                        Some(user.as_str())
                    );
                    assert_eq!(entry.instance_id, Some(TAB));
                    let context = entry
                        .after
                        .iter()
                        .filter(|hash| !entry.before.contains(hash))
                        .filter_map(|hash| doc.get_change_by_hash(hash))
                        .find_map(|change| change.message().map(str::to_owned))
                        .and_then(|message| {
                            tilo_actions::ActionContext::from_change_message(&message)
                        })
                        .expect("semantic action provenance");
                    assert_eq!(context.action_label, "Insert text");
                    assert_eq!(
                        context.app.as_ref().map(|app| app.id.as_str()),
                        Some(app_id.as_str())
                    );
                    assert_eq!(
                        context
                            .resource
                            .as_ref()
                            .map(|resource| resource.id.as_str()),
                        Some(record.as_str())
                    );
                })
                .expect("record history");
        }

        // Phase `canonical_app_grant_authorizes_but_invented_app_id_does_not`.
        {
            for (granted_app, should_allow) in
                [("org.tilo.Editor", true), ("org.example.Editor", false)]
            {
                let (chrome, store) = chrome_focused_on("Editor");
                let user = {
                    let state = chrome.borrow();
                    let tree = state.tree();
                    state.local_user_for_room(&tree.desktop.workspaces[0].room)
                };
                {
                    let state = chrome.borrow();
                    let mut tree = state.tree_mut();
                    let workspace = &mut tree.desktop.workspaces[0];
                    workspace.owner = None;
                    workspace.capabilities.clear();
                    workspace.grant_capability(crate::shell::state::CapOverride::grant(
                        crate::shell::state::Subject::person(user),
                        crate::shell::state::Scope::App(granted_app.into()),
                        crate::shell::state::Verb::Write,
                    ));
                    drop(tree);
                }
                let outcome = insert(
                    &chrome,
                    &args(vec![("text", ArgValue::Text("canonical".into()))]),
                );
                assert_eq!(outcome.is_ok(), should_allow, "grant for {granted_app}");
                assert_eq!(
                    tab_body(&store, TAB, crate::shell::editor_app::BODY_KEY).is_some(),
                    should_allow
                );
            }
        }

        // Phase `an_active_workspace_grant_does_not_authorize_another_workspaces_record`.
        {
            let (chrome, store) = chrome_focused_on("Notes");
            {
                let state = chrome.borrow_mut();
                let mut tree = state.tree_mut();
                tree.desktop.workspaces[0].set_owner("another-person");
                let mut active = Workspace::new(WorkspaceId::new(), "Authorized elsewhere");
                active.set_owner(FIXTURE_USER);
                let active_id = active.id;
                tree.desktop.workspaces.push(active);
                tree.set_active(active_id);
            }

            let error = insert(
                &chrome,
                &args(vec![(
                    "text",
                    ArgValue::Text("must not cross rooms".into()),
                )]),
            )
            .expect_err("target workspace authority is independent of the active workspace");
            assert!(matches!(error, ActionError::Refused(_)));
            assert_eq!(tab_body(&store, TAB, BODY_KEY), None);
            assert_eq!(
                store
                    .read_history(&RecordId::for_tab(TAB), |_, history| history.revision())
                    .expect("open record history"),
                0,
                "denied targeting must not create an attribution entry"
            );
        }
    }

    #[test]
    fn insert_offsets_are_clamped_character_offsets_costing_one_element_each() {
        // Phase `the_offset_is_a_character_offset_and_defaults_to_the_end`.
        // Omitting `at` appends; supplying it inserts in **characters**, not
        // UTF-8 bytes.
        {
            let (chrome, store) = chrome_focused_on("Notes");
            insert(&chrome, &args(vec![("text", ArgValue::Text("aβc".into()))])).unwrap();
            // β is two bytes; character 2 must land between β and c.
            insert(
                &chrome,
                &args(vec![
                    ("text", ArgValue::Text("X".into())),
                    ("at", ArgValue::Integer(2)),
                ]),
            )
            .unwrap();
            assert_eq!(tab_body(&store, TAB, BODY_KEY).as_deref(), Some("aβXc"));

            // No `at` appends to the end of what is there now.
            insert(&chrome, &args(vec![("text", ArgValue::Text("!".into()))])).unwrap();
            assert_eq!(tab_body(&store, TAB, BODY_KEY).as_deref(), Some("aβXc!"));
        }

        // Phase `an_out_of_range_offset_is_clamped_into_the_buffer`.
        // Clamped rather than refused or panicking.
        {
            let (chrome, store) = chrome_focused_on("Notes");
            insert(&chrome, &args(vec![("text", ArgValue::Text("abc".into()))])).unwrap();
            insert(
                &chrome,
                &args(vec![
                    ("text", ArgValue::Text("Z".into())),
                    ("at", ArgValue::Integer(999)),
                ]),
            )
            .unwrap();
            insert(
                &chrome,
                &args(vec![
                    ("text", ArgValue::Text("A".into())),
                    ("at", ArgValue::Integer(-5)),
                ]),
            )
            .unwrap();
            assert_eq!(tab_body(&store, TAB, BODY_KEY).as_deref(), Some("AabcZ"));
        }

        // Phase `an_insert_costs_one_element_per_character_not_a_rewrite`.
        // One inserted character costs one Automerge element, not a document
        // rewrite — proven by counting `Text` elements, since the resulting
        // string is identical either way.
        {
            use automerge::{ObjType, ReadDoc, Value};

            let (chrome, store) = chrome_focused_on("Notes");
            insert(
                &chrome,
                &args(vec![("text", ArgValue::Text("x".repeat(64)))]),
            )
            .unwrap();

            let record = RecordId::for_tab(TAB);
            let before = store
                .read(&record, |doc| {
                    let (_, id) = doc.get(&automerge::ROOT, BODY_KEY).unwrap().unwrap();
                    doc.length(&id)
                })
                .unwrap();

            insert(&chrome, &args(vec![("text", ArgValue::Text("y".into()))])).unwrap();

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
    }
}
