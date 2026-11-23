//! `RichText` fields in a workspace: a message's body reads as the text it holds and by the
//! titles it refers to, and search finds a message by its text and by the titles it refers to.

use crate::fixtures::Workspace;
use serde_json::json;

use crate::fixtures::{Replica, field_id};

const SOURCE: &str = include_str!("../../tilolang/tests/fixtures/richtext.tl");
const APP: &str = "so.tilo.lang.richtext";

struct Ws {
    ada: Replica,
    grace: Replica,
    meta: tilolang_runtime::Meta,
    root: String,
}

fn workspace() -> Ws {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[(SOURCE, "")]);
    let package = packages[0].clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let grace = ada.fork("grace", &[(SOURCE, &package.ids)]);
    Ws {
        ada,
        grace,
        meta: package.meta,
        root: root.doc,
    }
}

fn plan(
    replica: &Replica,
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
        .unwrap()
        .id
        .clone();
    let args = args.as_object().cloned().unwrap_or_default();
    let kind = format!("tl.{APP}.{ty}");
    replica
        .carry(APP, &kind, &action, Some(resource), &args, None, None)
        .unwrap();
}

fn items(replica: &Replica, meta: &tilolang_runtime::Meta, field: &str) -> Vec<String> {
    let field = field_id(meta, "Board", field);
    match replica
        .host
        .runtime()
        .read(&replica.host.root_of(APP).unwrap(), &field)
    {
        Ok(tilolang_runtime::Value::List(items)) => items
            .into_iter()
            .map(|item| match item {
                tilolang_runtime::Value::Ref(r) => r.target.item.clone().unwrap_or_default(),
                other => panic!("an item: {other:?}"),
            })
            .collect(),
        other => panic!("{other:?}"),
    }
}

/// What `post`'s body says, as its field holds it.
fn body(replica: &Replica, meta: &tilolang_runtime::Meta, root: &str, post: &str) -> String {
    let at = post_at(meta, root, post);
    match replica.host.runtime().read(&at, "body") {
        Ok(tilolang_runtime::Value::Rich(rich)) => rich.written_text(),
        other => panic!("{other:?}"),
    }
}

/// The resource `post` (its id, in the board `root`) is.
fn post_at(meta: &tilolang_runtime::Meta, root: &str, post: &str) -> tilolang_runtime::ResourceRef {
    let board_ty = meta.types.iter().find(|t| t.name == "Board").unwrap();
    let post_ty = meta.types.iter().find(|t| t.name == "Post").unwrap();
    tilolang_runtime::ResourceRef::root(root, board_ty.id.clone()).item(post, post_ty.id.clone())
}

tilo_test_env::scenario! {
    fn a_reference_in_rich_text_reads_as_its_reader_holds_it;

    phase two_writers_edits_with_references_read_in_a_workspace;
    {
        let Ws {
            ada,
            grace,
            meta,
            root,
        } = workspace();
        plan(
            &ada,
            &meta,
            "Board",
            &root,
            "add_task",
            json!({"title": "Ship it"}),
        );
        plan(
            &ada,
            &meta,
            "Board",
            &root,
            "say_text",
            json!({"text": "Plan for today"}),
        );
        ada.sync_with(&grace);
        let posts = items(&ada, &meta, "posts");
        let tasks = items(&ada, &meta, "tasks");
        let post = format!("{root}/{}:{}", field_id(&meta, "Board", "posts"), posts[0]);
        let task = format!("{root}/{}:{}", field_id(&meta, "Board", "tasks"), tasks[0]);

        plan(
            &ada,
            &meta,
            "Post",
            &post,
            "append",
            json!({"line": "Then lunch"}),
        );
        // Grace writes her reference into the body she now reads.
        ada.sync_with(&grace);
        plan(
            &grace,
            &meta,
            "Post",
            &post,
            "refer",
            json!({"t": task}),
        );
        ada.sync_with(&grace);

        // Every device reads the text both wrote, and the task the text refers to.
        let said = body(&ada, &meta, &root, &posts[0]);
        assert_eq!(said, body(&grace, &meta, &root, &posts[0]));
        assert!(said.contains("Plan for today"), "{said}");
        assert!(said.contains("Then lunch"), "{said}");
        assert!(said.contains("Ship it"), "{said}");

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

    phase a_reference_reads_as_the_readers_inline_view_under_what_they_hold;
    {
        use tilo_render_protocol::patch::rich::InlineView;
        use tilolang_runtime::rich::Reference;
        use tilolang_tilo::instance::inline_view;
        let Ws {
            ada,
            meta,
            root,
            ..
        } = workspace();
        plan(
            &ada,
            &meta,
            "Board",
            &root,
            "add_task",
            json!({"title": "Fix the build"}),
        );
        let tasks = items(&ada, &meta, "tasks");
        let task = format!("{root}/{}:{}", field_id(&meta, "Board", "tasks"), tasks[0]);
        let to_task = Reference::resource(APP, format!("tl.{APP}.Task"), format!("tl:{APP}:{task}"));

        assert_eq!(
            inline_view(&ada.host, &Names, &Reference::Member(tilolang_tilo::runtime::ids::MemberId::person("grace"))),
            InlineView::Person {
                identity: tilolang_tilo::runtime::ids::MemberId::person("grace"),
                name: "Grace Hopper".into()
            },
            "a person by what the workspace calls them"
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
            &ada,
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
        // A document this reader does not hold (it is sent only to its readers) is only there.
        let elsewhere = Reference::resource(
            APP,
            format!("tl.{APP}.Task"),
            format!("tl:{APP}:not-shared/f1:t9"),
        );
        assert_eq!(
            inline_view(&ada.host, &Names, &elsewhere),
            InlineView::Unreadable
        );
        // Another app's resource reads as its run, where only that app can say how it reads.
        let channel = Reference::resource("so.tilo.chat", "chat.channel", "chat.channel:general");
        assert_eq!(
            inline_view(&ada.host, &Names, &channel),
            InlineView::Elsewhere
        );
    }

    phase a_referenced_title_reaches_only_the_readers_who_hold_it_on_every_plain_text_path;
    {
        let (Ws { ada, grace, meta, .. }, task, post) = a_post_about_a_task_only_ada_holds();
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
            &ada,
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
    }
}

#[test]
fn a_person_mentioned_in_rich_text_is_held_by_the_text() {
    let Ws {
        ada,
        grace,
        meta,
        root,
    } = workspace();
    plan(
        &ada,
        &meta,
        "Board",
        &root,
        "add_task",
        json!({"title": "Fix the build"}),
    );
    let tasks = items(&ada, &meta, "tasks");
    let task = format!("{root}/{}:{}", field_id(&meta, "Board", "tasks"), tasks[0]);
    // Ada asks Grace about the task, as her composer writes it: a run naming Grace and one
    // referring to the task.
    let mut asked = tilolang_runtime::rich::RichValue::from_text("Could ");
    asked.push_reference(
        "@Grace",
        &tilolang_runtime::rich::Reference::Member(tilolang_runtime::ids::MemberId::person(
            "grace",
        )),
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
        &ada,
        &meta,
        "Board",
        &root,
        "post",
        json!({"body": encoded}),
    );
    ada.sync_with(&grace);
    let posts = items(&grace, &meta, "posts");
    assert_eq!(
        body(&grace, &meta, &root, &posts[0]),
        "Could @Grace look at Fix the build"
    );

    // The text itself holds the mention: the person it names, and the task as a reference, not a
    // person.
    let at = post_at(&meta, &root, &posts[0]);
    let rich = match grace.host.runtime().read(&at, "body").unwrap() {
        tilolang_runtime::Value::Rich(rich) => rich,
        other => panic!("{other:?}"),
    };
    assert!(
        rich.mentions(&tilolang_runtime::ids::MemberId::person("grace")),
        "the text names grace"
    );
    let references = rich.references();
    let people: Vec<&str> = references
        .iter()
        .filter_map(|reference| match reference {
            tilolang_runtime::rich::Reference::Member(member) => Some(member.id()),
            tilolang_runtime::rich::Reference::Resource(_) => None,
        })
        .collect();
    assert_eq!(people, ["grace"], "the task reference is no person");
    // Ada wrote it: the mention names grace alone.
    assert!(
        !rich.mentions(&tilolang_runtime::ids::MemberId::person("ada")),
        "the author is not mentioned"
    );
}

struct Names;

impl tilolang_tilo::runtime::NameHost for Names {
    fn name(&self, who: &tilolang_tilo::runtime::Actor) -> String {
        match who.id() {
            "grace" => "Grace Hopper".to_owned(),
            other => other.to_owned(),
        }
    }
}

/// Every text `node` and the nodes inside it show.
fn texts_of(node: &tilolang_runtime::UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            tilolang_runtime::Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
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
            &tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
                replica.name.clone(),
            )),
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

/// Everything `replica`'s person is shown of `post` as plain text: its excerpt (an action's
/// answer), the board's list row, what search finds and shows, the label the post is published
/// under, and what an agent reads of it. Each is a line, named by its path.
fn plain_paths(
    replica: &Replica,
    meta: &tilolang_runtime::Meta,
    post: &str,
    view: &mut tilolang_runtime::ViewInstance,
) -> Vec<(&'static str, String)> {
    view.poll_patches().unwrap();
    let mut row = Vec::new();
    row.extend(texts_of(view.tree()));
    let hits = tilolang_tilo::search::every_hit(&replica.host);
    let found = hits
        .iter()
        .filter(|hit| hit.resource == format!("tl:{APP}:{post}"))
        .flat_map(|hit| std::iter::once(hit.title.clone()).chain(hit.texts.clone()))
        .collect::<Vec<_>>()
        .join(" | ");
    let ty = meta.types.iter().find(|t| t.name == "Post").unwrap();
    let at = tilolang_runtime::ResourceRef::parse_id(post, ty.id.clone()).unwrap();
    vec![
        ("excerpt", excerpt(replica, meta, post)),
        ("list row", row.join(" | ")),
        ("search", found),
        ("label", replica.host.label_at(&at)),
        (
            "agent read",
            replica
                .host
                .answer_read(
                    &serde_json::json!({ "item": format!("tl:{APP}:{post}") }),
                    replica.host.viewer(),
                )
                .unwrap()
                .to_string(),
        ),
    ]
}

/// A workspace where Ada posts to Grace about a task, "Acquisition plan", that only Ada holds:
/// Grace's store holds the post, never the task. The workspace, the task's path and the post's.
fn a_post_about_a_task_only_ada_holds() -> (Ws, String, String) {
    let Ws {
        ada,
        grace,
        meta,
        root,
    } = workspace();
    plan(
        &ada,
        &meta,
        "Board",
        &root,
        "add_task",
        json!({"title": "Acquisition plan"}),
    );
    let tasks = items(&ada, &meta, "tasks");
    let task_id = tasks[0].clone();
    let task = format!("{root}/{}:{task_id}", field_id(&meta, "Board", "tasks"));
    // Ada tells Grace where the notes are, referring to the task by the title it has now.
    let mut said = tilolang_runtime::rich::RichValue::default();
    said.push_reference(
        "@Grace",
        &tilolang_runtime::rich::Reference::Member(tilolang_runtime::ids::MemberId::person(
            "grace",
        )),
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
        &ada,
        &meta,
        "Board",
        &root,
        "post",
        json!({"body": encoded}),
    );
    ada.sync_with(&grace);
    // Grace does not hold the task: her store holds the post, never the task it refers to.
    plan(
        &grace,
        &meta,
        "Board",
        &root,
        "drop_task",
        json!({"t": task}),
    );
    let posts = items(&ada, &meta, "posts");
    let post = format!("{root}/{}", posts[0]);
    // What was written still says the title: nothing rewrites the stored text.
    assert_eq!(
        body(&grace, &meta, &root, &posts[0]),
        "@Grace the notes are in Acquisition plan"
    );
    (
        Ws {
            ada,
            grace,
            meta,
            root,
        },
        task,
        post,
    )
}
