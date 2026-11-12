//! Another package's `fn`, called through its alias: it runs as that package's own code, its
//! reads are the caller's, and a run that replays after a question takes the path it took.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, Resolution, ResourceRef, RunOutcome, Runtime, SimpleAuthority,
        Value,
    };

    /// A list of counts, and the rule over it: the only place the rule is written.
    const BAG: &str = "app so.tilo.lib \"Lib\"

type Bag {
    items: Collection<Item>
}

type Item {
    n: Int
}

root Bag

default view card for Bag

/// Whether any item is over ten.
fn big(b: Bag) -> Bool {
    b.items.any(i -> i.n > 10)
}

action add(b: Bag, n: Int) {
    b.items.insert(Item(n: n))
}

action drop_big(b: Bag) {
    for i in b.items.filter(x -> x.n > 10) {
        b.items.remove(i)
    }
}

/// Sets what every item holds, and nothing else of the bag.
action set_all(b: Bag, to: Int = 13) {
    for i in b.items {
        i.n = to
    }
}

view card(b: Bag) {
    text(\"{b.items.len()}\")
}
";

    /// An action and a view that ask the other package's rule instead of copying it.
    const CALLER: &str = "app so.tilo.caller \"Caller\"

use so.tilo.lib as lib

action sweep(b: lib.Bag) -> Int {
    let n = 0
    if lib.big(b) {
        lib.drop_big(b)
        n = 1
    }
    if lib.big(b) {
        n = n + 10
    }
    n
}

view bag(b: lib.Bag) {
    if lib.big(b) {
        text(\"big\")
    } else {
        text(\"small\")
    }
}

/// How many the bag holds, and how many of it are over ten: each reads other parts of the
/// other package's record.
view counts(b: lib.Bag) {
    text(\"{b.items.len()}\")
}

view bigs(b: lib.Bag) {
    text(\"{b.items.filter(x -> x.n > 10).len()}\")
}
";

    struct World {
        rt: Runtime,
        lib: App,
        caller: App,
        bag: ResourceRef,
    }

    fn world() -> World {
        let lib = App::compile(BAG, &[]);
        let caller = App::compile(CALLER, &[lib.meta().clone()]);
        let auth = SimpleAuthority::new();
        let rt = Runtime::new(AmStore::new(), auth).unwrap();
        lib.load(&rt);
        caller.load(&rt);
        let bag = rt.create_root("Bag").unwrap();
        World {
            rt,
            lib,
            caller,
            bag,
        }
    }

    impl World {
        fn add(&self, n: i64) {
            let add = self.lib.action("add");
            self.rt
                .invoke_action(&me(), (add.as_str(), vec![res(&self.bag), Value::Int(n)]))
                .unwrap();
        }

        /// Sets what every item holds, through the package that writes the record.
        fn set_all(&self, to: i64) {
            let set = self.lib.action("set_all");
            self.rt
                .invoke_action(&me(), (set.as_str(), vec![res(&self.bag), Value::Int(to)]))
                .unwrap();
        }

        fn start(&self) -> InvokeOutcome {
            self.rt
                .invoke_action(&me(), (self.caller.action("sweep"), vec![res(&self.bag)]))
                .unwrap()
        }

        fn items(&self) -> usize {
            let Value::List(items) = self.rt.read(&self.bag, "items").unwrap() else {
                panic!("items");
            };
            items.len()
        }
    }

    #[test]
    fn a_run_replays_the_rule_it_read_from_the_package_that_wrote_it() {
        let w = world();
        w.add(20);
        w.add(1);
        let InvokeOutcome::Suggested(asked) = w.start() else {
            panic!("the other package's action asks");
        };
        assert_eq!(asked.action, w.lib.action("drop_big"));
        assert_eq!(w.items(), 2, "asking changes nothing");
        let out =
            w.rt.resolve(&asked.run, &me(), Resolution::Approve)
                .unwrap();
        assert_eq!(
            out,
            RunOutcome::Done(Value::Int(1)),
            "the replay takes the branch it took before the question, and the rule reads what \
             the clear left"
        );
        assert_eq!(w.items(), 1, "the item over ten went; the other stayed");
        let drops =
            w.rt.history(&w.bag.doc)
                .unwrap()
                .iter()
                .filter(|n| n.action == w.lib.action("drop_big"))
                .count();
        assert_eq!(drops, 1, "the clear ran once for the whole run");
    }

    #[test]
    fn a_view_runs_again_when_what_the_rule_read_changes() {
        let w = world();
        let mut view =
            w.rt.open_view(&me(), &w.caller.view("bag"), vec![res(&w.bag)])
                .unwrap();
        assert_eq!(textual(view.tree()), "small");
        w.add(20);
        assert!(
            !view.poll_patches().unwrap().is_empty(),
            "the view read the bag through another package's fn: it runs again when it changes"
        );
        assert_eq!(textual(view.tree()), "big");
    }

    #[test]
    fn a_view_over_another_packages_record_runs_again_for_the_field_it_read() {
        let w = world();
        let mut counts =
            w.rt.open_view(&me(), &w.caller.view("counts"), vec![res(&w.bag)])
                .unwrap();
        let mut bigs =
            w.rt.open_view(&me(), &w.caller.view("bigs"), vec![res(&w.bag)])
                .unwrap();
        assert_eq!(textual(counts.tree()), "0");
        assert_eq!(textual(bigs.tree()), "0");
        // A row appears: both read the bag's items, so both run.
        w.add(20);
        assert!(
            !counts.poll_patches().unwrap().is_empty(),
            "the row it counts appeared"
        );
        assert!(
            !bigs.poll_patches().unwrap().is_empty(),
            "the items it looks at changed"
        );
        assert_eq!(textual(counts.tree()), "1");
        assert_eq!(textual(bigs.tree()), "1");
        // The package that owns the record changes what its items hold: the view that counts
        // them reads none of that, and only the view that reads it runs.
        w.set_all(3);
        assert!(
            counts.poll_patches().unwrap().is_empty(),
            "no item was added or removed: the count did not move"
        );
        assert!(
            !bigs.poll_patches().unwrap().is_empty(),
            "the field it reads moved"
        );
        assert_eq!(textual(bigs.tree()), "0");
    }

    /// A resource, as a call takes it.
    fn res(r: &ResourceRef) -> Value {
        Value::resource(r)
    }

    /// The text of a view's one text node.
    fn textual(node: &tilolang_runtime::UiNode) -> String {
        let texts = of_kind(node, "ui.Text");
        assert_eq!(texts.len(), 1, "{}", dump(node));
        text_prop(texts[0], "value")
    }
}
