//! The tree actions an item type gets from its fields: a type whose items nest (`(parent)`), are
//! named (`(title)`) and can be finished (`(done)`), held by a named root, has its outline's
//! move, indent, outdent, copy, delete, check and add-subtask without its app writing them.

use crate::common::{codes, compile_body};
use tilolang::meta::{ActionMeta, PackageMeta};

/// A type that is not the tasks app's, so what it gets comes from its fields alone.
const NOTES: &str = "\
type Note {
    title: Text (title)
    done: Bool (done) = false
    notebook: Option<Notebook> (inherit) = .none
    parent: Option<Note> (parent) = .none
}

type Notebook {
    name: Text
    notes: Collection<Note>
}

root notes: Notebook

default view notebook_page for Notebook

view notebook_page(n: Notebook) {
    stack {
        for x in n.notes {
            text(x.title)
        }
    }
}
";

fn action<'a>(meta: &'a PackageMeta, name: &str) -> &'a ActionMeta {
    meta.actions
        .iter()
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("no action {name}"))
}

#[test]
fn a_nested_item_type_gets_its_tree_actions_from_its_fields() {
    let meta = compile_body(NOTES).meta;
    let names: Vec<&str> = meta.actions.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        [
            // What an item's state speaks of comes before what it can be given.
            "check",
            "add_subtask",
            // Where it stands, what becomes of it, and the helpers they read.
            "move_to",
            "indent",
            "outdent",
            "duplicate",
            "remove",
            "copy_below",
        ],
        "the actions the type gets, and where they stand among its declarations"
    );

    // What the type's state speaks of: checking an item off, and its shortcut.
    let check = action(&meta, "check");
    assert_eq!(check.key.as_deref(), Some("Ctrl+enter"));
    let done = meta.types[0].fields.iter().find(|f| f.name == "done");
    assert_eq!(
        check.toggles.as_deref(),
        done.map(|f| f.id.as_str()),
        "the checkbox bound to the flag stands for the action"
    );
    assert!(
        check.present.is_some(),
        "its label follows whether it is done"
    );
    assert_eq!(
        check
            .params
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["t", "done"]
    );

    // What it is made of: an item under the one it stands under, keeping what it inherits.
    let add = action(&meta, "add_subtask");
    assert_eq!(add.label, "Add subtask");
    assert_eq!(add.icon.as_deref(), Some("list-tree"));
    assert_eq!(add.key.as_deref(), Some("Ctrl+Shift+n"));
    assert_eq!(
        add.did.as_deref(),
        Some("added a subtask"),
        "the history sentence comes from the action, so the app writes none"
    );
    assert_eq!(add.result.as_deref(), Some("Note"));

    // Where an item stands: the move the outline's drag names, and the two keys that indent.
    let move_to = action(&meta, "move_to");
    assert_eq!(move_to.role.as_deref(), Some("internal"));
    assert_eq!(
        move_to
            .params
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["notes", "moved", "parent", "before"]
    );
    let indent = action(&meta, "indent");
    assert_eq!(indent.label, "Indent");
    assert_eq!(indent.key.as_deref(), Some("Ctrl+]"));
    assert!(
        indent.params[1].place,
        "the row over it supplies the item it moves under"
    );
    let outdent = action(&meta, "outdent");
    assert_eq!(outdent.label, "Outdent");
    assert_eq!(outdent.key.as_deref(), Some("Ctrl+["));

    // What becomes of an item: a copy with the items under it, and a delete with them.
    let duplicate = action(&meta, "duplicate");
    assert_eq!(duplicate.label, "Duplicate");
    assert_eq!(duplicate.key.as_deref(), Some("Ctrl+Shift+d"));
    let remove = action(&meta, "remove");
    assert_eq!(remove.label, "Delete");
    assert_eq!(remove.key.as_deref(), Some("delete"));
    assert_eq!(remove.role.as_deref(), Some("destructive"));
}

#[test]
fn a_field_marked_inherit_says_what_an_item_keeps_of_the_one_it_stands_under() {
    let meta = compile_body(NOTES).meta;
    let note = &meta.types[0];
    let role = |name: &str| {
        note.fields
            .iter()
            .find(|field| field.name == name)
            .and_then(|field| field.role.as_ref().map(tilolang::types::FieldRole::text))
    };
    assert_eq!(role("notebook").as_deref(), Some("inherit"));
    assert_eq!(role("title").as_deref(), Some("title"));
    assert_eq!(role("done").as_deref(), Some("done"));
    assert_eq!(role("parent").as_deref(), Some("parent"));
}

#[test]
fn an_app_declares_an_action_of_the_same_name_to_override_it() {
    let body = format!(
        "{NOTES}
/// Copies a note, in the app's own words.
action duplicate(notes: Notebook, n: Note) -> Note (
    label: \"Copy note\",
) {{
    notes.notes.insert(Note(title: n.title, parent: n.parent))
}}
"
    );
    let meta = compile_body(&body).meta;
    let copies: Vec<&ActionMeta> = meta
        .actions
        .iter()
        .filter(|a| a.name == "duplicate")
        .collect();
    assert_eq!(copies.len(), 1, "one action of a name: the app's");
    assert_eq!(copies[0].label, "Copy note");
    // The rest is still the type's.
    assert!(meta.actions.iter().any(|a| a.name == "copy_below"));
    assert!(meta.actions.iter().any(|a| a.name == "remove"));
}

#[test]
fn a_type_whose_items_do_not_nest_gets_no_tree_actions() {
    let plain = "type Note {
    title: Text (title)
    done: Bool (done) = false
}

type Notebook {
    notes: Collection<Note>
}

root notes: Notebook

default view notebook_page for Notebook

view notebook_page(n: Notebook) {
    stack {
        for x in n.notes {
            text(x.title)
        }
    }
}
";
    assert_eq!(codes(plain), Vec::<String>::new());
    let meta = compile_body(plain).meta;
    assert!(
        meta.actions.is_empty(),
        "no action the type did not declare: {:?}",
        meta.actions
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_nested_type_no_root_holds_gets_no_tree_actions() {
    let loose = "type Note {
    title: Text (title)
    done: Bool (done) = false
    parent: Option<Note> (parent) = .none
}

type Notebook {
    name: Text
}
";
    assert_eq!(codes(loose), Vec::<String>::new());
    let meta = compile_body(loose).meta;
    assert!(
        meta.actions.is_empty(),
        "the actions read the items through a root holding them: {:?}",
        meta.actions
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
    );
}
