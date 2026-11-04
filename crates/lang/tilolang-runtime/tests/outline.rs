//! A type whose items nest, outside the tasks app: the outline's actions come from the type's
//! `(parent)`, `(done)`, `(title)` and `(inherit)` fields, and do what a nested list does.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use std::collections::HashMap;

    use super::common::*;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, RuntimeError, SimpleAuthority, Value,
    };

    const SOURCE: &str = r#"
app t.outline "Outline"

type Note {
    title: Text (title)
    done: Bool (done) = false
    notebook: Option<Notebook> (inherit) = .none
    parent: Option<Note> (parent) = .none
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

/// Adds a note at the end of a notebook.
action add(n: Notebook, title: Text) -> Note {
    n.notes.insert(Note(title: title, notebook: .some(n)))
}
"#;

    struct Notes {
        rt: Runtime,
        app: App,
        book: ResourceRef,
    }

    impl Notes {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let book = rt.create_root("Notebook").unwrap();
            Self { rt, app, book }
        }

        fn try_call(&self, action: &str, args: Vec<Value>) -> Result<InvokeOutcome, RuntimeError> {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args))
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            self.try_call(action, args).unwrap()
        }

        fn add(&self, title: &str) -> ResourceRef {
            returned_ref(self.call("add", vec![Value::resource(&self.book), Value::text(title)]))
        }

        fn below(&self, parent: &ResourceRef, title: &str) -> ResourceRef {
            returned_ref(self.call(
                "add_subtask",
                vec![
                    Value::resource(&self.book),
                    Value::resource(parent),
                    Value::text(title),
                ],
            ))
        }

        fn read(&self, at: &ResourceRef, field: &str) -> Value {
            self.rt.read(at, field).unwrap()
        }

        fn done(&self, at: &ResourceRef) -> bool {
            self.read(at, "done") == Value::Bool(true)
        }

        /// The note `at` stands under, by its item.
        fn under(&self, at: &ResourceRef) -> Option<String> {
            match self.read(at, "parent") {
                Value::Opt(Some(inner)) => match *inner {
                    Value::Ref(r) => Some(r.target.item.clone().unwrap_or_default()),
                    other => panic!("a parent that is no note: {other:?}"),
                },
                Value::Opt(None) => None,
                other => panic!("a parent that is no parent: {other:?}"),
            }
        }

        /// The notebook's notes as the outline shows them: each note, then the notes under it,
        /// depth first, with the depth it stands at.
        fn outline(&self) -> Vec<(String, usize)> {
            let Value::List(items) = self.read(&self.book, "notes") else {
                panic!("notes");
            };
            let mut children: HashMap<Option<String>, Vec<(String, String)>> = HashMap::new();
            for value in items {
                let Value::Ref(r) = value else {
                    panic!("a note that is no reference")
                };
                let note = r.target.as_ref().clone();
                let item = note.item.clone().unwrap_or_default();
                let Value::Text(title) = self.read(&note, "title") else {
                    panic!("a title");
                };
                children
                    .entry(self.under(&note))
                    .or_default()
                    .push((item, title));
            }
            let mut out = Vec::new();
            walk(None, 0, &children, &mut out);
            out
        }
    }

    /// Each note, then the notes under it, depth first, with the depth it stands at.
    fn walk(
        at: Option<&str>,
        depth: usize,
        children: &HashMap<Option<String>, Vec<(String, String)>>,
        out: &mut Vec<(String, usize)>,
    ) {
        let Some(kids) = children.get(&at.map(str::to_owned)) else {
            return;
        };
        for (item, title) in kids {
            out.push((title.clone(), depth));
            walk(Some(item), depth.saturating_add(1), children, out);
        }
    }

    fn lines(outline: &[(String, usize)]) -> Vec<(String, usize)> {
        outline.to_vec()
    }

    #[test]
    fn a_subtask_stands_last_under_its_parent_and_keeps_what_it_inherits() {
        let n = Notes::new();
        let first = n.add("first");
        n.add("last");
        let one = n.below(&first, "one");
        assert_eq!(
            lines(&n.outline()),
            [
                ("first".to_owned(), 0),
                ("one".to_owned(), 1),
                ("last".to_owned(), 0),
            ],
            "a new subtask stands with its siblings, under the note it was made under"
        );
        assert_eq!(n.under(&one).as_deref(), first.item.as_deref());
        // It keeps what the one it stands under is in, which `(inherit)` marks.
        let Value::Opt(Some(inner)) = n.read(&one, "notebook") else {
            panic!("a subtask is in the notebook it was made in");
        };
        let Value::Ref(r) = *inner else {
            panic!("a notebook that is no reference")
        };
        assert_eq!(r.target.item, n.book.item);
        // A second one stands after the first, not between it and its parent.
        n.below(&first, "two");
        assert_eq!(
            lines(&n.outline()),
            [
                ("first".to_owned(), 0),
                ("one".to_owned(), 1),
                ("two".to_owned(), 1),
                ("last".to_owned(), 0),
            ]
        );
        // A done note opens again: there is work under it now.
        n.call("check", vec![Value::resource(&first)]);
        assert!(n.done(&first));
        n.below(&first, "three");
        assert!(!n.done(&first), "the note it was added under is open again");
    }

    #[test]
    fn checking_an_item_off_finishes_what_is_under_it_and_opening_it_reopens_what_it_is_under() {
        let n = Notes::new();
        let top = n.add("top");
        let middle = n.below(&top, "middle");
        let bottom = n.below(&middle, "bottom");
        n.call("check", vec![Value::resource(&top)]);
        assert!(n.done(&top) && n.done(&middle) && n.done(&bottom), "done");
        n.call("check", vec![Value::resource(&bottom)]);
        assert!(
            !n.done(&top) && !n.done(&middle) && !n.done(&bottom),
            "the two it is under are open again"
        );
        assert_eq!(n.outline().len(), 3);
    }

    #[test]
    fn indenting_puts_an_item_under_the_one_over_it_and_outdenting_brings_it_back() {
        let n = Notes::new();
        let top = n.add("top");
        let a = n.add("a");
        // `Ctrl+]`: under the note the list shows over it.
        n.call(
            "indent",
            vec![Value::resource(&a), Value::some(Value::resource(&top))],
        );
        assert_eq!(n.under(&a).as_deref(), top.item.as_deref());
        // `Ctrl+[`: back beside the note it was under, just after it.
        n.call("outdent", vec![Value::resource(&a)]);
        assert_eq!(n.under(&a), None);
        assert_eq!(
            lines(&n.outline()),
            [("top".to_owned(), 0), ("a".to_owned(), 0)],
            "it stands beside its parent"
        );
    }

    #[test]
    fn moving_an_item_puts_it_under_another_or_before_it_and_refuses_to_put_it_inside_itself() {
        let n = Notes::new();
        let a = n.add("a");
        let b = n.add("b");
        let a1 = n.below(&a, "a1");
        // The end of `b`'s items.
        n.call(
            "move_to",
            vec![
                Value::resource(&n.book),
                Value::resource(&a),
                Value::some(Value::resource(&b)),
                Value::none(),
            ],
        );
        assert_eq!(n.under(&a).as_deref(), b.item.as_deref());
        assert_eq!(
            lines(&n.outline()),
            [
                ("b".to_owned(), 0),
                ("a".to_owned(), 1),
                ("a1".to_owned(), 2),
            ]
        );
        // Where `b` stands: back to the top, before it.
        n.call(
            "move_to",
            vec![
                Value::resource(&n.book),
                Value::resource(&a),
                Value::none(),
                Value::some(Value::resource(&b)),
            ],
        );
        assert_eq!(n.under(&a), None);
        assert_eq!(
            lines(&n.outline()),
            [
                ("a".to_owned(), 0),
                ("a1".to_owned(), 1),
                ("b".to_owned(), 0),
            ]
        );
        // A note cannot go inside itself, nor inside anything it holds.
        for inside in [&a1, &a] {
            let refused = n.try_call(
                "move_to",
                vec![
                    Value::resource(&n.book),
                    Value::resource(&a),
                    Value::some(Value::resource(inside)),
                    Value::none(),
                ],
            );
            let Err(RuntimeError::Trap(why)) = refused else {
                panic!("a note went inside itself: {refused:?}");
            };
            assert!(why.contains("cannot go inside itself"), "{why}");
        }
        assert_eq!(
            lines(&n.outline()),
            [
                ("a".to_owned(), 0),
                ("a1".to_owned(), 1),
                ("b".to_owned(), 0),
            ],
            "a refused move leaves the notes where they were"
        );
    }

    #[test]
    fn a_copy_takes_the_items_under_it_along_and_a_delete_takes_them_with_it() {
        let n = Notes::new();
        let a = n.add("a");
        n.below(&a, "a1");
        n.below(&a, "a2");
        let b = n.add("b");
        n.call(
            "duplicate",
            vec![Value::resource(&n.book), Value::resource(&a)],
        );
        assert_eq!(
            lines(&n.outline()),
            [
                ("a".to_owned(), 0),
                ("a1".to_owned(), 1),
                ("a2".to_owned(), 1),
                ("a".to_owned(), 0),
                ("a1".to_owned(), 1),
                ("a2".to_owned(), 1),
                ("b".to_owned(), 0),
            ],
            "the copy and what is under it stand just after what they copy"
        );
        n.call(
            "remove",
            vec![Value::resource(&n.book), Value::resource(&b)],
        );
        assert_eq!(n.outline().len(), 6);
        n.call(
            "remove",
            vec![Value::resource(&n.book), Value::resource(&a)],
        );
        assert_eq!(
            lines(&n.outline()),
            [
                ("a".to_owned(), 0),
                ("a1".to_owned(), 1),
                ("a2".to_owned(), 1),
            ],
            "the delete takes the notes under it with it"
        );
    }
}
