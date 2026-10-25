//! The library on its own: verbs, field edits, the invariant, merging,
//! the published tree and search, through the same door the host uses.

use super::*;
use crate::model::Language;
use ui::tilo_actions::{ActionSpec, Resource as Published};
use ui::tilo_resource::{Edit, Replica, Resource};

fn edit(library: &mut Replica<Snippets>, edit: &Edit<Snippets>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    library
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn create(library: &mut Replica<Snippets>, title: &str, folder: Option<&str>) -> Id {
    library
        .apply(Snippets::NEW_SNIPPET.with(snippets::NewSnippet {
            title: title.to_owned(),
            folder: folder.map(str::to_owned),
        }))
        .unwrap();
    titled(library, title.trim())
}

fn titled(library: &Replica<Snippets>, title: &str) -> Id {
    library
        .state()
        .unwrap()
        .snippets
        .iter()
        .find(|(_, s)| s.title == title)
        .map_or_else(|| panic!("a snippet titled {title}"), |(id, _)| id.clone())
}

fn snippet(library: &Replica<Snippets>, id: &Id) -> Snippet {
    library.state().unwrap().get(id).cloned().unwrap()
}

fn child<'a>(tree: &'a Published, id: &str) -> &'a Published {
    tree.children
        .iter()
        .find(|child| child.id.as_str() == id)
        .unwrap_or_else(|| panic!("{id} is published"))
}

fn action<'a>(resource: &'a Published, id: &str) -> &'a ActionSpec {
    resource
        .actions
        .iter()
        .find(|spec| spec.id == id)
        .unwrap_or_else(|| panic!("{} offers {id}", resource.id.as_str()))
}

/// Apply what `spec` builds for `arguments`, as the door does for an action
/// a published child offers.
fn invoke_spec(library: &mut Replica<Snippets>, spec: &ActionSpec, arguments: serde_json::Value) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    let encoded = spec.build_json(&arguments).expect("the arguments bind");
    let action: <Snippets as Resource>::Action =
        serde_json::from_slice(&encoded.payload).expect("one of the library's actions");
    library.apply_action(&action).unwrap();
}

/// Plain fields change through field edits at their paths; the verbs do
/// what is more than setting a field.
#[test]
fn verbs_and_field_edits_change_one_snippet() {
    let mut library = Replica::<Snippets>::new();
    let deploy = create(&mut library, " Deploy ", Some(" Ops "));
    let created = snippet(&library, &deploy);
    assert_eq!(
        (created.title.as_str(), created.folder.0.as_str()),
        ("Deploy", "Ops"),
        "a new snippet's title and folder are trimmed"
    );

    let paths = Snippets::paths().snippets.item(&deploy);
    edit(&mut library, &paths.favorite.set(true)).unwrap();
    edit(&mut library, &paths.language.set(Language::Shell)).unwrap();
    edit(&mut library, &paths.tags.insert(&"k8s".to_owned())).unwrap();
    edit(&mut library, &paths.folder.set(Atom("Release".into()))).unwrap();
    edit(&mut library, &paths.title.replace("Deploy app")).unwrap();
    edit(&mut library, &paths.body.splice(0, 0, "kubectl apply")).unwrap();
    library
        .apply(Snippets::RECORD_USE.with(deploy.clone()))
        .unwrap();
    let changed = snippet(&library, &deploy);
    assert!(changed.favorite);
    assert_eq!(changed.language, Language::Shell);
    assert!(changed.tags.contains(&"k8s".to_owned()));
    assert_eq!(changed.folder.0, "Release");
    assert_eq!(changed.title, "Deploy app");
    assert_eq!(changed.body, "kubectl apply");
    assert_eq!(changed.uses.get(), 1);
    assert!(
        edit(&mut library, &paths.uses.add(5)).is_err(),
        "only copying counts a use"
    );

    library
        .apply(Snippets::DUPLICATE.with(snippets::Duplicate {
            snippet: deploy.clone(),
            title: Some("Deploy app (copy)".into()),
        }))
        .unwrap();
    let state = library.state().unwrap();
    let ids: Vec<&Id> = state.snippets.ids().collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], &deploy, "the copy is placed after its original");
    let copy = state.get(ids[1]).unwrap();
    assert_eq!(copy.title, "Deploy app (copy)");
    assert_eq!(copy.body, "kubectl apply");
    assert!(!copy.favorite && copy.uses.get() == 0);

    // Undo takes a deletion back, so it runs at once.
    library
        .apply(Snippets::DELETE.with(deploy.clone()))
        .expect("an undoable delete runs at once");
    assert!(library.state().unwrap().get(&deploy).is_none());
    assert!(
        library.apply(Snippets::RECORD_USE.with(deploy)).is_err(),
        "a snippet that is gone is not counted"
    );
}

/// Every way in runs the snippet's invariant: a blank or padded tag, a
/// padded folder are refused whole.
#[test]
fn the_invariant_refuses_blank_tags_and_padded_folders() {
    let mut library = Replica::<Snippets>::new();
    let id = create(&mut library, "Deploy", None);
    let paths = Snippets::paths().snippets.item(&id);
    let before = library.state().unwrap();
    assert!(edit(&mut library, &paths.tags.insert(&" ".to_owned())).is_err());
    assert!(edit(&mut library, &paths.tags.insert(&" k8s".to_owned())).is_err());
    assert!(edit(&mut library, &paths.folder.set(Atom("Ops ".into()))).is_err());
    edit(&mut library, &paths.tags.insert(&"k8s".to_owned())).unwrap();
    assert!(
        library
            .apply(Snippets::RENAME_TAG.with(snippets::RenameTag {
                tag: "k8s".into(),
                to: " ".into(),
            }))
            .is_err(),
        "a verb is held to it too"
    );
    assert_eq!(
        library.state().unwrap().get(&id).unwrap().tags.len(),
        1,
        "{before:?}"
    );
}

/// Two people editing one library at once converge: the body merges per
/// character, tags per tag, uses add up.
#[test]
fn concurrent_edits_converge() {
    let mut ours = Replica::<Snippets>::new();
    let id = create(&mut ours, "Query", None);
    let body = Snippets::paths().snippets.item(&id).body;
    edit(&mut ours, &body.splice(0, 0, "select 1")).unwrap();
    let mut theirs = ours.fork();

    edit(&mut theirs, &body.splice(8, 0, ";")).unwrap();
    edit(
        &mut theirs,
        &Snippets::paths()
            .snippets
            .item(&id)
            .tags
            .insert(&"sql".to_owned()),
    )
    .unwrap();
    theirs.apply(Snippets::RECORD_USE.with(id.clone())).unwrap();
    edit(&mut ours, &body.splice(0, 6, "SELECT")).unwrap();
    edit(
        &mut ours,
        &Snippets::paths()
            .snippets
            .item(&id)
            .tags
            .insert(&"db".to_owned()),
    )
    .unwrap();
    ours.apply(Snippets::RECORD_USE.with(id.clone())).unwrap();

    ours.sync_with(&mut theirs).unwrap();
    assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
    let merged = snippet(&ours, &id);
    assert_eq!(merged.body, "SELECT 1;");
    assert_eq!(
        merged.tags.iter().cloned().collect::<Vec<_>>(),
        ["db", "sql"]
    );
    assert_eq!(merged.uses.get(), 2);
}

/// Each snippet is published with the verbs that act on it; each folder
/// and tag is a place whose verbs are the library's, bound to it.
#[test]
fn snippets_folders_and_tags_offer_their_verbs() {
    let mut library = Replica::<Snippets>::new();
    let deploy = create(&mut library, "Deploy", Some("Ops"));
    let tag = Snippets::paths().snippets.item(&deploy).tags;
    edit(&mut library, &tag.insert(&"k8s".to_owned())).unwrap();
    let tree = library.published().unwrap();

    let item = child(
        &tree,
        &tilo_resource::items::item_resource_id::<Snippets>("snippets", &deploy),
    );
    assert_eq!(item.label, "Deploy");
    for verb in ["duplicate", "delete", "record-use"] {
        assert!(
            action(item, verb)
                .params()
                .iter()
                .all(|p| p.name != "snippet"),
            "{verb} is bound to the snippet, never asked for its id"
        );
    }
    let delete = action(item, "delete");
    assert!(delete.contract.can_be_undone());
    assert_eq!(
        delete.contract.confirmation,
        ui::tilo_actions::ConfirmationPolicy::None,
        "Undo takes a deletion back, so nobody is asked"
    );

    let ops = child(&tree, "snippets.library/derived/folder:Ops");
    assert_eq!(ops.label, "Ops");
    assert_eq!(ops.kind, "snippets.library.folder");
    assert!(
        action(ops, "reveal-folder").params().is_empty(),
        "a folder reveals itself"
    );
    assert!(
        tree.actions
            .iter()
            .all(|spec| spec.id != "reveal-folder" && spec.id != "reveal"),
        "revealing is offered on the place, never with a name to type"
    );
    let new_here = action(ops, "new-snippet");
    assert_eq!(
        new_here
            .params()
            .iter()
            .map(|p| p.name.to_string())
            .collect::<Vec<_>>(),
        ["title"],
        "the folder is bound"
    );
    invoke_spec(
        &mut library,
        new_here,
        serde_json::json!({"title": "Rollback"}),
    );
    let rollback = titled(&library, "Rollback");
    assert_eq!(snippet(&library, &rollback).folder.0, "Ops");
    invoke_spec(
        &mut library,
        action(ops, "rename-folder"),
        serde_json::json!({"to": "Operations"}),
    );
    let state = library.state().unwrap();
    assert!(state.snippets.values().all(|s| s.folder.0 == "Operations"));

    let k8s = child(&tree, "snippets.library/derived/tag:k8s");
    assert_eq!(k8s.kind, "snippets.library.tag");
    assert!(action(k8s, "reveal").params().is_empty());
    invoke_spec(
        &mut library,
        action(k8s, "rename-tag"),
        serde_json::json!({"to": "kubernetes"}),
    );
    assert!(
        snippet(&library, &deploy)
            .tags
            .contains(&"kubernetes".to_owned())
    );
    let tree = library.published().unwrap();
    let kubernetes = child(&tree, "snippets.library/derived/tag:kubernetes");
    invoke_spec(
        &mut library,
        action(kubernetes, "remove-tag"),
        serde_json::json!({}),
    );
    assert!(snippet(&library, &deploy).tags.is_empty());
    assert!(
        library
            .published()
            .unwrap()
            .children
            .iter()
            .all(|child| !child.id.as_str().contains("/derived/tag:")),
        "a tag no snippet carries is no place"
    );
}

#[test]
fn search_finds_each_snippet_by_what_it_holds() {
    let mut library = Replica::<Snippets>::new();
    let deploy = create(&mut library, "Deploy", Some("Ops"));
    let paths = Snippets::paths().snippets.item(&deploy);
    edit(&mut library, &paths.tags.insert(&"k8s".to_owned())).unwrap();
    edit(&mut library, &paths.body.replace("kubectl rollout")).unwrap();
    edit(
        &mut library,
        &paths.description.replace("Roll a deployment"),
    )
    .unwrap();
    edit(&mut library, &paths.language.set(Language::Shell)).unwrap();
    create(&mut library, "Backup", None);

    let entries = Snippets::search(&library.state().unwrap());
    assert_eq!(entries.len(), 2, "one entry per snippet");
    let hit = entries.iter().find(|e| e.title == "Deploy").unwrap();
    for held in [
        "Ops",
        "shell",
        "k8s",
        "kubectl rollout",
        "Roll a deployment",
    ] {
        assert!(
            hit.fields.iter().any(|(text, _)| text.contains(held)),
            "{held} in {:?}",
            hit.fields
        );
    }
    assert_eq!(
        hit.anchor.as_deref(),
        Some(tilo_resource::items::item_resource_id::<Snippets>("snippets", &deploy).as_str())
    );
}
