//! A package that writes through a library's action: the record it makes is the library's, an
//! ordinary workspace record under the library's app id, and the package reads it back through
//! the library's own function.

use automerge::AutoCommit;
use tilolang_runtime::{UiNode, Value, ViewInstance};
use tilolang_tilo::record::DynValue;

use crate::fixtures::{Replica, button_key, field_id};

/// The library: a record type of its own, an enum, a function over its resource type, and the
/// one action that writes its records.
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
/// applied from a flow, its function called from the view.
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
flow add_high() {
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
    fn walk<'a>(node: &'a UiNode, out: &mut Vec<Option<&'a Value>>) {
        if node.kind == "ui.Text" {
            out.push(node.props.get("value"));
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(view.tree(), &mut out);
    out
}

#[test]
fn a_library_owns_the_records_its_action_writes() {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(MARKS, ""), (NOTES, "")]);
    let (library, _user) = (&packages[0], &packages[1]);
    ada.host.root_of(LIBRARY).unwrap();
    ada.host.root_of(USER).unwrap();
    ada.commit();

    let view = ada.view(USER);
    assert_eq!(texts(&view), [Some(&Value::text("0"))], "no mark yet");
    view.press(&button_key(&view)).unwrap();
    ada.commit();

    // The press ran the user's flow, which applied the library's action: the mark is in the
    // library's root record, under the library's app id.
    let record = ada.root(LIBRARY);
    let Some(DynValue::Collection(marks)) = record.get(&field_id(&library.meta, "Marks", "marks"))
    else {
        panic!("the board holds its marks: {record:?}");
    };
    assert_eq!(marks.len(), 1, "the mark the user's press added");
    let mark = marks.at(0).expect("the one mark").1;
    assert_eq!(
        mark.get(&field_id(&library.meta, "Mark", "title")),
        Some(&DynValue::Text("High".to_owned()))
    );
    assert_eq!(
        texts(&ada.view(USER)),
        [Some(&Value::text("1"))],
        "the user's view reads the count through the library's function"
    );
}
