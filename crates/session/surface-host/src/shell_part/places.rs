//! A place on the shell's canvas that names another app's record composes that app's own view
//! over the record's reading as the session publishes it, and an edit in the composed view
//! goes to the record's door: the note the notes app reads is the note the place shows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures"
)]

use serde_json::{Map, Value as Json, json};
use tilo_service::{Service as _, Started};
use tilolang_tilo::records_service::RecordService;

use super::tests_support::{part_with, reading_with_windows};
use super::*;

const NOTES_APP: &str = "so.tilo.lang.notes";

/// Click the shell's control `name`, as the person does: what stands there takes the press,
/// and the frames after it settle what the press made.
fn click(part: &mut ShellPart, name: &str) {
    let (x, y, w, h) = part
        .control_bounds(name)
        .unwrap_or_else(|| panic!("the screen shows {name}"));
    part.pointer_motion(x + w / 2.0, y + h / 2.0);
    part.prepare_frame();
    part.pointer_button(0x110, true);
    part.prepare_frame();
    part.pointer_button(0x110, false);
    for _ in 0..6 {
        part.prepare_frame();
    }
}

/// A room's notebook, published as the session publishes it.
fn notebook() -> (RecordService, tilolang_tilo::runtime::Meta) {
    let record = tilo_resource::Doc::new();
    let host = Host::new(&record, Principal::new("Pat"), || None).unwrap();
    host.load_builtin(
        include_str!("../../../../../apps/tl/discussion/app.tl"),
        include_str!("../../../../../apps/tl/discussion/app.ids"),
    )
    .unwrap();
    let notes = host
        .load_builtin(
            include_str!("../../../../../apps/tl/notes/app.tl"),
            include_str!("../../../../../apps/tl/notes/app.ids"),
        )
        .unwrap();
    host.root_of(NOTES_APP).unwrap();
    let mut record = record;
    for committed in host.take_committed() {
        tilo_actions_apply(&host, &mut record, &committed.action);
    }
    (RecordService::new(record, host, "Pat"), notes.meta)
}

/// Commit what the runtime made on its own (the notebook's root) to `record`.
fn tilo_actions_apply(
    host: &Host,
    record: &mut tilo_resource::Doc,
    action: &tilolang_tilo::DynAction,
) {
    let context = ui::tilo_actions::ActionContext::for_resource("room", &action.kind, &action.kind)
        .with_invoker(ui::tilo_actions::ActionInvoker::actor("Pat"));
    host.with_schemas(|| {
        ui::tilo_actions::apply_local_with_context(
            record,
            action,
            action.encode().unwrap(),
            Some(b"Pat"),
            context,
        )
    })
    .unwrap();
    record.commit();
    host.sync_from(record);
}

fn action_id(meta: &tilolang_tilo::runtime::Meta, name: &str) -> String {
    meta.actions
        .iter()
        .find(|action| action.name == name)
        .unwrap()
        .id
        .clone()
}

fn field_id(meta: &tilolang_tilo::runtime::Meta, ty: &str, field: &str) -> String {
    meta.types
        .iter()
        .find(|declared| declared.name == ty)
        .and_then(|declared| declared.fields.iter().find(|f| f.name == field))
        .unwrap()
        .id
        .clone()
}

/// A note titled `title`, made through the notes app's own actions.
fn note(service: &mut RecordService, meta: &tilolang_tilo::runtime::Meta, title: &str) -> String {
    let notebook = service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{NOTES_APP}.Notebook"))
        .unwrap();
    let Started::Done(made) = service
        .invoke(
            notebook.id.as_str(),
            &action_id(meta, "new_note"),
            &Map::new(),
        )
        .unwrap()
    else {
        panic!("a tilolang action is done once committed");
    };
    let note = made.resources.first().cloned().unwrap();
    service
        .invoke(
            &note,
            &action_id(meta, "retitle"),
            json!({ "title": title }).as_object().unwrap(),
        )
        .unwrap();
    service.settle();
    note
}

/// The person's arrangement: one placed strip on the canvas, its one place showing `record` as
/// the type `shows` through the view `shown_as` (`""`: the type's default view).
fn arrangement(record: &str, shows: &str, shown_as: &str) -> ArrangementReading {
    ArrangementReading {
        workspaces: vec![ArrangementWorkspaceReading {
            key: "w1".to_owned(),
            name: "Desk".to_owned(),
            current: true,
        }],
        rows: vec![ArrangementRowReading {
            key: "r1".to_owned(),
            workspace: "w1".to_owned(),
            name: "Main".to_owned(),
            shell: String::new(),
        }],
        strips: vec![ArrangementStripReading {
            key: "s1".to_owned(),
            workspace: "w1".to_owned(),
            row: "r1".to_owned(),
            width: 1000,
            placed: true,
            canvas_width: 1100,
            canvas_height: 1200,
            ..ArrangementStripReading::default()
        }],
        places: vec![ArrangementPlaceReading {
            key: "p1".to_owned(),
            strip: "s1".to_owned(),
            shows: shows.to_owned(),
            shown_as: shown_as.to_owned(),
            record: record.to_owned(),
            ..ArrangementPlaceReading::default()
        }],
    }
}

/// The notes app's editor, drawing the note `record`.
fn note_arrangement(record: &str) -> ArrangementReading {
    arrangement(record, "Note", "note_editor")
}

/// The shell part showing `note` in a place, its reading published, drawn.
fn showing(service: &RecordService, note: &str) -> (ShellPart, Rc<SessionLink>) {
    let (mut part, link) = part_with(reading_with_windows());
    let arrangement = note_arrangement(note);
    link.publish_arrangement(arrangement.clone());
    assert!(
        follow(&link, &arrangement, service),
        "the place's record has a reading"
    );
    for _ in 0..4 {
        part.prepare_frame();
    }
    (part, link)
}

/// What the session hands the shell for `arrangement`'s places, as it does on its turn: the
/// notes app's package, which the places show, and the readings of the records they name.
/// Whether a reading changed.
fn follow(link: &SessionLink, arrangement: &ArrangementReading, service: &RecordService) -> bool {
    link.publish_packages(BTreeMap::from([(
        NOTES_APP.to_owned(),
        service.module(NOTES_APP).expect("the notes app's module"),
    )]));
    link.follow_places(arrangement, &service.resources(), |id| service.read(id))
}

/// Run what the shell asked against the note's door, as the session's turn does.
fn serve(link: &SessionLink, service: &mut RecordService) {
    link.serve(|ask| {
        let arguments: Map<String, Json> = ask
            .arguments
            .iter()
            .map(|(name, value)| (name.clone(), Json::String(value.clone())))
            .collect();
        service
            .invoke(&ask.resource, &ask.action, &arguments)
            .map(drop)
            .map_err(|error| panic!("the note's door refused {ask:?}: {error:?}"))
    });
}

/// The `spaces` desk style over the arrangement the part holds now. A desk style is a view of
/// the arrangement any installed package offers (`so.tilo.shell:spaces`, shell.md §6), and the
/// desk draws the person's choice through `show(a, view: <style>)`; this is the style the places
/// below are read through, opened over the arrangement the part holds.
fn canvas(part: &ShellPart) -> tilolang_tilo::runtime::ViewInstance {
    let host = part.instance.host().expect("the shell runs");
    let app = host
        .packages()
        .last()
        .map(|package| package.meta.app.clone())
        .unwrap();
    let root = host
        .root_of_layer(
            &app,
            &RootView {
                root: Some("Arrangement".to_owned()),
                view: None,
            },
        )
        .unwrap();
    let view = host
        .package(&app)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == "spaces")
        .unwrap()
        .id
        .clone();
    host.open_resource(&root, Some(&view)).unwrap()
}

/// The control bound to the field `field` of a resource of another app.
fn bound_to<'a>(
    node: &'a tilolang_tilo::runtime::UiNode,
    field: &str,
) -> Option<&'a tilolang_tilo::runtime::UiNode> {
    let bound = node.binding.as_ref().is_some_and(|binding| {
        matches!(
            &binding.target,
            tilolang_tilo::runtime::view::BindTarget::Field { target, field: bound }
                if bound == field && target.ty.contains('#')
        )
    });
    if bound {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| bound_to(child, field))
}

/// Every text the tree shows, in its props.
fn texts(node: &tilolang_tilo::runtime::UiNode) -> Vec<String> {
    let mut out: Vec<String> = node
        .props
        .values()
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    for child in &node.children {
        out.extend(texts(child));
    }
    out
}

#[test]
fn a_place_composes_the_notes_editor_over_the_note_and_an_edit_there_changes_the_note() {
    let (mut service, meta) = notebook();
    let note = note(&mut service, &meta, "Groceries");

    let (mut part, link) = showing(&service, &note);
    let arrangement = note_arrangement(&note);

    let view = canvas(&part);
    let title_field = field_id(&meta, "Note", "title");
    assert!(
        texts(view.tree()).contains(&"Groceries".to_owned()),
        "the place shows the note: {:#?}",
        view.tree()
    );
    let title = bound_to(view.tree(), &title_field)
        .unwrap_or_else(|| panic!("the place shows the note's editor: {:#?}", view.tree()));
    assert_eq!(
        view.edit(&title.key, &Value::text("Groceries and lemons"))
            .unwrap(),
        tilolang_tilo::runtime::EditOutcome::Applied
    );
    assert!(link.has_asks(), "the edit is asked of the note's door");

    // The edit went to the note's door, as the session's turn runs what the shell asked.
    serve(&link, &mut service);
    let reading = service.read(&note).unwrap();
    assert_eq!(
        reading.get(&title_field),
        Some(&json!("Groceries and lemons")),
        "the note the notes app reads changed: {reading:#}"
    );

    // And the place draws the note as it is now.
    assert!(follow(&link, &arrangement, &service));
    for _ in 0..4 {
        part.prepare_frame();
    }
    let view = canvas(&part);
    assert!(
        texts(view.tree()).contains(&"Groceries and lemons".to_owned()),
        "{:#?}",
        view.tree()
    );
}

#[test]
fn the_shell_keeps_no_copy_of_the_apps_it_composes() {
    let (part, _link) = part_with(reading_with_windows());
    let host = part.instance.host().expect("the shell runs");
    // The apps the shell's views compose are other apps' here: what the shell's runtime runs of
    // them reads their records from the session, and the shell's record holds none of theirs.
    for app in ["so.tilo.lang.chat", NOTES_APP, "so.tilo.lang.tasks"] {
        assert!(
            host.package(app).is_none(),
            "`{app}` runs as the shell's own package, keeping a copy of its records"
        );
    }
    assert_eq!(
        host.packages()
            .iter()
            .map(|package| package.meta.app.clone())
            .collect::<Vec<_>>(),
        vec![host.packages().last().unwrap().meta.app.clone()],
        "the shell runs its own package alone"
    );
}

#[test]
fn the_notes_body_written_in_a_place_is_the_body_the_notes_app_reads() {
    let (mut service, meta) = notebook();
    let note = note(&mut service, &meta, "Groceries");
    let (part, link) = showing(&service, &note);

    let view = canvas(&part);
    let body_field = field_id(&meta, "Note", "body");
    let body = bound_to(view.tree(), &body_field)
        .unwrap_or_else(|| panic!("the place shows the note's body: {:#?}", view.tree()));
    let written = Value::Rich(tilolang_tilo::runtime::rich::RichValue::from_text(
        "Lemons, two of them",
    ));
    assert_eq!(
        view.edit(&body.key, &written).unwrap(),
        tilolang_tilo::runtime::EditOutcome::Applied
    );
    serve(&link, &mut service);

    let reading = service.read(&note).unwrap();
    assert!(
        reading
            .get(&body_field)
            .is_some_and(|body| body.to_string().contains("Lemons, two of them")),
        "the note the notes app reads has the body written in the place: {reading:#}"
    );
}

/// A note placed with no view named stands on the framed canvas of the whole screen as its
/// type's default view, the notes app's editor, laid out in its frame with the note's own
/// fields in its controls: drawn, not only built.
#[test]
fn a_placed_note_is_laid_out_on_the_framed_canvas_as_its_editor() {
    let _settled = ui::motion::testing::settled();
    let (mut service, meta) = notebook();
    let note = note(&mut service, &meta, "Groceries");
    let (mut part, link) = super::tests_support::part_as(reading_with_windows(), None);
    let mut arrangement = note_arrangement(&note);
    // The one place names no view: it falls back to its type's default view.
    let placed = arrangement
        .places
        .first_mut()
        .expect("the arrangement places the note");
    placed.shown_as = String::new();
    link.publish_arrangement(arrangement.clone());
    assert!(follow(&link, &arrangement, &service));
    for _ in 0..6 {
        part.prepare_frame();
    }
    // The bar's preset menu says the desk shows the strips; its items show the framed canvas,
    // where the place is drawn through the note's own editor.
    click(&mut part, "Showing Strips");
    click(&mut part, "Frames");
    for _ in 0..8 {
        part.prepare_frame();
    }
    // The editor's title field, holding the note's title, with room on screen.
    let title = part
        .widgets
        .host_mut()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some("Untitled"))
        .map(|(_, node)| (node.value().map(str::to_owned), node.bounds()));
    let Some((value, Some(bounds))) = title else {
        panic!(
            "the note's editor stands in its place: {:?}",
            crate::Published::accessible_labels(&mut part)
        );
    };
    assert_eq!(
        value.as_deref(),
        Some("Groceries"),
        "the title field holds the note's title"
    );
    assert!(
        bounds.width() > 0.0 && bounds.height() > 0.0,
        "the editor is laid out with room: {bounds:?}"
    );
}

const CHAT: &str = include_str!("../../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../../apps/tl/chat/app.ids");
const DISCUSSION: &str = include_str!("../../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../../apps/tl/discussion/app.ids");
const TASKS: &str = concat!(
    include_str!("../../../../../apps/tl/tasks/app.tl"),
    "\n",
    include_str!("../../../../../apps/tl/tasks/scenarios.tl")
);
const TASKS_IDS: &str = include_str!("../../../../../apps/tl/tasks/app.ids");
const CHAT_APP: &str = "so.tilo.lang.chat";
const TASKS_APP: &str = "so.tilo.lang.tasks";

/// What Bo writes, and what the task made from it is called.
const SAID: &str = "can someone book the van for Saturday?";

/// Commit what the runtime made on its own to `record`, as a session's record takes it.
fn commit(host: &Host, record: &mut tilo_resource::Doc) {
    for committed in host.take_committed() {
        tilo_actions_apply(host, record, &committed.action);
    }
}

/// The value the action `action` of `meta` gives when the person runs it with `args`.
fn call(host: &Host, meta: &tilolang_tilo::runtime::Meta, action: &str, args: Vec<Value>) -> Value {
    let id = action_id(meta, action);
    match host
        .runtime()
        .invoke_action(&Principal::new("ada"), (id.as_str(), args))
        .unwrap()
    {
        tilolang_tilo::runtime::InvokeOutcome::Done(value) => value,
        other => panic!("{action}: {other:?}"),
    }
}

/// The resource a value names.
fn target(value: &Value) -> ResourceRef {
    match value {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

/// The room the session publishes: a chat with the channel `design` and Bo's message in it, a
/// tasks app, and the task Ada made from that message through its own menu.
struct Room {
    service: RecordService,
    channel: String,
    message: ResourceRef,
    task: ResourceRef,
    tasks: tilolang_tilo::runtime::Meta,
    /// The readings the session follows for the shell's places, by id, each with its type: what
    /// it reads again for the ones a change touched.
    following: BTreeMap<String, String>,
    /// The apps whose packages a view read a type of before it was loaded, kept for the rest of
    /// the session: what the session publishes with the packages the places show.
    needed: std::collections::BTreeSet<String>,
}

fn room() -> Room {
    let record = tilo_resource::Doc::new();
    let host = Host::new(&record, Principal::new("ada"), || None).unwrap();
    let chat = host.load_builtin(CHAT, CHAT_IDS).unwrap();
    host.load_builtin(DISCUSSION, DISCUSSION_IDS).unwrap();
    let tasks = host.load_builtin(TASKS, TASKS_IDS).unwrap();
    // Both roots exist, as they do once each app's window was opened in the workspace.
    let all = Value::resource(&host.root_of(CHAT_APP).unwrap());
    let tasks_root = Value::resource(&host.root_of(TASKS_APP).unwrap());
    let mut record = record;
    commit(&host, &mut record);
    let team = call(
        &host,
        &chat.meta,
        "new_category",
        vec![all, Value::text("Team")],
    );
    let design = call(
        &host,
        &chat.meta,
        "add_channel",
        vec![team, Value::text("design")],
    );
    commit(&host, &mut record);
    let said = Value::Rich(tilolang_tilo::runtime::rich::RichValue::from_text(SAID));
    let message = call(
        &host,
        &chat.meta,
        "send",
        vec![design.clone(), said, Value::none()],
    );
    commit(&host, &mut record);
    // The task is made from the message, as the message's own menu makes it: the words the
    // message says fill the title the task is called by, and the task is made from the message.
    let message = target(&message);
    let id = action_id(&tasks.meta, "new_task");
    let filled = host
        .runtime()
        .fill_entry(&id, vec![Some(tasks_root)], SAID)
        .unwrap()
        .unwrap_or_else(|| panic!("the message's words make a task"));
    let args = host.runtime().complete_args(&id, filled).unwrap();
    let made = match host
        .runtime()
        .invoke_action_from(&Principal::new("ada"), (id.as_str(), args), &message)
        .unwrap()
    {
        tilolang_tilo::runtime::InvokeOutcome::Done(made) => made,
        other => panic!("new_task: {other:?}"),
    };
    commit(&host, &mut record);
    Room {
        service: RecordService::new(record, host, "ada"),
        channel: target(&design).id_string(),
        message,
        task: published(&target(&made)),
        tasks: tasks.meta,
        following: BTreeMap::new(),
        needed: std::collections::BTreeSet::new(),
    }
}

/// `record` as the session publishes it to another app: `tl:<app>:<id>`, of the type
/// `<app>#<Type>`.
fn published(record: &ResourceRef) -> ResourceRef {
    let schema = tilolang_tilo::schema::lookup(&record.ty).expect("a tilolang record");
    let id = format!("tl:{}:{}", schema.app, record.id_string());
    ResourceRef::parse_id(&id, format!("{}#{}", schema.app, schema.name))
        .expect("a published resource id")
}

/// Every node of `kind` under `node`.
fn nodes_of<'a>(
    node: &'a tilolang_tilo::runtime::UiNode,
    kind: &str,
    out: &mut Vec<&'a tilolang_tilo::runtime::UiNode>,
) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        nodes_of(child, kind, out);
    }
}

/// The waiting states the canvas shows, in its tree or in a row of one of its lists: a list's
/// rows are built as they show.
fn skeletons(
    view: &mut tilolang_tilo::runtime::ViewInstance,
) -> Vec<tilolang_tilo::runtime::UiNode> {
    let _ = view.poll_patches();
    let mut found = Vec::new();
    let mut here = Vec::new();
    nodes_of(view.tree(), "ui.Skeleton", &mut here);
    found.extend(here.into_iter().cloned());
    for row in rows(view) {
        let mut here = Vec::new();
        nodes_of(&row, "ui.Skeleton", &mut here);
        found.extend(here.into_iter().cloned());
    }
    found
}

/// The rows of every list the canvas draws.
fn rows(view: &mut tilolang_tilo::runtime::ViewInstance) -> Vec<tilolang_tilo::runtime::UiNode> {
    let mut lists = Vec::new();
    nodes_of(view.tree(), "ui.VirtualList", &mut lists);
    let keys: Vec<String> = lists.iter().map(|list| list.key.clone()).collect();
    let mut rows = Vec::new();
    for key in keys {
        for index in 0..view.list_len(&key).unwrap_or(0) {
            if let Ok(Some(row)) = view.row(&key, index) {
                rows.push(row.clone());
            }
        }
    }
    rows
}

/// The rows the canvas draws of the task `of`, wherever they stand: the task's own row view as
/// the message's reference to it draws it, its checkbox bound to a field of the reading the
/// session publishes. The rows of a list are built as they show.
fn task_rows(
    view: &mut tilolang_tilo::runtime::ViewInstance,
    of: &ResourceRef,
) -> Vec<tilolang_tilo::runtime::UiNode> {
    let _ = view.poll_patches();
    let mut found = Vec::new();
    for row in &rows(view) {
        let mut here = Vec::new();
        nodes_of(row, "ui.ListRow", &mut here);
        found.extend(
            here.into_iter()
                .filter(|list_row| {
                    list_row.props.get("title") == Some(&Value::text(SAID))
                        && list_row.props.get("bind") == Some(&Value::text("checkbox"))
                        && list_row.binding.as_ref().is_some_and(|binding| {
                            matches!(
                                &binding.target,
                                tilolang_tilo::runtime::view::BindTarget::Field { target, .. }
                                    if target.id_string() == of.id_string()
                            )
                        })
                })
                .cloned(),
        );
    }
    found
}

/// One turn of the session over the shell's asks, as the session runs it: the package of each
/// app the places show and of each app a view read a type of, and the reading of each resource a
/// place names and of each the views read. `needed` keeps the apps asked for, as the session
/// keeps them for the rest of its run. Whether anything was published.
fn session_turn(link: &SessionLink, arrangement: &ArrangementReading, room: &mut Room) -> bool {
    let mut needed = std::mem::take(&mut room.needed);
    needed.extend(link.take_wanted_packages().into_keys());
    let mut apps = needed.clone();
    room.needed = needed;
    for ty in super::readings::placed_records(arrangement, &room.service.resources()).values() {
        if let Some(app) = super::readings::app_of(ty) {
            apps.insert(app.to_owned());
        }
    }
    let mut packages = BTreeMap::new();
    for app in apps {
        if let Some(module) = room.service.module(&app) {
            packages.insert(app, module);
        }
    }
    let mut changed = link.publish_packages(packages);
    changed |= link.follow_places(arrangement, &room.service.resources(), |id| {
        room.service.read(id)
    });
    for (id, ty) in link.take_wanted() {
        room.following.insert(id, ty);
    }
    // The session reads again the readings a change touched; the test reads them all, as every
    // one of them is what the place draws.
    for (id, ty) in &room.following {
        if let Some(reading) = room.service.read(id) {
            changed |= link.publish_reading(id, ty, reading);
        }
    }
    changed
}

/// A chat window on the room's channel, its first frames drawn, with no tasks window open: what
/// the place holds once its views read the readings the session published.
fn chat_place(mut room: Room) -> (ShellPart, Rc<SessionLink>, Room, ArrangementReading) {
    let arrangement = arrangement(
        &format!("tl:{CHAT_APP}:{}", room.channel),
        "TextChannel",
        "channel",
    );
    let (mut part, link) = part_with(reading_with_windows());
    link.publish_arrangement(arrangement.clone());
    part.prepare_frame();
    session_turn(&link, &arrangement, &mut room);
    part.prepare_frame();
    session_turn(&link, &arrangement, &mut room);
    part.prepare_frame();
    (part, link, room, arrangement)
}

/// The row the chat place draws for the task made from its message, once the session served the
/// tasks package its views asked for.
fn row_once_loaded(
    part: &mut ShellPart,
    link: &SessionLink,
    arrangement: &ArrangementReading,
    room: &mut Room,
) -> tilolang_tilo::runtime::UiNode {
    for _ in 0..8 {
        session_turn(link, arrangement, room);
        part.prepare_frame();
        let mut view = canvas(part);
        if let Some(row) = task_rows(&mut view, &room.task).pop() {
            return row;
        }
    }
    panic!(
        "the chat place draws the task made from the message: asked for {:?}",
        room.needed
    );
}

/// A chat window with no tasks window open waits for the tasks package its message's row needs:
/// a view that reads a resource of a type no loaded package declares asks for that app's package,
/// and the place shows its waiting state rather than nothing until it comes.
#[test]
fn a_chat_place_waits_for_the_tasks_package_the_row_it_draws_reads() {
    let _settled = ui::motion::testing::settled();
    let room = room();
    // What the message's reading says was made from it: the task, as its own app keeps it.
    let reading = room
        .service
        .read(&format!("tl:{CHAT_APP}:{}", room.message.id_string()))
        .unwrap();
    assert!(
        reading.to_string().contains(&room.task.id_string()),
        "the message's reading carries what was made from it: {reading:#}"
    );
    let (mut part, link, mut room, arrangement) = chat_place(room);

    assert!(
        !link.package_apps().iter().any(|app| app == TASKS_APP),
        "no tasks window is open: its package is not loaded"
    );
    assert!(link.wants_packages(), "the views ask for the tasks package");
    let mut view = canvas(&part);
    assert!(
        !skeletons(&mut view).is_empty(),
        "the place shows its waiting state: {:#?}",
        view.tree()
    );
    assert!(
        task_rows(&mut view, &room.task).is_empty(),
        "no row of the task is drawn before the tasks package is loaded: {:#?}",
        view.tree()
    );

    // The session serves the ask: the tasks package is published as the place's own is, the
    // task's reading with it, and the message's reference to the task draws it through the
    // task's row view.
    let row = row_once_loaded(&mut part, &link, &arrangement, &mut room);
    assert!(
        room.needed.contains(TASKS_APP),
        "the tasks package was asked for"
    );
    assert_eq!(
        row.props.get("title"),
        Some(&Value::text(SAID)),
        "the row draws the task through its row view: {row:#?}"
    );
}

/// The row a chat place draws for a task made from its message ticks to done when the task is
/// finished in Tasks, with no Tasks window open: it is the task's own row view, drawn over the
/// reading the session publishes.
#[test]
fn a_chat_place_ticks_the_task_made_from_its_message_done() {
    let _settled = ui::motion::testing::settled();
    let room = room();
    let (mut part, link, mut room, arrangement) = chat_place(room);
    row_once_loaded(&mut part, &link, &arrangement, &mut room);
    let mut view = canvas(&part);
    let row = task_rows(&mut view, &room.task)
        .pop()
        .expect("the chat place draws the task");
    assert_eq!(
        view.bound_value(&row.key),
        Some(Value::Bool(false)),
        "the task is not done"
    );

    // Ben finishes the task in Tasks, with no Tasks window open: the message's row ticks.
    room.service
        .invoke(
            &room.task.id_string(),
            &action_id(&room.tasks, "check"),
            &Map::new(),
        )
        .unwrap();
    room.service.settle();
    for _ in 0..8 {
        session_turn(&link, &arrangement, &mut room);
        part.prepare_frame();
    }
    let mut view = canvas(&part);
    let row = task_rows(&mut view, &room.task)
        .pop()
        .unwrap_or_else(|| panic!("the chat place still draws the task"));
    assert_eq!(
        view.bound_value(&row.key),
        Some(Value::Bool(true)),
        "the row shows the task done"
    );
}
