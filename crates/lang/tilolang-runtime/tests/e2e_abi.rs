//! The ABI additions end to end: scalar sets and maps, map reads, `sort_by` and `try`.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, FlowOutcome, ResourceRef, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = "app so.tilo.gaps \"Gaps\"\n\
type Item {\n    title: Text\n    rank: Int\n}\n\
type Bag {\n    nums: Set<Int>\n    scores: Map<Float>\n    counts: Map<Int>\n    items: Collection<Item>\n    n: Int merge counter\n}\n\
action add_num(b: Bag, x: Int) {\n    b.nums.add(x)\n}\n\
action drop_num(b: Bag, x: Int) {\n    b.nums.remove(x)\n}\n\
action put_score(b: Bag, k: Text, v: Float) {\n    b.scores.put(k, v)\n}\n\
action put_count(b: Bag, k: Text, v: Int) {\n    b.counts.put(k, v)\n}\n\
action drop_count(b: Bag, k: Text) {\n    b.counts.remove(k)\n}\n\
action add_item(b: Bag, title: Text, rank: Int) {\n    b.items.insert(Item(title: title, rank: rank))\n}\n\
action bump(b: Bag) {\n    b.n += 1\n}\n\
action size(b: Bag) -> Int {\n    return b.n\n}\n\
flow bump_flag(b: Bag) -> Int {\n    let ok = try bump(b)\n    if ok {\n        return 1\n    }\n    return 0\n}\n\
flow size_or(b: Bag) -> Int {\n    let got = try size(b)\n    return got.unwrap_or(-1)\n}\n\
view scores(b: Bag) {\n    column {\n        for k in b.scores.keys() {\n            text(k)\n        }\n        text(b.scores.get(\"x\").unwrap_or(0.5))\n        text(b.counts.get(\"y\").unwrap_or(-1))\n    }\n}\n\
view by_title(b: Bag) {\n    column {\n        for i in b.items.sort_by(t -> t.title) {\n            text(i.title)\n        }\n    }\n}\n\
view by_rank(b: Bag) {\n    column {\n        for i in b.items.sort_by(t -> t.rank) {\n            text(i.title)\n        }\n    }\n}\n\
view nums(b: Bag) {\n    column {\n        for x in b.nums {\n            text(x)\n        }\n    }\n}";

    struct Gaps {
        rt: Runtime,
        auth: SimpleAuthority,
        app: App,
        bag: ResourceRef,
    }

    fn gaps() -> Gaps {
        let app = App::compile(SOURCE, &[]);
        let auth = SimpleAuthority::new();
        let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
        app.load(&rt);
        let bag = rt.create("Bag").unwrap();
        Gaps { rt, auth, app, bag }
    }

    impl Gaps {
        fn call(&self, action: &str, mut args: Vec<Value>) {
            args.insert(0, Value::resource(&self.bag));
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap();
        }

        fn texts(&self, view: &str) -> Vec<Value> {
            let v = self
                .rt
                .open_view(
                    &me(),
                    &self.app.view(view),
                    vec![Value::resource(&self.bag)],
                )
                .unwrap();
            of_kind(v.tree(), "text")
                .iter()
                .map(|t| t.props["value"].clone())
                .collect()
        }

        fn flow(&self, name: &str) -> FlowOutcome {
            self.rt
                .run_flow(
                    &me(),
                    &self.app.flow(name),
                    vec![Value::resource(&self.bag)],
                )
                .unwrap()
        }
    }

    #[test]
    fn sets_of_ints_take_boxed_elements() {
        let g = gaps();
        for x in [3, 1, 2, 3] {
            g.call("add_num", vec![Value::Int(x)]);
        }
        assert_eq!(
            g.texts("nums"),
            [Value::Int(1), Value::Int(2), Value::Int(3)]
        );
        g.call("drop_num", vec![Value::Int(2)]);
        assert_eq!(g.texts("nums"), [Value::Int(1), Value::Int(3)]);
    }

    #[test]
    fn maps_of_floats_and_ints_are_written_boxed_and_read_with_get_and_keys() {
        let g = gaps();
        assert_eq!(
            g.texts("scores"),
            [Value::Float(0.5), Value::Int(-1)],
            "an absent key reads as none"
        );
        g.call("put_score", vec![Value::text("x"), Value::Float(2.5)]);
        g.call("put_score", vec![Value::text("a"), Value::Float(1.0)]);
        g.call("put_count", vec![Value::text("y"), Value::Int(7)]);
        assert_eq!(
            g.texts("scores"),
            [
                Value::text("a"),
                Value::text("x"),
                Value::Float(2.5),
                Value::Int(7)
            ]
        );
        g.call("drop_count", vec![Value::text("y")]);
        assert_eq!(g.texts("scores").last(), Some(&Value::Int(-1)));
    }

    #[test]
    fn sort_by_orders_by_text_and_by_int_keys() {
        let g = gaps();
        for (t, r) in [("pear", 2), ("apple", 3), ("fig", 1)] {
            g.call("add_item", vec![Value::text(t), Value::Int(r)]);
        }
        let titles = |v: Vec<Value>| -> Vec<String> {
            v.iter().map(|t| t.as_text().unwrap().to_owned()).collect()
        };
        assert_eq!(titles(g.texts("by_title")), ["apple", "fig", "pear"]);
        assert_eq!(titles(g.texts("by_rank")), ["fig", "pear", "apple"]);
    }

    #[test]
    fn try_tells_a_flow_whether_an_invocation_worked() {
        let g = gaps();
        assert_eq!(g.flow("bump_flag"), FlowOutcome::Done(Value::Int(1)));
        assert_eq!(g.flow("size_or"), FlowOutcome::Done(Value::Int(1)));

        g.auth.deny_action(&g.app.action("bump"));
        g.auth.deny_action(&g.app.action("size"));
        assert_eq!(g.flow("bump_flag"), FlowOutcome::Done(Value::Int(0)));
        assert_eq!(g.flow("size_or"), FlowOutcome::Done(Value::Int(-1)));
        let c = g.rt.read(&g.bag, "n").unwrap();
        assert_eq!(c, Value::Int(1), "the denied bump never ran");
    }
}
