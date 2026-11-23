//! The canvas app (`apps/tl/canvas/app.tl`) end to end: an artwork begun, layers added, a
//! stroke painted point by point, and its window drawn from the records.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value, ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/canvas/app.tl");

    struct Canvas {
        rt: Runtime,
        app: App,
        root: ResourceRef,
    }

    impl Canvas {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let root = rt.create_root("Canvas").unwrap();
            Self { rt, app, root }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn new_artwork(&self, title: &str) -> ResourceRef {
            returned_ref(self.call(
                "new_artwork",
                vec![
                    Value::resource(&self.root),
                    Value::text(title),
                    Value::Float(800.0),
                    Value::Float(600.0),
                ],
            ))
        }

        fn window(&self, art: &ResourceRef) -> ViewInstance {
            let mut view = self
                .rt
                .open_view(&me(), &self.app.view("artwork"), vec![Value::resource(art)])
                .unwrap_or_else(|e| panic!("artwork: {e}"));
            view.set_lazy_rows(false).unwrap();
            view
        }
    }

    #[test]
    fn an_artwork_is_painted_layer_by_layer() {
        let c = Canvas::new();
        let art = c.new_artwork("A study");
        assert_eq!(
            c.rt.read(&art, "title").unwrap(),
            Value::Text("A study".into())
        );
        let layers = c.rt.read(&art, "layers").unwrap();
        assert!(
            matches!(layers, Value::List(ref rows) if rows.len() == 1),
            "a new artwork starts with one layer: {layers:?}"
        );

        returned_ref(c.call(
            "add_layer",
            vec![Value::resource(&art), Value::text("Ink")],
        ));
        let stroke = returned_ref(c.call(
            "add_stroke",
            vec![
                Value::resource(&art),
                Value::None,
                Value::List(vec![
                    Value::Float(10.0),
                    Value::Float(20.0),
                    Value::Float(0.8),
                ]),
                Value::Float(6.0),
                Value::text("#1d1b19"),
                Value::text(""),
            ],
        ));
        assert_eq!(
            c.rt.read(&stroke, "kind").unwrap(),
            Value::Text("stroke".into())
        );

        let shown = c.window(&art);
        assert!(
            !of_kind(shown.tree(), "ui.PaintSurface").is_empty(),
            "the artwork is painted on a surface"
        );
        assert!(
            !of_kind(shown.tree(), "ui.ListRow").is_empty(),
            "its layers are listed"
        );
    }

    #[test]
    fn a_selection_is_filled_as_a_mark() {
        let c = Canvas::new();
        let art = c.new_artwork("Cut out");
        c.call(
            "select_area",
            vec![
                Value::resource(&art),
                Value::text("M0 0 L 100 0 L 100 100 Z"),
                Value::Float(0.0),
            ],
        );
        c.call(
            "fill_selection",
            vec![
                Value::resource(&art),
                Value::None,
                Value::text("#d24b3a"),
            ],
        );
        let marks = c.rt.read(&art, "marks").unwrap();
        assert!(
            matches!(marks, Value::List(ref rows) if rows.len() == 1),
            "the selection is filled as one mark: {marks:?}"
        );
    }
}
