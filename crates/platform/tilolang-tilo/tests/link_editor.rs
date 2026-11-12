//! A note's editor offers the notebook's notes as `[[` links, each by the title it reads as
//! now; a reference written into a note is what `linked()` reports the note by.

use automerge::AutoCommit;
use serde_json::json;
use tilolang_runtime::rich::{Reference, RichValue};
use tilolang_runtime::store::{LinkHow, Store as _};
use tilolang_runtime::{NameHost, Principal, ResourceRef, Value, ViewInstance};
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::record::DynValue;

use crate::fixtures::{Replica, field_id};

const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
const NOTES: &str = include_str!("../../../../apps/tl/notes/app.tl");
const NOTES_IDS: &str = include_str!("../../../../apps/tl/notes/app.ids");
const APP: &str = "so.tilo.lang.notes";

struct Workspace {
    ada: Replica,
    plan: String,
    groceries: String,
}

/// A notebook holding `Plan` and `Groceries`.
fn workspace() -> Workspace {
    let (mut ada, packages) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[(DISCUSSION, DISCUSSION_IDS), (NOTES, NOTES_IDS)],
    );
    let meta = packages[1].meta.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let mut made: Vec<String> = Vec::new();
    for title in ["Plan", "Groceries"] {
        call(
            &mut ada,
            &meta,
            "Notebook",
            &root.id_string(),
            "new_note",
            json!({}),
        );
        let notes = notes(&ada, &meta, &root);
        let note = notes.last().expect("the note just made").clone();
        let note = format!(
            "{}/{}:{}",
            root.id_string(),
            field_id(&meta, "Notebook", "notes"),
            note
        );
        call(
            &mut ada,
            &meta,
            "Note",
            &note,
            "retitle",
            json!({ "title": title }),
        );
        made.push(note);
    }
    Workspace {
        ada,
        plan: made[0].clone(),
        groceries: made[1].clone(),
    }
}

fn call(
    replica: &mut Replica,
    meta: &tilolang_runtime::Meta,
    ty: &str,
    resource: &str,
    name: &str,
    args: serde_json::Value,
) {
    let action = meta
        .actions
        .iter()
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("no action {name}"));
    let args = args.as_object().cloned().unwrap_or_default();
    let kind = format!("tl.{APP}.{ty}");
    let planned = replica
        .host
        .plan(&PlanRequest {
            app: APP,
            kind: &kind,
            action_id: &action.id,
            resource: Some(resource),
            args: &args,
            caller: None,
            from: None,
        })
        .unwrap();
    replica.apply(&planned);
}

/// The notebook's notes, as their ids in the collection.
fn notes(replica: &Replica, meta: &tilolang_runtime::Meta, root: &ResourceRef) -> Vec<String> {
    let record = replica.host.store().record(&root.doc).unwrap();
    match record.get(&field_id(meta, "Notebook", "notes")) {
        Some(DynValue::Collection(items)) => {
            items.iter().map(|(id, _)| id.as_str().to_owned()).collect()
        }
        other => panic!("{other:?}"),
    }
}

/// The node of `kind` at or under `node`.
fn find<'a>(node: &'a tilo_ui_host::UiNode, kind: &str) -> Option<&'a tilo_ui_host::UiNode> {
    if node.kind == kind {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, kind))
}

/// What the notes the editor offers read as, in the order the view names them.
fn offered(editor: &tilo_ui_host::UiNode) -> Vec<(String, String)> {
    match editor.props.get("links") {
        Some(tilo_ui_host::Value::List(items)) => items
            .iter()
            .map(|item| match item {
                tilo_ui_host::Value::Link { reference, title } => {
                    (reference.clone(), title.clone())
                }
                other => panic!("a link, not {other:?}"),
            })
            .collect(),
        other => panic!("no links offered: {other:?}"),
    }
}

/// Names, as a window no workspace named anybody to reads them.
struct Names;

impl NameHost for Names {
    fn name(&self, who: &Principal) -> String {
        who.shown()
    }
}

/// The view of the note `note`, and the tree the window receives of it: its `[[` picker closed,
/// as a note opens.
fn window(replica: &Replica, note: &str) -> (ViewInstance, tilo_ui_host::UiNode) {
    let view = note_view(replica, note);
    let tree = rendered(replica, &view);
    (view, tree)
}

/// The view of the note `note`, its `[[` picker opened with `query` typed into it, and the tree
/// the window receives of it then.
fn picking(replica: &Replica, note: &str, query: &str) -> (ViewInstance, tilo_ui_host::UiNode) {
    let mut view = note_view(replica, note);
    let key = editor_key(&rendered(replica, &view));
    view.fire_with(&key, "links_open", Some(&Value::Bool(true)))
        .expect("the picker opens");
    view.fire_with(&key, "links_query", Some(&Value::text(query)))
        .expect("the text typed reaches the view");
    // What the person did runs the view again, as the window's next frame does.
    assert!(view.is_stale(), "what was typed reaches the view's state");
    view.poll_patches()
        .expect("the view runs with the picker open");
    let tree = rendered(replica, &view);
    (view, tree)
}

/// The view of the note `note`.
fn note_view(replica: &Replica, note: &str) -> ViewInstance {
    let package = replica.host.package(APP).unwrap();
    let ty = package
        .meta
        .types
        .iter()
        .find(|ty| ty.name == "Note")
        .unwrap()
        .id
        .clone();
    let target = ResourceRef {
        ty,
        ..ResourceRef::parse_id(note, String::new()).unwrap()
    };
    replica.host.open_resource(&target, None).unwrap()
}

/// The key of the note's editor in `tree`.
fn editor_key(tree: &tilo_ui_host::UiNode) -> String {
    find(tree, "ui.BlockEditor")
        .expect("the note's body is the editor")
        .key
        .clone()
}

/// The tree the window receives of `view`.
fn rendered(replica: &Replica, view: &ViewInstance) -> tilo_ui_host::UiNode {
    let names = Names;
    let convert = tilolang_tilo::instance::wire::Convert {
        names: &names,
        field_name: &|_| String::new(),
        cases: &|_| Vec::new(),
        toggles: &|_, _| None,
        invokes: &|_, _| None,
        embeds: &|reference| tilolang_tilo::instance::inline_view(&replica.host, &names, reference),
        preview: &|reference| {
            tilolang_tilo::instance::preview(&replica.host, &names, reference, &mut Vec::new())
        },
        link: &|at| tilolang_tilo::instance::link_of(&replica.host, at),
    };
    convert.node(view.tree())
}

#[test]
fn a_note_offers_its_notebooks_notes_by_the_titles_they_read_as_now() {
    let Workspace {
        mut ada,
        plan,
        groceries,
    } = workspace();
    // A note opens with its picker closed, and offers nothing: reading the notebook's notes is
    // the picker's work, not every frame of the document's.
    let (_view, tree) = window(&ada, &plan);
    let editor = find(&tree, "ui.BlockEditor").expect("the note's body is the editor");
    assert_eq!(
        offered(editor),
        [],
        "no note is read while the picker is closed"
    );

    let (_view, tree) = picking(&ada, &plan, "");
    let editor = find(&tree, "ui.BlockEditor").expect("the note's body is the editor");
    let links = offered(editor);
    assert_eq!(
        links
            .iter()
            .map(|(_, title)| title.as_str())
            .collect::<Vec<_>>(),
        ["Plan", "Groceries"],
        "the notebook's notes, each by its title: {links:?}"
    );
    assert!(
        links.iter().all(
            |(reference, title)| reference.contains("so.tilo.lang.notes") && reference != title
        ),
        "each is the catalogue's reference to the note, never its title: {links:?}"
    );
    // What is typed in the picker narrows them, as search finds them.
    let (_view, tree) = picking(&ada, &plan, "gro");
    assert_eq!(
        offered(find(&tree, "ui.BlockEditor").unwrap())
            .iter()
            .map(|(_, title)| title.as_str())
            .collect::<Vec<_>>(),
        ["Groceries"],
        "only the note holding what was typed"
    );

    // A note renamed later is offered by its new title: the view names the record, and how it
    // reads is read now.
    let package = ada.host.package(APP).unwrap();
    call(
        &mut ada,
        &package.meta,
        "Note",
        &groceries,
        "retitle",
        json!({ "title": "Shopping" }),
    );
    let (_view, tree) = picking(&ada, &plan, "");
    let editor = find(&tree, "ui.BlockEditor").expect("the note's body is the editor");
    assert_eq!(
        offered(editor)
            .iter()
            .map(|(_, title)| title.as_str())
            .collect::<Vec<_>>(),
        ["Plan", "Shopping"],
        "the new title, not the one it had when the view first ran"
    );
}

#[test]
fn a_reference_written_into_a_note_is_a_mention_that_linked_reports() {
    let Workspace {
        ada,
        plan,
        groceries,
    } = workspace();
    let (view, tree) = window(&ada, &plan);
    let key = find(&tree, "ui.BlockEditor")
        .expect("the note's body is the editor")
        .key
        .clone();

    // What choosing a note in the editor writes: a run that reads as the note's title, marked
    // as the reference to it (`ui.BlockEditor`'s own test drives the picker to this).
    let (_view, other) = picking(&ada, &groceries, "gro");
    let (reference, title) = offered(find(&other, "ui.BlockEditor").unwrap())
        .into_iter()
        .find(|(_, title)| title == "Groceries")
        .expect("the note it offers");
    let target = Reference::of_mark(&reference).expect("a reference");
    let mut body = RichValue::from_text("Groceries: ");
    body.push_reference(&title, &target);
    view.edit(&key, &Value::Rich(body)).expect("the body edits");

    let mentioned = ada
        .host
        .store()
        .linked(&ResourceRef::parse_id(&groceries, String::new()).unwrap())
        .unwrap();
    let plan_id = ResourceRef::parse_id(&plan, String::new())
        .unwrap()
        .id_string();
    assert!(
        mentioned
            .iter()
            .any(|link| link.how == LinkHow::Mention && link.of.id_string() == plan_id),
        "the note that mentions it, and how: {mentioned:?}"
    );
}

/// The window's view of the note `note`, and the tree the window receives of it: what
/// converting it read of the records its cards show is kept by the view, which runs again when
/// one changes.
fn live(
    replica: &Replica,
    note: &str,
) -> (
    std::rc::Rc<std::cell::RefCell<ViewInstance>>,
    tilo_ui_host::UiNode,
) {
    let package = replica.host.package(APP).unwrap();
    let ty = package
        .meta
        .types
        .iter()
        .find(|ty| ty.name == "Note")
        .unwrap()
        .id
        .clone();
    let target = ResourceRef {
        ty,
        ..ResourceRef::parse_id(note, String::new()).unwrap()
    };
    let view = replica.host.open_resource(&target, None).unwrap();
    let view = std::rc::Rc::new(std::cell::RefCell::new(view));
    let names = Names;
    let tree = tilolang_tilo::instance::window_tree(&replica.host, &names, &view);
    (view, tree)
}

/// What the editor of the note `note` shows of the record `reference`: its title and the start
/// of its content, as the window received them.
fn embed_of(
    replica: &Replica,
    view: &std::rc::Rc<std::cell::RefCell<ViewInstance>>,
    reference: &str,
) -> Option<(String, String)> {
    let values = tilolang_tilo::instance::shown_values(&replica.host, &Names, view);
    let rich = values.iter().find_map(|(_, value)| match value {
        tilo_ui_host::Value::Rich(rich) => Some(rich),
        _ => None,
    })?;
    let embed = rich
        .embeds
        .iter()
        .find(|embed| embed.reference == reference)?;
    let title = match &embed.view {
        tilo_render_protocol::patch::rich::InlineView::Resource { title, .. } => title.clone(),
        other => panic!("a record, not {other:?}"),
    };
    let start = embed
        .preview
        .as_ref()?
        .iter()
        .map(|run| run.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    Some((title, start))
}

#[test]
fn an_embedded_note_shows_its_start_links_back_and_follows_the_note() {
    let Workspace {
        mut ada,
        plan,
        groceries,
    } = workspace();
    let package = ada.host.package(APP).unwrap();
    // The note that is embedded says something of its own.
    let (groceries_view, tree) = live(&ada, &groceries);
    let key = find(&tree, "ui.BlockEditor")
        .expect("its body is the editor")
        .key
        .clone();
    groceries_view
        .borrow_mut()
        .edit(&key, &Value::Rich(RichValue::from_text("Oat milk, eggs")))
        .expect("the body writes");
    let meta = package.meta.clone();
    drop(package);

    // The note that embeds it: a paragraph that is nothing but a projection of it.
    let (view, tree) = live(&ada, &plan);
    let key = find(&tree, "ui.BlockEditor")
        .expect("the body is the editor")
        .key
        .clone();
    // The records a view offers are the ones its picker is open to show: the note chosen here
    // is the one its text then projects.
    let (_, picked) = picking(&ada, &plan, "Groceries");
    let (reference, _) = offered(find(&picked, "ui.BlockEditor").unwrap())
        .into_iter()
        .find(|(_, title)| title == "Groceries")
        .expect("the note it offers");
    let mut body = RichValue::from_text("Groceries");
    body.blocks[0].marks.push(tilo_doc::document::MarkSpan {
        start: 0,
        end: 9,
        name: tilolang_runtime::rich::PROJECTION.into(),
        value: tilo_doc::document::MarkValue::Str(reference.clone()),
    });
    view.borrow_mut()
        .edit(&key, &Value::Rich(body))
        .expect("the body writes");

    assert_eq!(
        embed_of(&ada, &view, &reference),
        Some(("Groceries".to_owned(), "Oat milk, eggs".to_owned())),
        "the card is titled by the note and shows the start of what it says"
    );

    // What links to it says so: the note that embeds it mentions it.
    let mentioned = ada
        .host
        .store()
        .linked(&ResourceRef::parse_id(&groceries, String::new()).unwrap())
        .unwrap();
    let plan_id = ResourceRef::parse_id(&plan, String::new())
        .unwrap()
        .id_string();
    assert!(
        mentioned
            .iter()
            .any(|link| link.how == LinkHow::Mention && link.of.id_string() == plan_id),
        "the note that embeds it, and how: {mentioned:?}"
    );

    // The embedded note says something else: the view showing the card runs again, and the
    // window is sent the card as it reads now.
    groceries_view
        .borrow_mut()
        .edit(&key, &Value::Rich(RichValue::from_text("Bread and cheese")))
        .expect("the body writes");
    let patches = view.borrow_mut().poll_patches().expect("the view runs");
    assert!(
        !patches.is_empty(),
        "the note it shows changed: the view runs again"
    );
    assert_eq!(
        embed_of(&ada, &view, &reference),
        Some(("Groceries".to_owned(), "Bread and cheese".to_owned())),
        "the card follows the note it shows"
    );

    // Renamed, the note keeps the card: the reference names it, not the title it had.
    call(
        &mut ada,
        &meta,
        "Note",
        &groceries,
        "retitle",
        json!({ "title": "Shopping" }),
    );
    assert!(view.borrow_mut().poll_patches().is_ok());
    assert_eq!(
        embed_of(&ada, &view, &reference),
        Some(("Shopping".to_owned(), "Bread and cheese".to_owned())),
        "the card reads the note's title now, and is still the same card"
    );
}

/// `note`'s window with `target` written into its body as a projection of it, as choosing the
/// note in the editor's picker writes it: its view, the tree the window receives of it, and the
/// reference the card it shows is of.
fn projecting(
    replica: &Replica,
    note: &str,
    title: &str,
) -> (
    std::rc::Rc<std::cell::RefCell<ViewInstance>>,
    tilo_ui_host::UiNode,
    String,
) {
    let (view, tree) = live(replica, note);
    let key = find(&tree, "ui.BlockEditor")
        .expect("the body is the editor")
        .key
        .clone();
    let (_, picked) = picking(replica, note, title);
    let (reference, _) = offered(find(&picked, "ui.BlockEditor").unwrap())
        .into_iter()
        .find(|(_, offered)| offered == title)
        .expect("the note it offers");
    let mut body = RichValue::from_text(title);
    body.blocks[0].marks.push(tilo_doc::document::MarkSpan {
        start: 0,
        end: title.chars().count(),
        name: tilolang_runtime::rich::PROJECTION.into(),
        value: tilo_doc::document::MarkValue::Str(reference.clone()),
    });
    view.borrow_mut()
        .edit(&key, &Value::Rich(body))
        .expect("the body writes");
    (view, tree, reference)
}

#[test]
fn an_embedded_note_shows_its_first_lines_and_follows_them() {
    let Workspace {
        ada,
        plan,
        groceries,
    } = workspace();
    // The embedded note says six lines, more than a card shows at once.
    let told = [
        "Oat milk, eggs, two lemons, and the bread the baker keeps back under the counter.",
        "Cheese from the stall by the river, the hard one, and something soft for the weekend.",
        "Coffee beans, the dark roast, ground for the machine rather than the pot this time.",
        "Apples for the crumble, the ones that keep, and a lemon for the end of the soup.",
        "Yoghurt, the plain one, and the small jars of the jam the grocer orders from the farm.",
        "Something green for Sunday, and candles for the table, and the good butter again.",
    ];
    let (groceries_view, tree) = live(&ada, &groceries);
    let groceries_key = find(&tree, "ui.BlockEditor")
        .expect("its body is the editor")
        .key
        .clone();
    groceries_view
        .borrow_mut()
        .edit(
            &groceries_key,
            &Value::Rich(RichValue::from_text(&told.join("\n"))),
        )
        .expect("the body writes");

    // The note that embeds it: a paragraph that is nothing but a projection of it.
    let (view, _tree, reference) = projecting(&ada, &plan, "Groceries");
    let Some((title, said)) = embed_of(&ada, &view, &reference) else {
        panic!("the card shows what it projects");
    };
    assert_eq!(
        title, "Groceries",
        "the card is titled by the note it projects"
    );
    let lines: Vec<&str> = said
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(
        lines.len() >= 3,
        "a card of it shows several lines of what it says, not one: {lines:?}"
    );
    assert_eq!(
        lines.first().map(|line| line.trim()),
        Some(told[0]),
        "the first line of the note, as it says it: {lines:?}"
    );
    assert!(
        lines.len() < told.len(),
        "the card shows the start of it, and the note reads on: {lines:?}"
    );

    // What it says changes: the card is what it says now, from its first line.
    groceries_view
        .borrow_mut()
        .edit(
            &groceries_key,
            &Value::Rich(RichValue::from_text(
                "Bread, cheese and apples, from the market.",
            )),
        )
        .expect("the body writes");
    assert!(view.borrow_mut().poll_patches().is_ok(), "the view runs");
    let Some((_, said)) = embed_of(&ada, &view, &reference) else {
        panic!("the card still shows what it projects");
    };
    assert_eq!(
        said.lines().next().map(str::trim),
        Some("Bread, cheese and apples, from the market."),
        "the card follows the note it shows"
    );
}
