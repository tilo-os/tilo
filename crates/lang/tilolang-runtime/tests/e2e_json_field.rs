//! A resource's field holds a JSON document: written whole, read back as the `json.Value` it
//! was, and carried to every replica of the workspace.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, InvokeOutcome, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = r#"app test.jsonfield "Json field"

type Holder {
    note: Text
    data: json.Value
}

root Holder

default view holder for Holder

view holder(h: Holder) {
    text("{h.note}")
}

action put(h: Holder, value: json.Value) {
    h.data = value
}

action kind(h: Holder) -> Text {
    match h.data {
        .object(fields) => "object {fields.keys().len()}",
        .array(items) => "list {items.len()}",
        .string(text) => "text {text}",
        .number(number) => "number",
        .bool(flag) => "bool",
        .null => "null",
    }
}
"#;

    tilo_test_env::scenario! {
        fn a_field_holds_a_json_document_and_reads_it_back;

        phase a_resource_starts_at_json_null_and_keeps_what_it_is_given;
        {
            let app = App::compile(SOURCE, &[]);
            let store = AmStore::new();
            let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let me = me();
            let holder = rt.create("Holder").unwrap();
            assert_eq!(
                rt.read(&holder, "data").unwrap(),
                Value::Json(Box::new(serde_json::Value::Null)),
                "a document nothing wrote yet is null"
            );
            let put = app.action("put");
            let document = serde_json::json!({"title": "Take the ferry", "stops": [1, 2]});
            let out = rt
                .invoke_action(
                    &me,
                    (
                        put.as_str(),
                        vec![
                            Value::resource(&holder),
                            Value::Json(Box::new(document.clone())),
                        ],
                    ),
                )
                .unwrap();
            assert_eq!(out, InvokeOutcome::Done(Value::Unit));
            assert_eq!(
                rt.read(&holder, "data").unwrap(),
                Value::Json(Box::new(document.clone())),
                "the document is the value it was given"
            );
            let kind = app.action("kind");
            assert_eq!(
                rt.invoke_action(&me, (kind.as_str(), vec![Value::resource(&holder)]))
                    .unwrap(),
                InvokeOutcome::Done(Value::text("object 2"))
            );
        }

        phase the_document_reaches_a_second_replica_as_it_is;
        {
            let app = App::compile(SOURCE, &[]);
            let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
            let rt_a = Runtime::new(store_a.clone(), SimpleAuthority::new()).unwrap();
            let rt_b = Runtime::new(store_b.clone(), SimpleAuthority::new()).unwrap();
            app.load(&rt_a);
            app.load(&rt_b);
            let me = me();
            let holder = rt_a.create("Holder").unwrap();
            store_a.sync_with(&mut store_b).unwrap();
            let document = serde_json::json!({"nested": {"deep": [true, null]}});
            let put = app.action("put");
            rt_a
                .invoke_action(
                    &me,
                    (
                        put.as_str(),
                        vec![
                            Value::resource(&holder),
                            Value::Json(Box::new(document.clone())),
                        ],
                    ),
                )
                .unwrap();
            store_a.sync_with(&mut store_b).unwrap();
            assert_eq!(
                rt_b.read(&holder, "data").unwrap(),
                Value::Json(Box::new(document)),
                "the workspace carries the document whole"
            );
        }
    }
}
