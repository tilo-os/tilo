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
