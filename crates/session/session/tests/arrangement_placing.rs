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
use tilo_session::mount::indexes::SessionIndexes;
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
                indexes: Some(std::sync::Arc::new(SessionIndexes::new(index, None))),
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

// (7) The system's `open` places a resource with its default view beside where it was asked
// (`docs/designs/app-language/one-app-model.md` §3, "`open(r)` places r with its default view,
// beside where it was asked, for every app"). A resource standing already comes forward instead of
// a second place being made.
#[test]
fn the_system_open_brings_what_is_placed_forward() {
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
                "x": 300,
                "y": 200,
            })
            .as_object()
            .expect("arguments"),
        )
        .expect("the anchor is placed");
    settle(&mut session, |session| place_of(session, place).is_some());

    session
        .invoke(
            "shell:arrangement",
            "open",
            serde_json::json!({ "resource": "tl:so.tilo.lang.mail:inbox" })
                .as_object()
                .expect("arguments"),
        )
        .expect("the open runs");

    let held: Vec<_> = arrangement(&session)
        .places
        .into_iter()
        .filter(|held| held.record == "tl:so.tilo.lang.mail:inbox")
        .collect();
    assert_eq!(held.len(), 1, "one place of the resource: {held:?}");
    assert_eq!(held[0].key, place.to_string(), "the place it already had");
    assert_eq!(held[0].shows, app);
    assert!(
        asked.borrow().is_empty(),
        "the app ran nothing to be shown: {:?}",
        asked.borrow()
    );
}

/// Opening what is not yet placed stands a place of it through its primary view, as the person's
/// own arranging step (`docs/designs/app-language/one-app-model.md` §3).
#[test]
fn the_system_open_stands_a_new_place() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    let (app, _) = with_mailbox(&mut session);
    let before = arrangement(&session).places.len();

    session
        .invoke(
            "shell:arrangement",
            "open",
            serde_json::json!({ "resource": "tl:so.tilo.lang.mail:inbox" })
                .as_object()
                .expect("arguments"),
        )
        .expect("the open runs");
    settle(&mut session, |session| {
        arrangement(session).places.len() == before + 1
    });
    let entry = arrangement(&session)
        .places
        .into_iter()
        .find(|held| held.record == "tl:so.tilo.lang.mail:inbox")
        .expect("the place stands");
    assert_eq!(entry.shows, app, "it shows the app the resource is of");
    assert_eq!(entry.shown_as, "inbox", "its primary published view");
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("a step of the history");
    assert_eq!(who, ADA);
    assert_eq!(verb, "place", "opening is one arranging step");
}

/// The live run's failure (shell.md §3, "a place next to where it came from"): an agent that
/// opened tasks, mail and notes beside its conversation saw every place stand over the
/// conversation or below it, and some off the screen. Three places opened beside one anchor stand
/// clear of it and of each other, each to its right or below it: the place beside the conversation
/// first, then down.
#[test]
fn places_opened_beside_one_place_stand_clear_of_it_and_each_other() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    // The app: a conversation and three things opened beside it, each a published resource.
    struct Things;
    impl Published for Things {
        fn resources(&mut self) -> Vec<ResourceSpec> {
            ["conversation", "tasks", "mail", "notes"]
                .into_iter()
                .map(|id| {
                    let mut resource =
                        ResourceSpec::new(format!("tl:test.anchor:{id}"), "test.thing", id)
                            .with_action(view_action("open-thing"))
                            .with_view(
                                ResourceViewSpec::new("page", "Page", "open-thing")
                                    .with_primary(true),
                            );
                    // An app's resource: the arrangement places what an app publishes, the way
                    // every app's own resources are.
                    resource.app_id = Some("test.anchor".to_owned());
                    resource
                })
                .collect()
        }

        fn invoke(
            &mut self,
            _: &ActionAddress,
            _: &serde_json::Map<String, serde_json::Value>,
            _: &mut tilo_surface_host::Look<'_>,
        ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
            Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ))
        }

        fn accessible_labels(&mut self) -> Vec<String> {
            Vec::new()
        }
    }
    session.publish(Rc::new(std::cell::RefCell::new(Things)));
    step(&mut session);
    step(&mut session);
    let open = |session: &mut Session, resource: &str, beside: &str| {
        session
            .invoke(
                "shell:arrangement",
                "open",
                serde_json::json!({ "resource": resource, "beside": beside })
                    .as_object()
                    .expect("arguments"),
            )
            .expect("the open runs");
    };
    open(&mut session, "tl:test.anchor:conversation", "");
    settle(&mut session, |session| {
        arrangement(session)
            .places
            .iter()
            .any(|held| held.record == "tl:test.anchor:conversation")
    });
    for opened in [
        "tl:test.anchor:tasks",
        "tl:test.anchor:mail",
        "tl:test.anchor:notes",
    ] {
        open(&mut session, opened, "tl:test.anchor:conversation");
        settle(&mut session, |session| {
            arrangement(session)
                .places
                .iter()
                .any(|held| held.record == opened)
        });
    }
    // Every place's canvas rectangle, as the desk holds it.
    let boxes: Vec<(String, i64, i64, i64, i64)> = arrangement(&session)
        .places
        .iter()
        .filter_map(|held| {
            let strip = strip_of(&session, number(&held.strip))?;
            Some((
                held.record.clone(),
                strip.canvas_x,
                strip.canvas_y,
                strip.canvas_width,
                strip.canvas_height,
            ))
        })
        .collect();
    assert_eq!(
        boxes.len(),
        4,
        "the conversation and three opened beside it: {boxes:?}"
    );
    let conversation = boxes
        .iter()
        .find(|(record, ..)| record == "tl:test.anchor:conversation")
        .expect("the conversation stands")
        .clone();
    for (index, (a, ax, ay, aw, ah)) in boxes.iter().enumerate() {
        for (b, bx, by, bw, bh) in boxes.iter().skip(index + 1) {
            let overlaps = ax < &(bx + bw) && bx < &(ax + aw) && ay < &(by + bh) && by < &(ay + ah);
            assert!(!overlaps, "{a} and {b} stand over each other: {boxes:?}");
        }
    }
    for (record, x, y, ..) in &boxes {
        if *record == conversation.0 {
            continue;
        }
        assert!(
            *x >= conversation.1 && *y >= conversation.2,
            "{record} stands to the conversation's right or below it, never above or to its left: \
             ({x},{y}) against {conversation:?}"
        );
        assert!(
            (*x, *y) != (conversation.1, conversation.2),
            "{record} stands over the conversation: {boxes:?}"
        );
    }
}

// (8) The live run's failure: an agent asked to show the person their mails stood one Mail place of
// the mailbox (the `Inbox` tab) and a second of a thread in it (the `sam@rivera.example` tab), so
// the person had two windows of one mailbox. Opening a resource of a root already standing — the
// root itself, or a view inside it — brings what stands forward, as activating what is shown does
// (`shell.md` §3): one mailbox is open, whether opened by its root or by a message in it.
#[test]
fn the_open_brings_a_place_of_the_same_root_forward_rather_than_standing_a_second() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    step(&mut session);
    // The app: a mailbox root and a thread inside it, as an app publishes its record.
    struct Mail;
    impl Published for Mail {
        fn resources(&mut self) -> Vec<ResourceSpec> {
            let mut root =
                ResourceSpec::new("tl:so.tilo.lang.mail:root-a1b2c3", "mail.mail", "Mail")
                    .with_action(view_action("open-mailbox"))
                    .with_view(
                        ResourceViewSpec::new("mailbox", "Mailbox", "open-mailbox")
                            .with_primary(true),
                    );
            root.app_id = Some("so.tilo.lang.mail".to_owned());
            let mut thread = ResourceSpec::new(
                "tl:so.tilo.lang.mail:root-a1b2c3/ie99c2c19-72",
                "mail.thread",
                "sam@rivera.example",
            )
            .with_action(view_action("open-thread"))
            .with_view(ResourceViewSpec::new("thread", "Thread", "open-thread").with_primary(true));
            thread.app_id = Some("so.tilo.lang.mail".to_owned());
            let mut other = ResourceSpec::new(
                "tl:so.tilo.lang.mail:root-z9y8x7/ie77ac1f-01",
                "mail.thread",
                "news@example",
            )
            .with_action(view_action("open-news"))
            .with_view(ResourceViewSpec::new("thread", "Thread", "open-news").with_primary(true));
            other.app_id = Some("so.tilo.lang.mail".to_owned());
            vec![root.with_child(thread), other]
        }

        fn invoke(
            &mut self,
            _: &ActionAddress,
            _: &serde_json::Map<String, serde_json::Value>,
            _: &mut tilo_surface_host::Look<'_>,
        ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
            Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ))
        }

        fn accessible_labels(&mut self) -> Vec<String> {
            Vec::new()
        }
    }
    session.publish(Rc::new(std::cell::RefCell::new(Mail)));
    step(&mut session);
    step(&mut session);
    let open = |session: &mut Session, resource: &str| {
        session
            .invoke(
                "shell:arrangement",
                "open",
                serde_json::json!({ "resource": resource })
                    .as_object()
                    .expect("arguments"),
            )
            .expect("the open runs");
    };
    let places_of = |session: &Session, prefix: &str| {
        arrangement(session)
            .places
            .into_iter()
            .filter(|held| held.record.starts_with(prefix))
            .count()
    };

    // The mailbox opens once, and a thread in it brings that place forward rather than a second.
    open(&mut session, "tl:so.tilo.lang.mail:root-a1b2c3");
    settle(&mut session, |session| {
        places_of(session, "tl:so.tilo.lang.mail:root-a1b2c3") == 1
    });
    open(
        &mut session,
        "tl:so.tilo.lang.mail:root-a1b2c3/ie99c2c19-72",
    );
    step(&mut session);
    step(&mut session);
    assert_eq!(
        places_of(&session, "tl:so.tilo.lang.mail:root-a1b2c3"),
        1,
        "one mailbox is open: {:?}",
        arrangement(&session).places
    );

    // A thread of another root of the same app is the other mailbox, and opens in its own place.
    open(&mut session, "tl:so.tilo.lang.mail:root-z9y8x7/ie77ac1f-01");
    settle(&mut session, |session| {
        places_of(session, "tl:so.tilo.lang.mail:root-z9y8x7") == 1
    });
    assert_eq!(
        places_of(&session, "tl:so.tilo.lang.mail:root-a1b2c3"),
        1,
        "the other root's thread stands beside the first mailbox, not over it"
    );
}
