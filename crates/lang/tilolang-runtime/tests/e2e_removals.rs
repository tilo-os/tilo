//! Items a list operation picked out of a collection (a filter, `remove_where`, a sort, a `find`)
//! are the collection's own items: an action removes exactly those, in one change, from the
//! collection they stand in, also where their holder has two collections of their type.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, Value};

    const SOURCE: &str = include_str!("../../tilolang/tests/fixtures/sweep.tl");

    struct Sweep {
        rt: Runtime,
        app: App,
        board: ResourceRef,
    }

    impl Sweep {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            app.load(&rt);
            let board = rt.create("Board").unwrap();
            Self { rt, app, board }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> Value {
            let id = self.app.action(action);
            match self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap() {
                tilolang_runtime::InvokeOutcome::Done(value) => value,
                other => panic!("{action} answered {other:?}"),
            }
        }

        /// Items titled `titles` in the board's collection `field`, those in `done` finished.
        fn fill(&self, field: &str, titles: &[&str], done: &[&str]) {
            let add = if field == "items" { "add" } else { "keep" };
            for title in titles {
                let made = self.call(add, vec![Value::resource(&self.board), Value::text(*title)]);
                if done.contains(title) {
                    self.call("finish", vec![made]);
                }
            }
        }

        fn titles(&self, field: &str) -> Vec<String> {
            self.names(&self.board, field, "title")
        }

        /// The text field `name` of each item of `holder`'s collection `field`, in order.
        fn names(&self, holder: &ResourceRef, field: &str, name: &str) -> Vec<String> {
            let Value::List(items) = self.rt.read(holder, field).unwrap() else {
                panic!("{field} reads as a list");
            };
            items
                .iter()
                .map(|item| {
                    let Value::Ref(item) = item else {
                        panic!("an item reads as its reference, not {item:?}");
                    };
                    match self.rt.read(&item.target, name).unwrap() {
                        Value::Text(text) => String::from(&*text),
                        other => panic!("{name} reads {other:?}"),
                    }
                })
                .collect()
        }

        /// How many changes the board's document holds.
        fn changes(&self) -> usize {
            self.rt.history(&self.board.doc).unwrap().len()
        }
    }

    fn made(value: &Value) -> ResourceRef {
        match value {
            Value::Ref(made) => (*made.target).clone(),
            other => panic!("an action answered {other:?}, not what it made"),
        }
    }

    #[test]
    fn items_a_filter_remove_where_or_sort_picked_out_are_removed_in_one_change() {
        for (action, answer) in [
            ("clear_filtered", Value::Unit),
            ("clear_where", Value::Int(2)),
            ("clear_sorted", Value::Unit),
        ] {
            let s = Sweep::new();
            s.fill("items", &["milk", "eggs", "bread", "tea"], &["eggs", "tea"]);
            let before = s.changes();
            assert_eq!(
                s.call(action, vec![Value::resource(&s.board)]),
                answer,
                "{action}"
            );
            assert_eq!(s.titles("items"), ["milk", "bread"], "{action}");
            assert_eq!(s.changes(), before + 1, "{action} is one change");
        }
        let s = Sweep::new();
        s.fill("items", &["milk", "eggs", "tea"], &["eggs", "tea"]);
        s.call("clear_first", vec![Value::resource(&s.board)]);
        assert_eq!(s.titles("items"), ["milk", "tea"]);
    }

    #[test]
    fn items_filtered_from_one_of_two_collections_of_a_type_leave_that_one() {
        let s = Sweep::new();
        s.fill("items", &["milk", "eggs"], &["eggs"]);
        s.fill("kept", &["old", "older", "keep"], &["old", "older"]);
        s.call("clear_kept", vec![Value::resource(&s.board)]);
        assert_eq!(s.titles("kept"), ["keep"]);
        assert_eq!(s.titles("items"), ["milk", "eggs"]);
        s.call("clear_filtered", vec![Value::resource(&s.board)]);
        assert_eq!(s.titles("items"), ["milk"]);
        assert_eq!(s.titles("kept"), ["keep"]);
    }

    #[test]
    fn an_items_filtered_spares_leave_its_spares_and_its_parts_stay() {
        let s = Sweep::new();
        let bike = made(&s.call("add", vec![Value::resource(&s.board), Value::text("bike")]));
        for (action, name, worn) in [
            ("fit", "chain", true),
            ("fit", "bell", false),
            ("stock", "tube", true),
            ("stock", "pad", false),
            ("stock", "cable", true),
        ] {
            s.call(
                action,
                vec![Value::resource(&bike), Value::text(name), Value::Bool(worn)],
            );
        }
        s.call("clear_worn_spares", vec![Value::resource(&bike)]);
        assert_eq!(s.names(&bike, "spares", "name"), ["pad"]);
        assert_eq!(s.names(&bike, "parts", "name"), ["chain", "bell"]);
    }
}
