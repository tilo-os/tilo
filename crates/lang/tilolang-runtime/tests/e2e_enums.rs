//! Sum types and view state end to end: enum values as host handles in fields, through sync
//! and in views, and `window`/`lens` state bound to controls.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{
        AmStore, EditOutcome, EnumValue, Principal, ResourceRef, Runtime, SimpleAuthority, Value,
    };

    const SOURCE: &str = r#"app so.tilo.shapes "Shapes"

enum Shape {
    dot
    line(len: Int)
    box(w: Float, label: Text)
}

type Canvas {
    shape: Shape
    last: Option<Shape>
}

root Canvas

default view drawing for Canvas

action draw(c: Canvas, len: Int) {
    c.last = .some(c.shape)
    c.shape = if len > 5 { .box(w: 1.5, label: "big") } else { .line(len: len) }
}

fn describe(s: Shape) -> Text {
    match s {
        .dot => "a dot",
        .line(n) => "a line of {n}",
        .box(w, label) => "{label} {w}",
    }
}

view drawing(c: Canvas) {
    lens query: Text
    window zoom: Int = 1
    page {
        text(describe(c.shape))
        if c.last == .some(Shape.dot) {
            text("was a dot")
        }
        input(bind: query, placeholder: "Find")
        input(bind: zoom, placeholder: "Zoom")
        text("{query} at {zoom}")
    }
}
"#;

    fn setup(store: &AmStore) -> (Runtime, App, ResourceRef) {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let canvas = rt.create("Canvas").unwrap();
        (rt, app, canvas)
    }

    fn draw(rt: &Runtime, app: &App, canvas: &ResourceRef, len: i64) {
        let id = app.action("draw");
        rt.invoke_action(
            &me(),
            (id.as_str(), vec![Value::resource(canvas), Value::Int(len)]),
        )
        .unwrap();
    }

    fn texts(v: &tilolang_runtime::ViewInstance) -> Vec<String> {
        of_kind(v.tree(), "text")
            .into_iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    // USER REQUIREMENT (2026-10-20): "Enums are valid field types (merge: register — whole
    // value)" and "Encode enum values in the ABI as host handles (tag + payload)".
    #[test]
    fn an_enum_field_holds_the_whole_value_and_syncs_as_a_register() {
        let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
        let (rt_a, app, canvas) = setup(&store_a);
        let ids = &app.compiled.ids;
        let id = |name: &str| ids.get(name).unwrap().to_owned();
        assert_eq!(
            rt_a.read(&canvas, "shape").unwrap(),
            Value::Enum(EnumValue {
                ty: id("Shape"),
                variant: id("Shape.dot"),
                fields: std::collections::BTreeMap::default(),
            }),
            "an enum field starts as its first variant"
        );
        draw(&rt_a, &app, &canvas, 3);
        let Value::Enum(line) = rt_a.read(&canvas, "shape").unwrap() else {
            panic!("an enum value");
        };
        assert_eq!(line.variant, id("Shape.line"));
        assert_eq!(line.fields[&id("Shape.line.len")], Value::Int(3));

        let rt_b = Runtime::new(store_b.clone(), SimpleAuthority::new()).unwrap();
        app.load(&rt_b);
        store_a.sync_with(&mut store_b).unwrap();
        assert_eq!(
            rt_b.read(&canvas, "shape").unwrap(),
            Value::Enum(line),
            "the whole value arrives"
        );
        draw(&rt_a, &app, &canvas, 9);
        draw(&rt_b, &app, &canvas, 2);
        store_a.sync_with(&mut store_b).unwrap();
        let (on_a, on_b) = (
            rt_a.read(&canvas, "shape").unwrap(),
            rt_b.read(&canvas, "shape").unwrap(),
        );
        assert_eq!(on_a, on_b, "concurrent writes converge on one whole value");
        assert!(
            matches!(&on_a, Value::Enum(shape) if shape.variant == id("Shape.box") || shape.fields.get(&id("Shape.line.len")) == Some(&Value::Int(2))),
            "never a mix of the two: {on_a:?}"
        );
    }

    #[test]
    fn views_match_on_enum_values_and_compare_options_of_them() {
        let store = AmStore::new();
        let (rt, app, canvas) = setup(&store);
        let mut v = rt
            .open_view(&me(), &app.view("drawing"), vec![Value::resource(&canvas)])
            .unwrap();
        assert_eq!(texts(&v)[0], "a dot");
        draw(&rt, &app, &canvas, 9);
        v.poll_patches().unwrap();
        assert_eq!(texts(&v)[..2], ["big 1.5", "was a dot"]);
        draw(&rt, &app, &canvas, 4);
        v.poll_patches().unwrap();
        assert_eq!(texts(&v)[0], "a line of 4");
        assert!(!texts(&v).contains(&"was a dot".to_owned()));
    }

    #[test]
    fn a_bound_lens_is_the_persons_and_a_bound_window_value_is_everyones() {
        let store = AmStore::new();
        let (rt, app, canvas) = setup(&store);
        let (ada, bo) = (Principal::new("person:ada"), Principal::new("person:bo"));
        let open = |who: &Principal| {
            rt.open_view(who, &app.view("drawing"), vec![Value::resource(&canvas)])
                .unwrap()
        };
        let (mut va, mut vb) = (open(&ada), open(&bo));
        let before = rt.history(&canvas.doc).unwrap().len();
        let inputs = |v: &tilolang_runtime::ViewInstance| -> Vec<String> {
            of_kind(v.tree(), "input")
                .into_iter()
                .map(|i| i.key.clone())
                .collect()
        };
        let (query, zoom) = {
            let keys = inputs(&va);
            (keys[0].clone(), keys[1].clone())
        };
        assert_eq!(va.bound_value(&query), Some(Value::text("")));
        assert_eq!(
            va.bound_value(&zoom),
            Some(Value::Int(1)),
            "the default shows"
        );

        assert_eq!(
            va.edit(&query, &Value::text("red")).unwrap(),
            EditOutcome::Applied
        );
        assert_eq!(
            va.edit(&zoom, &Value::text("3")).unwrap(),
            EditOutcome::Applied,
            "a number typed as text"
        );
        assert!(va.edit(&zoom, &Value::text("big")).is_err());
        va.poll_patches().unwrap();
        vb.poll_patches().unwrap();
        assert_eq!(texts(&va).last().unwrap(), "red at 3");
        assert_eq!(
            texts(&vb).last().unwrap(),
            " at 3",
            "Bo shares the window, not the lens"
        );
        assert_eq!(vb.bound_value(&inputs(&vb)[0]), Some(Value::text("")));
        assert_eq!(
            rt.history(&canvas.doc).unwrap().len(),
            before,
            "view state is not resource state"
        );
    }
}
