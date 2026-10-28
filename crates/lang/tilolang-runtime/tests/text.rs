//! A bound text field: edits are attributed changes, spliced into the CRDT text.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{EditOutcome, InvokeOutcome, Patch, Value};

    fn world() -> (World, tilolang_runtime::ResourceRef) {
        let w = World::new();
        w.rt.load_package(&note_wasm(), note_meta()).unwrap();
        let n = w.rt.create("Note").unwrap();
        (w, n)
    }

    #[test]
    fn editing_a_bound_input_changes_the_field_and_patches_the_view() {
        let (w, n) = world();
        let mut v =
            w.rt.open_view(&w.me, "vn", vec![Value::resource(&n)])
                .unwrap();
        assert_eq!(v.tree().find("in").unwrap().props["text"], Value::text(""));
        assert!(v.tree().find("in").unwrap().binding.is_some());

        assert_eq!(
            v.edit("in", &Value::text("hello")).unwrap(),
            EditOutcome::Applied
        );
        assert_eq!(
            v.poll_patches().unwrap(),
            vec![Patch::SetProp {
                path: vec!["in".into()],
                name: "text".into(),
                value: Some(Value::text("hello")),
            }]
        );
        assert_eq!(
            v.edit("in", &Value::text("hello, world")).unwrap(),
            EditOutcome::Applied
        );
        assert_eq!(w.rt.read(&n, "body").unwrap(), Value::text("hello, world"));
        assert_eq!(
            v.edit("in", &Value::text("")).unwrap(),
            EditOutcome::Applied
        );
        assert_eq!(w.rt.read(&n, "body").unwrap(), Value::text(""));
    }

    #[test]
    fn an_edit_is_an_attributed_change() {
        let (w, n) = world();
        let v =
            w.rt.open_view(&w.me, "vn", vec![Value::resource(&n)])
                .unwrap();
        v.edit("in", &Value::text("x")).unwrap();
        let notes = w.rt.history(&n.doc).unwrap();
        let edit = notes.iter().find(|c| c.action == "$edit").unwrap();
        assert_eq!(edit.invoker, "me");
        assert_eq!(edit.args, serde_json::json!(["fb", "x"]));
        assert_eq!(edit.chain.len(), 1, "the view's module is in the chain");
    }

    #[test]
    fn text_set_from_an_action_diffs_instead_of_rewriting() {
        let (w, n) = world();
        for s in ["abc", "abXc", "abXc!"] {
            let out =
                w.rt.invoke_action(
                    &w.me,
                    ("set_body", vec![Value::resource(&n), Value::text(s)]),
                )
                .unwrap();
            assert_eq!(out, InvokeOutcome::Done(Value::Unit));
            assert_eq!(w.rt.read(&n, "body").unwrap(), Value::text(s));
        }
    }

    #[test]
    fn a_node_without_a_binding_cannot_be_edited() {
        let w = World::new();
        let c = w.counter();
        let v =
            w.rt.open_view(&w.me, "vc", vec![Value::resource(&c)])
                .unwrap();
        assert!(v.edit("txt", &Value::text("x")).is_err());
        assert!(v.edit("missing", &Value::text("x")).is_err());
    }
}
