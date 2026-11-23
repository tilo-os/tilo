//! A package that writes through a library's action: the document it makes is the library's, an
//! ordinary workspace document under the library's app id, and the package reads it back through
//! the library's own function.

use crate::fixtures::Workspace;
use tilolang_runtime::{Value, ViewInstance};

use crate::fixtures::{Replica, button_key, field_id};

/// The library: a record type of its own, an enum, a function over its resource type, and the
/// one action that writes its documents.
const MARKS: &str = r#"app test.marks "Marks"

/// How much a mark matters.
enum Importance {
    /// Low
    low
    /// High
    high
}

/// One mark on the board.
type Mark {
    title: Text
    importance: Importance
}

/// The board's marks.
type Marks {
    marks: Collection<Mark>
}

root marks: Marks

default view board for marks

/// How many marks the board holds.
fn count(m: Marks) -> Int {
    m.marks.len()
}

/// Adds a mark to the board.
action add(m: Marks, title: Text, importance: Importance) {
    m.marks.insert(Mark(title: title, importance: importance))
}

/// The board: its marks.
view board(m: Marks) {
    page {
        for mark in m.marks {
            text(mark.title)
        }
    }
}
"#;

/// The package that uses it: no declarations of its own that the library owns, its action
/// applied from an action, its function called from the view.
const NOTES: &str = r#"app test.notes "Notes"

use test.marks as marks

/// A note of the notebook.
type Note {
    title: Text
}

/// The notebook's notes.
type Notebook {
    notes: Collection<Note>
}

root notebook: Notebook

default view main for Notebook

/// Adds a high mark to the board through the library's action.
action add_high() {
    marks.add(marks.marks, "High", .high)
}

/// The notebook: its notes, and how many marks the board holds.
view main(n: Notebook) {
    page {
        for note in n.notes {
            text(note.title)
        }
        text("{marks.count(marks.marks)}")
        button("Add high", on: add_high())
    }
}
"#;

const LIBRARY: &str = "test.marks";
const USER: &str = "test.notes";

/// What the view shows as text, in order.
fn texts(view: &ViewInstance) -> Vec<Option<&Value>> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .filter(|node| node.kind == "ui.Text")
        .map(|node| node.props.get("value"))
        .collect()
}

#[test]
fn a_library_owns_the_documents_its_action_writes() {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[(MARKS, ""), (NOTES, "")]);
    let (library, _user) = (&packages[0], &packages[1]);
    ada.host.root_of(LIBRARY).unwrap();
    ada.host.root_of(USER).unwrap();
    ada.commit();

    let view = ada.view(USER);
    assert_eq!(texts(&view), [Some(&Value::text("0"))], "no mark yet");
    view.press(&button_key(&view)).unwrap();
    ada.commit();

    // The press ran the user's action, which applied the library's action: the mark is in the
    // library's root, under the library's app id.
    let marks = ada.read(&ada.root(LIBRARY), "marks");
    let items = match marks {
        Value::List(items) => items,
        other => panic!("the board holds its marks: {other:?}"),
    };
    assert_eq!(items.len(), 1, "the mark the user's press added");
    let mark = match &items[0] {
        Value::Ref(mark) => mark.target.clone(),
        other => panic!("a mark: {other:?}"),
    };
    assert_eq!(
        ada.read(&mark, &field_id(&library.meta, "Mark", "title")),
        Value::text("High")
    );
    assert_eq!(
        texts(&ada.view(USER)),
        [Some(&Value::text("1"))],
        "the user's view reads the count through the library's function"
    );
}
