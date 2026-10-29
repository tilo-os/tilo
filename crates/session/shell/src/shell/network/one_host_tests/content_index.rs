//! What a person makes in a workspace shows in the workspace's content
//! list: a note, a task, a sheet or a document, made through the app's own
//! action as a person or an agent makes it, is listed in the room by its
//! name on the device that made it, and on the person's other device once
//! the app opens there.
//!
//! The list is the session's index of what the room's apps hold
//! ([`tilo_desktop_search::content_index`]): the workspace's settings
//! list its resources and search finds them by name.

use super::chat_seed::{
    NewWorkspace, Running, Step, broker_scratch, new_workspace, offered, offering,
};
use super::{RoomId, until};
use tilo_desktop_search::content_index::{self as app_content_index, SlotKey};
use ui::record::RecordCheckout;

/// One app's way of making an item named `token`.
struct Maker {
    app: &'static str,
    definition: fn() -> &'static ui::application::ApplicationDefinition,
    /// The action that makes the item, and its arguments for the name.
    action: &'static str,
    arguments: fn(&str) -> serde_json::Value,
}

/// The titles of the room's content list: each resource the room's apps
/// listed, by the title its entry shows, with the app.
fn content_list(room: RoomId) -> Vec<(String, String)> {
    let index = app_content_index::session();
    let room = room.to_string();
    index
        .resources(&room)
        .into_iter()
        .map(|(app, resource)| {
            let title = index.title(&room, &resource).unwrap_or_default();
            (app, title)
        })
        .collect()
}

/// Whether `room`'s content list shows an item of `app` named `token`.
fn listed(room: RoomId, app: &str, token: &str) -> bool {
    content_list(room)
        .iter()
        .any(|(by, title)| by == app && title.contains(token))
}

/// What the app would publish to the index now, for a failure to say
/// where the item stopped.
fn publishing(app: &mut ui::App) -> String {
    let entries: Vec<String> = ui::search::capability::search_index_entries(app.root_widget_mut())
        .into_iter()
        .map(|entry| format!("{} ({})", entry.title, entry.resource))
        .collect();
    format!("the app's entries {entries:?}")
}

/// Forget what `app` published in `room`, so only a new publication lists
/// it again: one process runs both devices, and their sessions share the
/// index.
fn forget(room: RoomId, app: &str) {
    let index = app_content_index::session();
    let room = room.to_string();
    let slots: Vec<SlotKey> = index
        .contents()
        .visible
        .into_iter()
        .map(|indexed| indexed.slot)
        .filter(|slot| slot.app == app && slot.room == room)
        .collect();
    for slot in slots {
        index.apply(
            &slot,
            ui::search::index_publication::IndexDelta {
                reset: true,
                ..Default::default()
            },
        );
    }
}

/// Make an item named `token` with `maker` in a new workspace on Ada's
/// desktop, and find it in the workspace's content list there; then find
/// it there again once her laptop's app opens the workspace.
fn made_item_is_listed_on_both_devices(maker: &Maker, seed: u8, ports: u16, token: &'static str) {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = broker_scratch();
    let mut workspace: NewWorkspace = new_workspace(&mut home, dir.path(), seed, ports);
    let room = workspace.room;
    let (app_id, action) = (maker.app, maker.action);
    let arguments = (maker.arguments)(token);
    let kind = (maker.definition)().record_kind.clone();

    let mut made: Option<String> = None;
    let (desktop_broker, found, desktop_app): (_, Result<RecordCheckout, String>, Running) =
        workspace.launch_app_on_desktop(
            dir.path(),
            app_id,
            (maker.definition)(),
            &format!("{app_id}'s new item in the workspace's content list"),
            move |_| {
                move |app, owners| {
                    if made.is_none() {
                        let resources = app.collect_resources();
                        let Some(resource) = offering(&resources, action) else {
                            return Step::Waiting(format!(
                                "no {action} offered; the app offers {:?}",
                                offered(&resources)
                            ));
                        };
                        let address =
                            tilo_actions::ActionAddress::new(resource.id.as_str(), action);
                        match app.invoke_local_resource_action(
                            &address,
                            arguments.as_object().expect("the arguments are an object"),
                        ) {
                            Ok(started) => made = Some(format!("{action} started {started:?}")),
                            Err(refused) => {
                                return Step::Failed(format!("{action} was refused: {refused:?}"));
                            }
                        }
                    }
                    if let Some(refused) = app.controlled_refused_action_for_test() {
                        return Step::Failed(format!("the session refused {action}: {refused}"));
                    }
                    if listed(room, app_id, token) {
                        // The record the item is in, which the laptop opens
                        // as the session opens an item picked from the
                        // list: the room's own record of the app, or a
                        // document of its own (a workbook, a Writer
                        // document).
                        let open = owners
                            .clone()
                            .open(app_id, &room.to_string())
                            .map(|records| records.open_records())
                            .unwrap_or_default();
                        return match open.into_iter().find(|record| record.kind == kind) {
                            Some(record) => Step::Done(record),
                            None => Step::Failed(format!("{app_id} lists the item in no record")),
                        };
                    }
                    Step::Waiting(format!(
                        "{}; the room lists {:?}; {}",
                        made.as_deref().unwrap_or_default(),
                        content_list(room),
                        publishing(app)
                    ))
                }
            },
        );
    let checkout = found.unwrap_or_else(|failure| {
        panic!("the desktop does not list {app_id}'s new item {token:?}: {failure}")
    });
    until(
        &[&workspace.desktop, &workspace.laptop],
        "the new item's writes on the room's host",
        || (workspace.desktop.retained_writes(room, app_id) >= 1).then_some(()),
    );
    drop(desktop_app);
    forget(room, app_id);
    assert!(
        !listed(room, app_id, token),
        "the index forgot the desktop's entries"
    );

    let (laptop_broker, read, laptop_app) = workspace.open_app_on_laptop(
        dir.path(),
        app_id,
        (maker.definition)(),
        checkout,
        &format!("{app_id}'s item made on the desktop in the content list on the laptop"),
        move |app, _| {
            if listed(room, app_id, token) {
                Step::Done(())
            } else {
                Step::Waiting(format!(
                    "the room lists {:?}; {}",
                    content_list(room),
                    publishing(app)
                ))
            }
        },
    );
    drop(laptop_app);
    if let Err(failure) = read {
        panic!("the laptop does not list {app_id}'s item {token:?} made on the desktop: {failure}");
    }
    drop((laptop_broker, desktop_broker));
    drop(workspace);
}

#[test]
#[serial_test::serial]
fn a_new_note_is_listed_in_the_workspace_on_both_devices() {
    made_item_is_listed_on_both_devices(
        &Maker {
            app: "so.tilo.notes",
            definition: notes::definition,
            action: "new-note",
            arguments: |token| serde_json::json!({ "title": token }),
        },
        151,
        0x0C11,
        "Groceries for the weekend",
    );
}

#[test]
#[serial_test::serial]
fn a_new_task_is_listed_in_the_workspace_on_both_devices() {
    made_item_is_listed_on_both_devices(
        &Maker {
            app: tasks::APP_ID,
            definition: || tilo_app::built_in_definition!(tasks::App),
            action: "create",
            arguments: |token| serde_json::json!({ "title": token }),
        },
        152,
        0x0C13,
        "Fix the garden gate",
    );
}

#[test]
#[serial_test::serial]
fn a_new_sheet_is_listed_in_the_workspace_on_both_devices() {
    made_item_is_listed_on_both_devices(
        &Maker {
            app: "so.tilo.sheets",
            definition: sheets::definition,
            action: "new-sheet",
            arguments: |token| serde_json::json!({ "name": token }),
        },
        153,
        0x0C15,
        "Holiday budget",
    );
}

#[test]
#[serial_test::serial]
fn a_written_document_is_listed_in_the_workspace_on_both_devices() {
    made_item_is_listed_on_both_devices(
        &Maker {
            app: "so.tilo.word-processor",
            definition: word_processor::definition,
            action: "append-paragraph",
            arguments: |token| serde_json::json!({ "text": token }),
        },
        154,
        0x0C17,
        "Letter to the landlord",
    );
}
