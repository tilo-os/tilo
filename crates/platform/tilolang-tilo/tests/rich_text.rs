//! `RichText` fields in a room: concurrent edits with references merge as splices and marks,
//! and search finds a message by its text and by the titles it refers to.

use automerge::AutoCommit;
use serde_json::json;
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::record::DynValue;

use crate::fixtures::{Replica, field_id};

const SOURCE: &str = include_str!("../../../lang/tilolang/tests/fixtures/richtext.tl");
const APP: &str = "so.tilo.lang.richtext";

struct Room {
    ada: Replica,
    grace: Replica,
    meta: tilolang_runtime::Meta,
    root: String,
}

fn room() -> Room {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(SOURCE, "")]);
    let package = packages[0].clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let grace = ada.fork("grace", &[(SOURCE, &package.ids)]);
    Room {
        ada,
        grace,
        meta: package.meta,
        root: root.doc,
    }
}

fn plan(
    replica: &mut Replica,
    meta: &tilolang_runtime::Meta,
    ty: &str,
    resource: &str,
    name: &str,
    args: serde_json::Value,
) {
    let action = meta.actions.iter().find(|a| a.name == name).unwrap();
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
        })
        .unwrap();
    replica.apply(&planned);
}

fn items(replica: &Replica, meta: &tilolang_runtime::Meta, root: &str, field: &str) -> Vec<String> {
    let record = replica.host.store().record(root).unwrap();
    match record.get(&field_id(meta, "Board", field)) {
        Some(DynValue::Collection(items)) => {
            items.iter().map(|(id, _)| id.as_str().to_owned()).collect()
        }
        other => panic!("{other:?}"),
    }
}

fn body(replica: &Replica, meta: &tilolang_runtime::Meta, root: &str, post: &str) -> String {
    let record = replica.host.store().record(root).unwrap();
    let posts = field_id(meta, "Board", "posts");
    let Some(DynValue::Collection(items)) = record.get(&posts) else {
        panic!("no posts")
    };
    let item = items
        .iter()
        .find(|(id, _)| id.as_str() == post)
        .map(|(_, item)| item.clone())
        .unwrap();
    match item.get(&field_id(meta, "Post", "body")) {
        Some(DynValue::RichText(rich)) => rich.text(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn two_writers_edits_with_references_merge_in_a_room() {
    let Room {
        mut ada,
        mut grace,
        meta,
        root,
    } = room();
    plan(
        &mut ada,
        &meta,
        "Board",
        &root,
        "add_task",
        json!({"title": "Ship it"}),
    );
    plan(
        &mut ada,
        &meta,
        "Board",
        &root,
        "say",
        json!({"text": "Plan for today"}),
    );
    ada.sync_with(&mut grace);
    let posts = items(&ada, &meta, &root, "posts");
    let tasks = items(&ada, &meta, &root, "tasks");
    let post = format!("{root}/{}:{}", field_id(&meta, "Board", "posts"), posts[0]);
    let task = format!("{root}/{}:{}", field_id(&meta, "Board", "tasks"), tasks[0]);

    plan(
        &mut ada,
        &meta,
        "Post",
        &post,
        "append",
        json!({"line": "Then lunch"}),
    );
    plan(
        &mut grace,
        &meta,
        "Post",
        &post,
        "refer",
        json!({"t": task}),
    );
    ada.sync_with(&mut grace);

    let said = body(&ada, &meta, &root, &posts[0]);
    assert_eq!(said, body(&grace, &meta, &root, &posts[0]));
    assert_eq!(said, "Plan for today see Ship it\nThen lunch");
    assert_eq!(
        ada.host.store().record(&root).unwrap(),
        grace.host.store().record(&root).unwrap()
    );

    // Search finds the message by its text and by the title of the task it refers to.
    let hits = tilolang_tilo::search::every_hit(&ada.host);
    let found = |words: &str| {
        hits.iter()
            .filter(|hit| hit.texts.iter().any(|text| text.contains(words)))
            .map(|hit| hit.type_name.clone())
            .collect::<Vec<_>>()
    };
    assert!(
        found("Then lunch").contains(&"Post".to_owned()),
        "{hits:#?}"
    );
    let by_title = found("Ship it");
    assert!(by_title.contains(&"Post".to_owned()), "{hits:#?}");
    assert!(by_title.contains(&"Task".to_owned()), "{hits:#?}");
    // The post's hit carries what it refers to, in the one form: the system's index finds it by
    // what the task is called now, and keeps it as one of the task's backlinks.
    let post = hits.iter().find(|hit| hit.type_name == "Post").unwrap();
    let referred: Vec<Option<String>> = post
        .references
        .iter()
        .map(|reference| {
            let reference = tilolang_runtime::rich::Reference::of_mark(reference)?;
            tilolang_runtime::rich::resource_id(&reference).map(str::to_owned)
        })
        .collect();
    // The task was named by its older path; it is referred to by its id.
    assert_eq!(referred, [Some(tilolang_runtime::value::flat_id(&task))]);
}

#[test]
fn a_person_mentioned_in_rich_text_is_told_through_the_mention_notices() {
    let Room {
        mut ada,
        mut grace,
        meta,
        root,
    } = room();
    plan(
        &mut ada,
        &meta,
        "Board",
        &root,
        "add_task",
        json!({"title": "Fix the build"}),
    );
    let tasks = items(&ada, &meta, &root, "tasks");
    let task = format!("{root}/{}:{}", field_id(&meta, "Board", "tasks"), tasks[0]);
    // Ada asks Grace about the task, as her composer writes it: a run naming Grace and one
    // referring to the task.
    let mut asked = tilolang_runtime::rich::RichValue::from_text("Could ");
    asked.push_reference(
        "@Grace",
        &tilolang_runtime::rich::Reference::Person("grace".into()),
    );
    asked.push_text(" look at ");
    asked.push_reference(
        "Fix the build",
        &tilolang_runtime::rich::Reference::resource(
            APP,
            format!("tl.{APP}.Task"),
            format!("tl:{APP}:{task}"),
        ),
    );
    let encoded = tilolang_runtime::value::to_json(&tilolang_runtime::Value::Rich(asked)).unwrap();
    plan(
        &mut ada,
        &meta,
        "Board",
        &root,
        "post",
        json!({"body": encoded}),
    );
    ada.sync_with(&mut grace);
    let posts = items(&grace, &meta, &root, "posts");
    assert_eq!(
        body(&grace, &meta, &root, &posts[0]),
        "Could @Grace look at Fix the build"
    );

    // The room's record holds the mention as every app's does: the system finds it without
    // the app, and the task reference is a reference, not a person.
    let found = ui::mentions::in_record(&grace.doc);
    let people: Vec<&str> = found.iter().map(|m| m.person.as_str()).collect();
    assert_eq!(people, ["grace"], "{found:#?}");
    assert!(found[0].excerpt.contains("look at"), "{found:#?}");

    let look = |doc| ui::mentions::Look {
        app: APP,
        record: "",
        kind: "tilolang",
        doc,
        tree: None,
        quiet: false,
        read: &ui::mentions::reading_nothing,
    };
    let name = |id: &str| id.to_owned();
    let mut grace_inbox = ui::mentions::MentionInbox::new(["grace".to_owned()]);
    let told = grace_inbox.look(&look(&grace.doc), &name);
    assert_eq!(told.show.len(), 1, "Grace is told once: {:#?}", told.show);
    assert!(
        grace_inbox.look(&look(&grace.doc), &name).show.is_empty(),
        "and never again"
    );
    let mut ada_inbox = ui::mentions::MentionInbox::new(["ada".to_owned()]);
    assert!(
        ada_inbox.look(&look(&ada.doc), &name).show.is_empty(),
        "nobody else is told"
    );
}

struct Names;

impl tilolang_tilo::runtime::NameHost for Names {
    fn name(&self, who: &tilolang_tilo::runtime::Principal) -> String {
        match who.0.as_str() {
            "grace" => "Grace Hopper".to_owned(),
            other => other.to_owned(),
        }
    }
}

#[test]
fn a_reference_reads_as_the_readers_inline_view_under_what_they_hold() {
    use tilo_render_protocol::patch::rich::InlineView;
    use tilolang_runtime::rich::Reference;
    use tilolang_tilo::instance::inline_view;
    let Room {
        mut ada,
        meta,
        root,
        ..
    } = room();
    plan(
        &mut ada,
        &meta,
        "Board",
        &root,
        "add_task",
        json!({"title": "Fix the build"}),
    );
    let tasks = items(&ada, &meta, &root, "tasks");
    let task = format!("{root}/{}:{}", field_id(&meta, "Board", "tasks"), tasks[0]);
    let to_task = Reference::resource(APP, format!("tl.{APP}.Task"), format!("tl:{APP}:{task}"));

    assert_eq!(
        inline_view(&ada.host, &Names, &Reference::Person("grace".into())),
        InlineView::Person {
            identity: "grace".into(),
            name: "Grace Hopper".into()
        },
        "a person by what the room calls them"
    );
    assert_eq!(
        inline_view(&ada.host, &Names, &to_task),
        InlineView::Resource {
            title: "Fix the build".into(),
            icon: None,
            detail: None
        }
    );
    // Its title is the task's now, never the one the text was written with.
    plan(
        &mut ada,
        &meta,
        "Task",
        &task,
        "rename",
        json!({"title": "Fix the release build"}),
    );
    assert_eq!(
        inline_view(&ada.host, &Names, &to_task),
        InlineView::Resource {
            title: "Fix the release build".into(),
            icon: None,
            detail: None
        }
    );
    // A record this reader does not hold (it is sent only to its readers) is only there.
    let elsewhere = Reference::resource(
        APP,
        format!("tl.{APP}.Task"),
        format!("tl:{APP}:not-shared/f1:t9"),
    );
    assert_eq!(
        inline_view(&ada.host, &Names, &elsewhere),
        InlineView::Unreadable
    );
    // An older form reads the same.
    let old = Reference::of_mark(&format!("tl:{APP}:{task}")).unwrap();
    assert!(matches!(
        inline_view(&ada.host, &Names, &old),
        InlineView::Resource { .. }
    ));
    // Another app's resource reads as its run, where only that app can say how it reads.
    let channel = Reference::resource("so.tilo.chat", "chat.channel", "chat.channel:general");
    assert_eq!(
        inline_view(&ada.host, &Names, &channel),
        InlineView::Elsewhere
    );
}

/// Every text `node` and the nodes inside it show.
fn texts_of(node: &tilolang_runtime::UiNode, out: &mut Vec<String>) {
    out.extend(node.props.values().filter_map(|value| match value {
        tilolang_runtime::Value::Text(text) => Some(text.clone()),
        _ => None,
    }));
    for child in &node.children {
        texts_of(child, out);
    }
}

/// What `replica` reads of `post` as a line: the post's `excerpt`, run as its person.
fn excerpt(replica: &Replica, meta: &tilolang_runtime::Meta, post: &str) -> String {
    let action = meta
        .actions
        .iter()
        .find(|a| a.name == "excerpt_of")
        .unwrap();
    let ty = meta.types.iter().find(|t| t.name == "Post").unwrap();
    let at = tilolang_runtime::ResourceRef::parse_id(post, ty.id.clone()).unwrap();
    match replica
        .host
        .runtime()
        .invoke_action(
            &tilolang_runtime::Principal::new(replica.name.clone()),
            (
                action.id.as_str(),
                vec![tilolang_runtime::Value::resource(&at)],
            ),
        )
        .unwrap()
    {
        tilolang_runtime::InvokeOutcome::Done(tilolang_runtime::Value::Text(text)) => text,
        other => panic!("{other:?}"),
    }
}

/// Everything `replica`'s person is shown or told of `post` as plain text: its excerpt (an
/// action's answer), the board's list row, what search finds and shows, the label the post is
/// published under, what an agent reads of it, and the excerpt of the notice of the mention in
/// it. Each is a line, named by its path.
fn plain_paths(
    replica: &Replica,
    meta: &tilolang_runtime::Meta,
    post: &str,
    view: &mut tilolang_runtime::ViewInstance,
) -> Vec<(&'static str, String)> {
    use tilolang_runtime::store::Store as _;
    view.poll_patches().unwrap();
    let mut row = Vec::new();
    texts_of(view.tree(), &mut row);
    let hits = tilolang_tilo::search::every_hit(&replica.host);
    let found = hits
        .iter()
        .filter(|hit| hit.resource == format!("tl:{APP}:{post}"))
        .flat_map(|hit| std::iter::once(hit.title.clone()).chain(hit.texts.clone()))
        .collect::<Vec<_>>()
        .join(" | ");
    let ty = meta.types.iter().find(|t| t.name == "Post").unwrap();
    let at = tilolang_runtime::ResourceRef::parse_id(post, ty.id.clone()).unwrap();
    let read =
        |reference: &tilolang_runtime::rich::Reference| replica.host.store().reading(reference);
    let notices = ui::mentions::in_record_for(&replica.doc, &read)
        .into_iter()
        .map(|mention| mention.excerpt)
        .collect::<Vec<_>>()
        .join(" | ");
    vec![
        ("excerpt", excerpt(replica, meta, post)),
        ("list row", row.join(" | ")),
        ("search", found),
        ("label", replica.host.label_at(&at)),
        (
            "agent read",
            replica
                .host
                .read_content(Some(&format!("tl:{APP}:{post}")))
                .unwrap()
                .to_string(),
        ),
        ("notice", notices),
    ]
}

/// A room where Ada posts to Grace about a task, "Acquisition plan", that only Ada holds:
/// Grace's store holds the post, never the task. The room, the task's path and the post's.
fn a_post_about_a_task_only_ada_holds() -> (Room, String, String) {
    let Room {
        mut ada,
        mut grace,
        meta,
        root,
    } = room();
    plan(
        &mut ada,
        &meta,
        "Board",
        &root,
        "add_task",
        json!({"title": "Acquisition plan"}),
    );
    let tasks = items(&ada, &meta, &root, "tasks");
    let task_id = tasks[0].clone();
    let task = format!("{root}/{}:{task_id}", field_id(&meta, "Board", "tasks"));
    // Ada tells Grace where the notes are, referring to the task by the title it has now.
    let mut said = tilolang_runtime::rich::RichValue::default();
    said.push_reference(
        "@Grace",
        &tilolang_runtime::rich::Reference::Person("grace".into()),
    );
    said.push_text(" the notes are in ");
    said.push_reference(
        "Acquisition plan",
        &tilolang_runtime::rich::Reference::resource(
            APP,
            format!("tl.{APP}.Task"),
            format!("tl:{APP}:{task}"),
        ),
    );
    let encoded = tilolang_runtime::value::to_json(&tilolang_runtime::Value::Rich(said)).unwrap();
    plan(
        &mut ada,
        &meta,
        "Board",
        &root,
        "post",
        json!({"body": encoded}),
    );
    ada.sync_with(&mut grace);
    // Grace does not hold the task: her store holds the post, never the task it refers to.
    plan(
        &mut grace,
        &meta,
        "Board",
        &root,
        "drop_task",
        json!({"t": task}),
    );
    let posts = items(&ada, &meta, &root, "posts");
    let post = format!("{root}/{}", posts[0]);
    // What was written still says the title: nothing rewrites the stored text.
    assert_eq!(
        body(&grace, &meta, &root, &posts[0]),
        "@Grace the notes are in Acquisition plan"
    );
    (
        Room {
            ada,
            grace,
            meta,
            root,
        },
        task,
        post,
    )
}

#[test]
fn a_referenced_title_reaches_only_the_readers_who_hold_it_on_every_plain_text_path() {
    let (
        Room {
            mut ada,
            grace,
            meta,
            ..
        },
        task,
        post,
    ) = a_post_about_a_task_only_ada_holds();
    // The post as a list shows it: its row view, open on each person's machine.
    let post_ty = meta.types.iter().find(|t| t.name == "Post").unwrap();
    let post_at = tilolang_runtime::ResourceRef::parse_id(&post, post_ty.id.clone()).unwrap();
    let row = meta.row_view(&post_ty.id).unwrap().to_owned();
    let mut ada_view = ada.host.open_resource(&post_at, Some(&row)).unwrap();
    let mut grace_view = grace.host.open_resource(&post_at, Some(&row)).unwrap();
    for (path, text) in plain_paths(&ada, &meta, &post, &mut ada_view) {
        assert!(text.contains("Acquisition plan"), "Ada's {path}: {text}");
    }
    for (path, text) in plain_paths(&grace, &meta, &post, &mut grace_view) {
        assert!(!text.contains("Acquisition"), "Grace's {path}: {text}");
        assert!(
            text.contains("Not shared with you"),
            "Grace's {path}: {text}"
        );
    }
    assert_eq!(
        excerpt(&grace, &meta, &post),
        "@Grace the notes are in Not shared with you"
    );
    // Search never matches her query against the title she cannot read.
    assert!(
        tilolang_tilo::search::every_hit(&grace.host)
            .iter()
            .all(|hit| hit.texts.iter().all(|text| !text.contains("Acquisition"))),
    );

    // Ada renames the task: every path reads its title now for her, and still nothing of it for
    // Grace, who is not sent the rename.
    plan(
        &mut ada,
        &meta,
        "Task",
        &task,
        "rename",
        json!({"title": "Merger timeline"}),
    );
    for (path, text) in plain_paths(&ada, &meta, &post, &mut ada_view) {
        assert!(text.contains("Merger timeline"), "Ada's {path}: {text}");
        assert!(!text.contains("Acquisition"), "Ada's {path}: {text}");
    }
    for (path, text) in plain_paths(&grace, &meta, &post, &mut grace_view) {
        assert!(
            !text.contains("Acquisition") && !text.contains("Merger"),
            "Grace's {path}: {text}"
        );
        assert!(
            text.contains("Not shared with you"),
            "Grace's {path}: {text}"
        );
    }
    // The notice Grace is told says no title either.
    let read = |reference: &tilolang_runtime::rich::Reference| {
        tilolang_runtime::store::Store::reading(grace.host.store(), reference)
    };
    let mut inbox = ui::mentions::MentionInbox::new(["grace".to_owned()]);
    let told = inbox.look(
        &ui::mentions::Look {
            app: APP,
            record: "",
            kind: "tilolang",
            doc: &grace.doc,
            tree: None,
            quiet: false,
            read: &read,
        },
        &|id: &str| id.to_owned(),
    );
    assert_eq!(told.show.len(), 1, "{:#?}", told.show);
    assert_eq!(
        told.show[0].body,
        "@Grace the notes are in Not shared with you"
    );
}
