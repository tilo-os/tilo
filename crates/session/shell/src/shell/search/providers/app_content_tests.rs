use std::sync::Arc;

use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::FreshId as _;
use crate::shell::state::{RoomId, Workspace, WorkspaceId, WorkspaceTree};
use ui::search::index_publication::{IndexState, IndexWeight, Publisher};

const PASSWORDS: &str = "so.tilo.passwords";
const NOTES: &str = "so.tilo.notes";

fn chrome() -> (Chrome, String) {
    let mut tree = WorkspaceTree::default();
    let workspace = Workspace::in_room(WorkspaceId::new(), RoomId::new(), "Home");
    let room = workspace.room().to_string();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    (ChromeState::new(tree), room)
}

fn query(provider: &AppContentProvider, needle: &str, chrome: &Chrome) -> Vec<SearchResult> {
    let state = chrome.borrow();
    let doc = automerge::AutoCommit::new();
    provider.query(SearchContext {
        query_scope: None,
        include_programmatic: false,
        needle,
        wanted: &[ResourceKind::Record],
        chrome: &state,
        doc: &doc,
        doc_root: &automerge::ROOT,
    })
}

fn titles(rows: &[SearchResult]) -> Vec<&str> {
    rows.iter().map(|row| row.title.as_str()).collect()
}

fn note(key: &str, title: &str) -> IndexEntry {
    IndexEntry {
        key: key.into(),
        resource: key.into(),
        action: None,
        title: title.into(),
        subtitle: None,
        group: None,
        icon: None,
        boost: 0,
        fields: Vec::new(),
        secret: false,
        references: Vec::new(),
    }
}

/// A vault login as Passwords' search publishes it: its name, its
/// username and its site, secret-bearing, never the password.
fn login(key: &str, title: &str, username: &str, site: &str) -> IndexEntry {
    IndexEntry {
        group: Some("Passwords".into()),
        icon: Some("globe".into()),
        fields: vec![
            (username.into(), IndexWeight::Identity),
            (site.into(), IndexWeight::Identity),
        ],
        secret: true,
        ..note(key, title)
    }
}

/// Publish `entries` from `app`'s window in `room` the way a running app
/// does: through a [`Publisher`], encoded, over the wire's decoding.
fn publish(
    index: &AppContentIndex,
    publisher: &mut Publisher,
    room: &str,
    app: &str,
    state: IndexState,
    entries: Vec<IndexEntry>,
) {
    for delta in publisher.publish(state, entries) {
        let bytes = delta.encode().expect("a delta encodes");
        index
            .publish(room, app, "tab:0001", &bytes)
            .expect("the index takes the delta");
    }
}

fn state(generation: u64, secrets_unlocked: Option<bool>) -> IndexState {
    IndexState {
        generation,
        secrets_unlocked,
    }
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn a_closed_apps_content_is_found_and_follows_its_updates_and_removals() {
    let (chrome, room) = chrome();
    let index = Arc::new(AppContentIndex::in_memory());
    let provider = AppContentProvider::without_broker(Arc::clone(&index));
    let mut publisher = Publisher::default();

    publish(
        &index,
        &mut publisher,
        &room,
        NOTES,
        state(1, None),
        vec![note("n1", "Groceries"), note("n2", "Trip to Lisbon")],
    );
    // The app has closed: nothing runs, and its content is still found.
    let rows = query(&provider, "groceries", &chrome);
    assert_eq!(titles(&rows), ["Groceries"]);
    assert!(rows.iter().all(|row| row.kind == ResourceKind::Record));
    assert!(rows.iter().all(|row| row.provider_id == PROVIDER_ID));
    assert!(
        rows.iter()
            .all(|row| row.score > 0 && row.score <= MAX_SCORE),
        "scores stay on the provider scale the registry normalises"
    );

    publish(
        &index,
        &mut publisher,
        &room,
        NOTES,
        state(2, None),
        vec![note("n1", "Groceries for Sunday")],
    );
    assert_eq!(
        titles(&query(&provider, "groceries", &chrome)),
        ["Groceries for Sunday"],
        "an update replaces the entry"
    );
    assert!(
        query(&provider, "lisbon", &chrome).is_empty(),
        "a removed entry is gone"
    );
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn a_locked_vault_offers_one_unlock_row_and_an_unlocked_one_lists_its_logins() {
    let (chrome, room) = chrome();
    let index = Arc::new(AppContentIndex::in_memory());
    let provider = AppContentProvider::without_broker(Arc::clone(&index));
    let mut publisher = Publisher::default();
    let logins = vec![
        login("passwords.item:1", "GitHub", "ada", "github.com"),
        login(
            "passwords.item:2",
            "GitHub (work)",
            "ada-work",
            "github.com",
        ),
    ];

    publish(
        &index,
        &mut publisher,
        &room,
        PASSWORDS,
        state(1, Some(false)),
        logins.clone(),
    );
    let locked = query(&provider, "github", &chrome);
    assert_eq!(
        locked.len(),
        1,
        "one row for the locked app, whatever matched"
    );
    assert!(tilo_desktop_search::indexed::unlock_app(&locked[0].id).is_some());
    assert!(
        locked[0].title.contains("github"),
        "the row says what it would search for"
    );
    assert!(
        !locked.iter().any(|row| row.title == "GitHub"),
        "a locked vault's logins are never shown"
    );
    assert!(
        query(&provider, "lisbon", &chrome).is_empty(),
        "no unlock row for a query nothing locked matches"
    );

    publish(
        &index,
        &mut publisher,
        &room,
        PASSWORDS,
        state(2, Some(true)),
        logins,
    );
    let unlocked = query(&provider, "github", &chrome);
    assert_eq!(titles(&unlocked), ["GitHub", "GitHub (work)"]);
    assert!(
        !unlocked
            .iter()
            .any(|row| tilo_desktop_search::indexed::unlock_app(&row.id).is_some())
    );
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn an_unlocked_github_login_is_found_by_github_with_passwords_closed() {
    let (chrome, room) = chrome();
    let index = Arc::new(AppContentIndex::in_memory());
    let provider = AppContentProvider::without_broker(Arc::clone(&index));
    let mut publisher = Publisher::default();
    // The login's name does not say github; its site does.
    publish(
        &index,
        &mut publisher,
        &room,
        PASSWORDS,
        state(1, Some(true)),
        vec![login(
            "passwords.item:gh",
            "Work account",
            "ada@example.com",
            "github.com",
        )],
    );
    // Passwords closes: its window's connection is gone and nothing
    // publishes any more. The index keeps what it published.
    drop(publisher);

    let rows = query(&provider, "github", &chrome);
    assert_eq!(
        titles(&rows),
        ["Work account"],
        "\"github\" lists that login"
    );
    assert!(provider.covers(PASSWORDS, &room, "passwords.item:gh"));

    // Locked: the unlock row, and not the login.
    let mut reopened = Publisher::default();
    publish(
        &index,
        &mut reopened,
        &room,
        PASSWORDS,
        state(1, Some(false)),
        Vec::new(),
    );
    let rows = query(&provider, "github", &chrome);
    assert_eq!(rows.len(), 1);
    assert!(
        tilo_desktop_search::indexed::unlock_app(&rows[0].id).is_some(),
        "locked: the unlock row"
    );
}

#[test]
fn one_entry_published_from_two_rooms_is_one_row() {
    let (chrome, room) = chrome();
    let other_room = RoomId::new().to_string();
    let index = Arc::new(AppContentIndex::in_memory());
    let provider = AppContentProvider::without_broker(Arc::clone(&index));
    let entry = login("passwords.item:gh", "GitHub", "ada", "github.com");
    for room in [&room, &other_room] {
        publish(
            &index,
            &mut Publisher::default(),
            room,
            PASSWORDS,
            state(1, Some(true)),
            vec![entry.clone()],
        );
    }
    assert_eq!(titles(&query(&provider, "github", &chrome)), ["GitHub"]);
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn a_hit_resolves_by_its_id() {
    let (chrome, room) = chrome();
    let index = Arc::new(AppContentIndex::in_memory());
    let provider = AppContentProvider::without_broker(Arc::clone(&index));
    publish(
        &index,
        &mut Publisher::default(),
        &room,
        NOTES,
        state(1, None),
        vec![note("n1", "Groceries")],
    );
    let id = query(&provider, "groceries", &chrome)[0].id.clone();
    let state = chrome.borrow();
    let doc = automerge::AutoCommit::new();
    let resolved = provider
        .resolve(
            &id,
            SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: &[ResourceKind::Record],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            },
        )
        .expect("the hit resolves");
    assert_eq!(resolved.title, "Groceries");
}

#[test]
fn opening_prefers_the_entrys_own_action_then_the_apps_open_at_its_resource() {
    use crate::shell::collab_broker::CrdtRoute;
    let room = RoomId::new().to_string();
    let action =
        |resource: &str, action: &str, params: Vec<tilo_actions::ActionParam>| PublishedAction {
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: resource.into(),
            action: action.into(),
            resource_kind: "test".into(),
            resource_label: resource.into(),
            action_label: action.into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::View,
            params,
            contract: tilo_actions::ActionContract::default(),
            view: None,
            input: PublishedInput::None,
        };
    let surface = |live: bool, actions: Vec<PublishedAction>| PublishedSurface {
        peer_id: 7,
        app_id: NOTES.into(),
        route: CrdtRoute {
            app_id: NOTES.into(),
            cluster_id: room.clone(),
            record_id: "tab:0002".into(),
        },
        actions,
        live,
    };
    let open = action(
        "notes",
        OPEN_ACTION,
        vec![tilo_actions::ActionParam::required(
            OPEN_ANCHOR,
            tilo_actions::ParamType::Text,
            "Item to open",
        )],
    );
    let slot = SlotKey::new(NOTES, &room, "tab:0001");

    // Anchored only: the app's `open`, at the entry's resource, on the
    // retained catalogue of the closed app.
    let (chosen, invoked, argument) = opening(
        vec![surface(false, vec![open.clone()])],
        &slot,
        &note("n1", "Groceries"),
    )
    .expect("the app's open opens it");
    assert!(!chosen.live);
    assert_eq!(invoked.action, OPEN_ACTION);
    let InvokeArgument::Json(values) = argument else {
        panic!("open takes the anchor as its argument");
    };
    assert_eq!(values.get(OPEN_ANCHOR), Some(&serde_json::json!("n1")));

    // An entry naming its own action invokes that, on the running app.
    let entry = IndexEntry {
        action: Some(ui::search::index_publication::IndexAction {
            resource: "n1".into(),
            action: "show".into(),
        }),
        ..note("n1\u{1f}show", "Groceries")
    };
    let (chosen, invoked, argument) = opening(
        vec![
            surface(false, vec![open.clone()]),
            surface(true, vec![open, action("n1", "show", Vec::new())]),
        ],
        &slot,
        &entry,
    )
    .expect("the entry's action opens it");
    assert!(chosen.live, "the running app is asked first");
    assert_eq!(invoked.action, "show");
    assert!(matches!(argument, InvokeArgument::None));

    // Another room's or app's surfaces never open it.
    let elsewhere = SlotKey::new(NOTES, &RoomId::new().to_string(), "tab:0001");
    assert!(opening(vec![surface(true, Vec::new())], &elsewhere, &entry).is_none());
}

/// A reference to the resource `id` of `app`, as rich text writes one.
fn reference_to(app: &str, kind: &str, id: &str) -> String {
    tilo_reference::Embed::resource(app, kind, id).mark_value()
}

#[test]
fn a_text_is_found_by_what_it_refers_to_as_that_is_called_now() {
    const TASKS: &str = "so.tilo.tasks";
    let (chrome, room) = chrome();
    let index = Arc::new(AppContentIndex::in_memory());
    let provider = AppContentProvider::without_broker(Arc::clone(&index));
    let (mut tasks, mut notes) = (Publisher::default(), Publisher::default());
    let task = |title: &str| note("tasks.task:t1", title);
    publish(
        &index,
        &mut tasks,
        &room,
        TASKS,
        state(1, None),
        vec![task("Fix the build")],
    );
    // A note whose text refers to the task, written while it was called "Fix the build".
    let plan = IndexEntry {
        references: vec![reference_to(TASKS, "tasks.task", "tasks.task:t1")],
        ..note("notes.page:p1", "Plan for Monday")
    };
    publish(&index, &mut notes, &room, NOTES, state(1, None), vec![plan]);
    assert!(titles(&query(&provider, "fix build", &chrome)).contains(&"Plan for Monday"));

    // The task is renamed in its app; the note is found by the new name, and no longer by the
    // old one, though nothing republished the note.
    publish(
        &index,
        &mut tasks,
        &room,
        TASKS,
        state(2, None),
        vec![task("Ship the release")],
    );
    let found = titles(&query(&provider, "ship release", &chrome))
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(found.contains(&"Plan for Monday".to_owned()), "{found:?}");
    assert!(
        !titles(&query(&provider, "fix build", &chrome)).contains(&"Plan for Monday"),
        "the old name is not what it is found by"
    );
    drop(provider);

    // Which texts refer to the task: the system's backlinks, in any app.
    let links = index.backlinks(&room, TASKS, "tasks.task:t1");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].slot.app, NOTES);
    assert_eq!(links[0].entry.title, "Plan for Monday");
    assert_eq!(links[0].referenced, ["Ship the release"]);
    assert!(index.backlinks(&room, TASKS, "tasks.task:t2").is_empty());
    // Removing the note removes its backlink.
    publish(&index, &mut notes, &room, NOTES, state(2, None), Vec::new());
    assert!(index.backlinks(&room, TASKS, "tasks.task:t1").is_empty());
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
fn a_text_is_never_found_by_the_title_of_what_the_searcher_does_not_hold() {
    const BOARD: &str = "so.tilo.lang.board";
    let task = "tl:so.tilo.lang.board:plans/tasks:t1";
    let reference = reference_to(BOARD, "tl.so.tilo.lang.board.Task", task);
    // Two people's sessions, each indexing what their own apps publish: Ada holds the task, so
    // her app publishes it and reads the post's reference by its title; Bea does not, so hers
    // publishes no task and reads the reference as not shared.
    let (chrome, room) = chrome();
    let post = |text: &str| IndexEntry {
        references: vec![reference.clone()],
        ..note("tl:so.tilo.lang.board:board/posts:p1", text)
    };
    let ada = Arc::new(AppContentIndex::in_memory());
    let bea = Arc::new(AppContentIndex::in_memory());
    let (mut ada_board, mut bea_board) = (Publisher::default(), Publisher::default());
    let ada_has = |task_title: &str| {
        vec![
            note(task, task_title),
            post(&format!("The notes are in {task_title}")),
        ]
    };
    publish(
        &ada,
        &mut ada_board,
        &room,
        BOARD,
        state(1, None),
        ada_has("Acquisition plan"),
    );
    publish(
        &bea,
        &mut bea_board,
        &room,
        BOARD,
        state(1, None),
        vec![post("The notes are in Not shared with you")],
    );
    let ada_finds = AppContentProvider::without_broker(Arc::clone(&ada));
    let bea_finds = AppContentProvider::without_broker(Arc::clone(&bea));
    assert!(
        titles(&query(&ada_finds, "acquisition", &chrome))
            .contains(&"The notes are in Acquisition plan")
    );
    let bea_found = query(&bea_finds, "acquisition", &chrome);
    assert!(bea_found.is_empty(), "{:?}", titles(&bea_found));
    let links = bea.backlinks(&room, BOARD, task);
    assert_eq!(links.len(), 1, "the post still refers to the task");
    assert!(
        links[0].referenced.is_empty(),
        "and names no title of it she cannot read"
    );

    // Renamed in Ada's app: she finds the post by the new name; Bea by nothing of it.
    publish(
        &ada,
        &mut ada_board,
        &room,
        BOARD,
        state(2, None),
        ada_has("Merger timeline"),
    );
    assert!(
        titles(&query(&ada_finds, "merger", &chrome)).contains(&"The notes are in Merger timeline")
    );
    assert_eq!(
        ada.backlinks(&room, BOARD, task)[0].referenced,
        ["Merger timeline"]
    );
    assert!(query(&bea_finds, "merger", &chrome).is_empty());
    assert!(query(&bea_finds, "acquisition", &chrome).is_empty());
}
