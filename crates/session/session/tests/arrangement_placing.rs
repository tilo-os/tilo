//! Showing a resource is an action on the person's arrangement, never on the
//! resource (shell.md §8, "The canvas composes views; it opens nothing").
//!
//! An arrangement entry names a resource and a view: the `place` action puts a
//! resource on the canvas as one of its published views at a point, and the
//! entry's `shows` follows from the resource's app. Text typed on the canvas
//! runs the one `find` the palette and an agent's `tilo` tool use, and the top
//! result becomes such an entry at the caret, through the same action.
//!
//! Each test here fails before that: an entry starts from `shows`, an app id,
//! and nothing places a resource by its views, or by what a search finds.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

mod common;

use std::rc::Rc;
use std::time::Duration;

use tilo_actions::{ActionAddress, OperationKind, ResourceSpec, ResourceViewSpec};
use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};
use tilo_surface_host::Published;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// The person at this device, by their root identity (hex).
const ADA: &str = "abababababababababababababababababababababababababababababababab";

/// A tilolang-style app resource: a mailbox shown through an `inbox` view (primary) and a `reader`
/// view, published under `app` . Its `open` is a view operation, as every resource's opener is, and
/// it carries no action that shows it: showing is the arrangement's.
fn view_action(id: &'static str) -> tilo_actions::ActionSpec {
    tilo_actions::ActionSpec::typed_operation(
        id,
        id.to_owned(),
        String::new(),
        OperationKind::View,
        "test.mail",
        Vec::new(),
        move |_| {
            tilo_actions::EncodedAction::encode_operation("test.mail", &id)
                .expect("a view action always serializes")
        },
    )
}

fn mailbox() -> ResourceSpec {
    ResourceSpec::new("tl:so.tilo.lang.mail:inbox", "mail.mailbox", "Inbox")
        .with_action(view_action("open-inbox"))
        .with_action(view_action("open-reader"))
        .with_view(ResourceViewSpec::new("inbox", "Inbox", "open-inbox").with_primary(true))
        .with_view(ResourceViewSpec::new("reader", "Reader", "open-reader"))
}

/// The app and its mailbox, published into the session's door; it records every action it is asked
/// to run, so a test can say showing ran none.
struct Mailbox {
    app: String,
    asked: Rc<std::cell::RefCell<Vec<String>>>,
}

impl Published for Mailbox {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        let mut app = ResourceSpec::new(format!("tl:{}", self.app), "mail.app", "Mail")
            .with_action(tilo_actions::ActionSpec::typed_operation(
                "search",
                "Search the mailbox".to_owned(),
                String::new(),
                OperationKind::Effect,
                "test.mail",
                Vec::new(),
                |_| {
                    tilo_actions::EncodedAction::encode_operation("test.mail", &"search")
                        .expect("the action always serializes")
                },
            ));
        app.app_id = Some(self.app.clone());
        let mut mailbox = mailbox();
        mailbox.app_id = Some(self.app.clone());
        vec![app.with_child(mailbox)]
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        _: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        self.asked.borrow_mut().push(address.action.clone());
        Ok(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

fn session() -> Session {
    crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            person: ADA.to_owned(),
            state: None,
            collab: false,
            ..Options::default()
        },
    )
}

fn step(session: &mut Session) {
    session.step(Duration::from_millis(1)).expect("a frame");
}

/// Run a frame until `done` , so what a change asked of the session lands.
fn settle(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    common::wait_for(session, "the change to land", |session| {
        done(session).then_some(())
    });
}

/// The arrangement the session's desk holds now, as a shell reads it.
fn arrangement(session: &Session) -> tilo_session::arrangement::Reading {
    session.arrangement().borrow().reading()
}

/// The place of the arrangement `key` names.
fn place_of(session: &Session, key: u64) -> Option<tilo_session::arrangement::PlaceReading> {
    let key = key.to_string();
    arrangement(session)
        .places
        .into_iter()
        .find(|place| place.key == key)
}

/// The strip of the arrangement `key` names.
fn strip_of(session: &Session, key: u64) -> Option<tilo_session::arrangement::StripReading> {
    let key = key.to_string();
    arrangement(session)
        .strips
        .into_iter()
        .find(|strip| strip.key == key)
}

/// An id as the arrangement reads it out: its whole number.
fn number(key: &str) -> u64 {
    key.parse().expect("an id reads as its number")
}

/// The row of the workspace in front, as the canvas place action names it.
fn front_row(session: &Session) -> u64 {
    let tree = session.handle().read(Clone::clone);
    let workspace = tree.active_workspace().expect("a workspace is in front");
    workspace.strips.first().expect("a row").id.0
}

/// An app with a mailbox is published into the session's door.
fn with_mailbox(session: &mut Session) -> (String, Rc<std::cell::RefCell<Vec<String>>>) {
    let app = "so.tilo.lang.mail".to_owned();
    let asked = Rc::new(std::cell::RefCell::new(Vec::new()));
    session.publish(Rc::new(std::cell::RefCell::new(Mailbox {
        app: app.clone(),
        asked: Rc::clone(&asked),
    })));
    // What the session publishes reaches the arrangement's catalogue and the search on the first
    // frame after it.
    step(session);
    step(session);
    (app, asked)
}

// (1) The arranging action places a resource as one of its views at a point: the entry names the
// resource and the view, `shows` follows from the resource's app, and the change is the person's
// one undoable step.
#[test]
fn a_resource_a_view_and_a_point_make_an_arrangement_entry() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let (app, _) = with_mailbox(&mut session);
    let row = front_row(&session);
    let (workspace, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        (
            tree.active_workspace()
                .expect("workspace")
                .slot
                .0
                .to_string(),
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };

    session
        .invoke(
            "shell:arrangement",
            "place",
            serde_json::json!({
                "workspace": workspace,
                "row": row,
                "strip": strip,
                "place": place,
                "resource": "tl:so.tilo.lang.mail:inbox",
                "view": "reader",
                "x": 240,
                "y": 160,
                "width": 720,
                "height": 520,
            })
            .as_object()
            .expect("arguments"),
        )
        .expect("the arranging action runs");
    settle(&mut session, |session| place_of(session, place).is_some());

    let entry = place_of(&session, place).expect("the entry is in the record");
    assert_eq!(
        entry.record, "tl:so.tilo.lang.mail:inbox",
        "it names the resource"
    );
    assert_eq!(entry.shown_as, "reader", "it names the view");
    assert_eq!(entry.shows, app, "shows is derived from the resource's app");
    // It stands at the point it was placed at, alone in its own strip.
    let held = strip_of(&session, number(&entry.strip)).expect("its strip is in the record");
    assert!(held.placed, "it stands on the canvas");
    assert_eq!((held.canvas_x, held.canvas_y), (240, 160));
    assert_eq!((held.canvas_width, held.canvas_height), (720, 520));
    // One attributed, undoable step.
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("it is a step of the record's history");
    assert_eq!(who, ADA, "attributed to the person");
    assert_eq!(verb, "place");
    session.undo_arrangement().expect("undo");
    settle(&mut session, |session| place_of(session, place).is_none());
}

// (2) A view left unnamed is the resource's primary published view, and nothing on the resource
// itself shows it: the arrangement carries the act.
#[test]
fn an_unnamed_view_is_the_resources_primary_and_no_resource_action_shows_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let (app, asked) = with_mailbox(&mut session);
    let row = front_row(&session);
    let (workspace, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        (
            tree.active_workspace()
                .expect("workspace")
                .slot
                .0
                .to_string(),
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };
    session
        .invoke(
            "shell:arrangement",
            "place",
            serde_json::json!({
                "workspace": workspace,
                "row": row,
                "strip": strip,
                "place": place,
                "resource": "tl:so.tilo.lang.mail:inbox",
            })
            .as_object()
            .expect("arguments"),
        )
        .expect("the arranging action runs");
    settle(&mut session, |session| place_of(session, place).is_some());
    let entry = place_of(&session, place).expect("the entry is in the record");
    assert_eq!(entry.shown_as, "inbox", "the primary published view");
    assert_eq!(entry.shows, app);
    // Showing it ran no action of the app's: the mailbox was never asked to show itself.
    assert!(
        asked.borrow().is_empty(),
        "the app ran nothing to be shown: {:?}",
        asked.borrow()
    );
    // And the arrangement publishes the act, so a person, an agent and the REPL show things the
    // same way.
    assert!(
        session.resources().iter().any(|resource| {
            resource.id.as_str() == "shell:arrangement"
                && resource.actions.iter().any(|action| action.id == "place")
        }),
        "the arranging action is published"
    );
    assert!(
        !session.resources().iter().any(|resource| resource
            .children
            .iter()
            .flat_map(|child| child.actions.iter())
            .any(|action| action.id.contains("show"))),
        "no resource carries an action that shows it"
    );
}

/// A session whose search reads `index` : the one index the palette and an agent's `tilo` tool find
/// things through, with `app` 's entries in it.
fn session_searching(
    index: std::sync::Arc<tilo_desktop_search::content_index::AppContentIndex>,
) -> Session {
    crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            person: ADA.to_owned(),
            state: None,
            collab: false,
            search: tilo_session::SearchOptions {
                content: Some(index),
                ..tilo_session::SearchOptions::default()
            },
            ..Options::default()
        },
    )
}

/// Publish the mailbox into the index the session searches, as an app does: the entry opens the
/// resource through its app's `open` , at that resource.
fn index_mailbox(
    index: &tilo_desktop_search::content_index::AppContentIndex,
    app: &str,
    workspace: &str,
) {
    let entry = tilo_resource::search_index::IndexEntry {
        key: "tl:so.tilo.lang.mail:inbox".to_owned(),
        resource: "tl:so.tilo.lang.mail:inbox".to_owned(),
        // An entry with no action of its own opens its anchor, the resource itself: what a note, a
        // message or a mailbox is.
        action: None,
        title: "Inbox".to_owned(),
        subtitle: Some("Mailbox".to_owned()),
        group: None,
        icon: None,
        boost: 0,
        fields: Vec::new(),
        secret: false,
        references: Vec::new(),
        ..tilo_resource::search_index::IndexEntry::default()
    };
    let mut publisher = tilo_resource::search_index::Publisher::default();
    for delta in publisher.publish(
        tilo_resource::search_index::IndexState {
            generation: 1,
            secrets_unlocked: None,
        },
        vec![entry],
    ) {
        index
            .publish(
                workspace,
                app,
                "tab:0001",
                &delta.encode().expect("a delta encodes"),
            )
            .expect("the index takes the delta");
    }
}

// (3) Text typed on the canvas runs the one `find` and the top result becomes an entry at the
// caret: the same action, attributed and undoable.
#[test]
fn text_typed_on_the_canvas_places_what_find_finds() {
    let _settled = ui::motion::testing::settled();
    let index =
        std::sync::Arc::new(tilo_desktop_search::content_index::AppContentIndex::in_memory());
    let mut session = session_searching(index.clone());
    step(&mut session);
    let (app, _) = with_mailbox(&mut session);
    // The id of the workspace in front: where search reads its entries.
    let workspace = session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
        .expect("a workspace is in front");
    index_mailbox(&index, &app, &workspace);

    session
        .invoke(
            "shell:arrangement",
            "place-found",
            serde_json::json!({"text": "inbox", "x": 320, "y": 96})
                .as_object()
                .expect("arguments"),
        )
        .expect("the arranging action runs");

    // The entry that stands is the one the search found, at the caret.
    let placed = arrangement(&session)
        .places
        .into_iter()
        .find(|place| place.record == "tl:so.tilo.lang.mail:inbox")
        .expect("the found resource is on the canvas");
    assert_eq!(placed.shown_as, "inbox", "its primary published view");
    assert_eq!(placed.shows, app, "shows follows from the resource's app");
    let strip =
        strip_of(&session, number(&placed.strip)).expect("the entry has a strip of its own");
    assert!(strip.placed, "it stands on the canvas");
    assert_eq!((strip.canvas_x, strip.canvas_y), (320, 96), "at the caret");
    // Attributed to the person, and undone as one step.
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("it is a step of the record's history");
    assert_eq!(who, ADA);
    assert_eq!(verb, "place");
    let id = number(&placed.key);
    session.undo_arrangement().expect("undo");
    settle(&mut session, |session| place_of(session, id).is_none());
}

// (4) A text nothing published matches is not dropped and not refused: it is a sentence to the
// agent the workspace keeps, so the ask conversation stands where the person typed (shell.md §8.1,
// "Asking from the canvas").
#[test]
fn text_that_finds_nothing_asks_the_kept_agent_instead_of_being_dropped() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let before = arrangement(&session).places.len();
    let placed = session
        .invoke(
            "shell:arrangement",
            "place-found",
            serde_json::json!({"text": "nothing is called this", "x": 0, "y": 0})
                .as_object()
                .expect("arguments"),
        )
        .expect("text that names nothing asks the agent");
    let _ = placed;
    assert_eq!(
        arrangement(&session).places.len(),
        before + 1,
        "the ask conversation stands where they typed"
    );
}

// (5) A workspace that holds no row yet takes a place all the same: a workspace just joined has no
// rows until its first window, and that window may be one the person places on the canvas — text
// typed there, or a conversation an ask starts.
#[test]
fn a_workspace_with_no_row_yet_takes_a_place_and_the_row_it_stands_in() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let (app, _) = with_mailbox(&mut session);
    // The workspace in front as a join leaves it: on the desk, holding no row, and with no row in
    // front of it either.
    let workspace = {
        let mut tree = session.handle().read(Clone::clone);
        let slot = tree
            .workspaces
            .first_mut()
            .expect("a workspace is in front");
        slot.strips.clear();
        let slot = slot.slot;
        tree.active_strip.remove(&slot);
        session.handle().replace_tree(tree);
        session.handle().read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.slot == slot)
                .map(|held| held.slot.0)
                .expect("the workspace is on the desk")
        })
    };
    step(&mut session);
    assert!(
        session.handle().read(|tree| {
            let Some(held) = tree.active_workspace() else {
                return false;
            };
            held.strips.is_empty() && !tree.active_strip.contains_key(&held.slot)
        }),
        "the workspace in front holds no row, as one just joined does"
    );

    let placed = session
        .invoke(
            "shell:arrangement",
            "place",
            serde_json::json!({
                "resource": "tl:so.tilo.lang.mail:inbox",
                "x": 240,
                "y": 160,
                "width": 720,
                "height": 520,
            })
            .as_object()
            .expect("arguments"),
        )
        .expect("a workspace with no row takes a place");
    let _ = placed;
    settle(&mut session, |session| {
        arrangement(session)
            .places
            .iter()
            .any(|place| place.record == "tl:so.tilo.lang.mail:inbox")
    });
    let entry = arrangement(&session)
        .places
        .into_iter()
        .find(|place| place.record == "tl:so.tilo.lang.mail:inbox")
        .expect("the found resource stands");
    assert_eq!(entry.shows, app, "shows follows from the resource's app");
    // The workspace holds the row the place stands in: the one made for it.
    let rows = session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .find(|held| held.slot.0 == workspace)
            .map(|held| {
                held.strips
                    .iter()
                    .map(|row| row.window_groups.len())
                    .collect::<Vec<_>>()
            })
    });
    assert_eq!(
        rows.as_deref(),
        Some([1].as_slice()),
        "the workspace has the one row, and the place's group stands in it"
    );
    let strip = strip_of(&session, number(&entry.strip)).expect("the place has its strip");
    assert!(strip.placed, "it stands on the canvas");
}

// A place asked to float stands above the workspace's windows at the corner and size it asked for,
// rather than on the canvas, and that is the same one undoable step.
#[test]
fn a_place_asked_to_float_stands_above_the_windows_at_its_corner() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    with_mailbox(&mut session);
    let row = front_row(&session);
    let (workspace, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        (
            tree.active_workspace()
                .expect("workspace")
                .slot
                .0
                .to_string(),
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };

    session
        .invoke(
            "shell:arrangement",
            "place",
            serde_json::json!({
                "workspace": workspace,
                "row": row,
                "strip": strip,
                "place": place,
                "resource": "tl:so.tilo.lang.mail:inbox",
                "view": "reader",
                "float": "top-left",
                "dx": 48,
                "dy": 64,
                "width": 520,
                "height": 360,
            })
            .as_object()
            .expect("arguments"),
        )
        .expect("the arranging action runs");
    settle(&mut session, |session| place_of(session, place).is_some());

    let entry = place_of(&session, place).expect("the entry is in the record");
    let held = strip_of(&session, number(&entry.strip)).expect("its strip is in the record");
    assert!(held.floating, "it stands above the windows");
    assert_eq!(held.float_corner, "top-left", "at the corner it asked for");
    assert_eq!(
        (held.float_dx, held.float_dy),
        (48, 64),
        "kept that far off the corner's edges"
    );
    assert_eq!(
        (held.float_width, held.float_height),
        (520, 360),
        "at the size it asked for"
    );
    assert!(!held.placed, "a float stands above the canvas, not on it");

    session.undo_arrangement().expect("undo");
    settle(&mut session, |session| place_of(session, place).is_none());
}
