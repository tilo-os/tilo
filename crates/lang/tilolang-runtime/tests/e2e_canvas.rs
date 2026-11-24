//! The canvas app (`apps/tl/canvas/app.tl`) end to end: an artwork begun, layers added, a
//! stroke painted point by point, and its window drawn from the records.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, SaveRequest};
    use tilolang_runtime::canvas::{CanvasHost, OpenedArtwork as OpenedArchive};
    use tilolang_runtime::{
        AmStore, FileValue, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value,
        ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/canvas/app.tl");

    /// An artwork file as the device's drawing engine writes it, holding a painted stroke, a
    /// shape and a selection.
    const ARTWORK_FILE: &str = r##"{
        "format": "artwork",
        "version": 1,
        "title": "A study",
        "width": 800.0,
        "height": 600.0,
        "ground": { "texture": "linen", "colour": "#ffffff" },
        "layers": [{
            "id": "layer-1",
            "name": "Ink",
            "opacity": 0.5,
            "visible": true,
            "texture": 0.25,
            "locked": true
        }],
        "marks": [
            {
                "id": "mark-0",
                "layer": "layer-1",
                "name": "painting",
                "shape": {
                    "kind": "stroke",
                    "points": [[1.0, 2.0, 0.5], [3.0, 4.0, 0.6]],
                    "size": 6.0
                },
                "style": { "brush": "pen", "stroke": "#d24b3a", "opacity": 0.8 }
            },
            {
                "id": "mark-1",
                "layer": "layer-1",
                "shape": { "kind": "path", "d": "M 0 0 L 10 10 Z" },
                "style": { "fill": "#2f7fb8" }
            }
        ],
        "selection": { "d": "M 0 0 L 5 5 Z", "feather": 2.0 }
    }"##;

    /// The device's content store, of what a test kept there: a file's hash is its bytes.
    #[derive(Clone, Default)]
    struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
            Err("this test chooses nothing".to_owned())
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = format!("h{}", bytes.len());
            self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    /// The device's drawing engine, which opens every archive as [`ARTWORK_FILE`] and keeps the
    /// artwork each call asked it to draw.
    #[derive(Clone, Default)]
    struct Drawing(Rc<RefCell<Vec<String>>>);

    fn undrawn<T>() -> Result<T, String> {
        Err("this test draws nothing".to_owned())
    }

    impl CanvasHost for Drawing {
        fn look(
            &self,
            artwork: &str,
            _request: &str,
            _pictures: &[(String, Vec<u8>)],
        ) -> Result<(Vec<u8>, String), String> {
            self.0.borrow_mut().push(artwork.to_owned());
            undrawn()
        }

        fn export(
            &self,
            _artwork: &str,
            _request: &str,
            _pictures: &[(String, Vec<u8>)],
        ) -> Result<(Vec<u8>, String), String> {
            undrawn()
        }

        fn grade(&self, _picture: &[u8], _adjust: &str) -> Result<Vec<u8>, String> {
            Ok(vec![9, 9, 9])
        }

        fn wand(&self, _picture: &[u8], _request: &str) -> Result<String, String> {
            Ok("M 10 20 L 30 20 L 30 40 Z".to_owned())
        }

        fn find(
            &self,
            _artwork: &str,
            _request: &str,
            _pictures: &[(String, Vec<u8>)],
        ) -> Result<(String, Option<(String, Vec<u8>, [f64; 4])>), String> {
            Ok((
                "M 1 1 L 9 1 L 9 9 Z".to_owned(),
                Some((
                    "tilo-blob://soft".to_owned(),
                    vec![1, 2, 3],
                    [1.0, 2.0, 30.0, 20.0],
                )),
            ))
        }

        fn combine(&self, _artwork: &str, _request: &str) -> Result<String, String> {
            Ok(
                r##"{"d":"M 0 0 L 12 0 L 12 8 Z","fill":"#2f7fb8","stroke":"","stroke_width":0.0}"##
                    .to_owned(),
            )
        }

        fn measure(
            &self,
            artwork: &str,
            _request: &str,
            _pictures: &[(String, Vec<u8>)],
        ) -> Result<String, String> {
            self.0.borrow_mut().push(artwork.to_owned());
            Ok(r#"[{"id":"mark-0","ink_area":12.5,"offset":[-1.5,2.0]}]"#.to_owned())
        }

        fn diff(
            &self,
            before: &str,
            after: &str,
            request: &str,
            _pictures: &[(String, Vec<u8>)],
        ) -> Result<(Option<Vec<u8>>, String), String> {
            self.0
                .borrow_mut()
                .push(format!("{before}|{after}|{request}"));
            Ok((
                Some(vec![7, 7, 7]),
                r#"{"same":false,"marks":[{"id":"mark-1"}],"layers":[],"artwork":[],"media":[],"changed_region":[1.0,2.0,30.0,20.0]}"#
                    .to_owned(),
            ))
        }

        fn compare(
            &self,
            artwork: &str,
            request: &str,
            pictures: &[(String, Vec<u8>)],
        ) -> Result<(Vec<u8>, String), String> {
            self.0.borrow_mut().push(format!(
                "{artwork}|{request}|{}",
                pictures
                    .iter()
                    .map(|(address, _)| address.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            Ok((vec![4, 5, 6], r#"{"format":"png","score":0.12}"#.to_owned()))
        }

        fn inspect(&self, _artwork: &str, _request: &str) -> Result<String, String> {
            Ok(r#"{"layers":[{"id":"layer-1"}],"marks":[{"id":"mark-0"}]}"#.to_owned())
        }

        fn select(&self, _artwork: &str, _request: &str) -> Result<String, String> {
            Ok(r#"{"d":"M 0 0 L 9 0 L 9 9 Z","feather":1.0}"#.to_owned())
        }

        fn remove_along(&self, _artwork: &str, _request: &str) -> Result<String, String> {
            Ok(r#"["mark-1","mark-3"]"#.to_owned())
        }

        fn svg(&self, _artwork: &str, _request: &str) -> Result<String, String> {
            undrawn()
        }

        fn svg_import(&self, _markup: &str, _request: &str) -> Result<String, String> {
            undrawn()
        }

        fn save(&self, _artwork: &str, _pictures: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
            Ok(vec![7, 8, 9])
        }

        fn open(&self, _archive: &[u8]) -> Result<OpenedArchive, String> {
            Ok((ARTWORK_FILE.to_owned(), Vec::new()))
        }
    }

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
            let root = rt.create_root("Library").unwrap();
            Self { rt, app, root }
        }

        /// A canvas whose device keeps a file and draws on it.
        fn drawing() -> Self {
            let canvas = Self::new();
            let device = Device::default();
            device.0.borrow_mut().insert("h3".to_owned(), vec![1, 2, 3]);
            canvas.rt.set_blobs(device);
            canvas.rt.set_canvas(Drawing::default());
            canvas
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

        /// The layer `name` of `art`.
        fn layer(&self, art: &ResourceRef, name: &str) -> ResourceRef {
            self.layers(art)
                .into_iter()
                .find(|layer| self.text(layer, "name") == name)
                .unwrap_or_else(|| panic!("no layer {name}"))
        }

        fn layers(&self, art: &ResourceRef) -> Vec<ResourceRef> {
            match self.rt.read(art, "layers").unwrap() {
                Value::List(rows) => rows
                    .iter()
                    .map(|row| match row {
                        Value::Ref(r) => ResourceRef::clone(&r.target),
                        other => panic!("a layer, not {other:?}"),
                    })
                    .collect(),
                other => panic!("layers, not {other:?}"),
            }
        }

        fn layer_names(&self, art: &ResourceRef) -> Vec<String> {
            self.layers(art)
                .into_iter()
                .map(|layer| self.text(&layer, "name"))
                .collect()
        }

        fn text(&self, at: &ResourceRef, field: &str) -> String {
            match self.rt.read(at, field).unwrap() {
                Value::Text(text) => text,
                other => panic!("{field} reads as text, not {other:?}"),
            }
        }

        fn float(&self, at: &ResourceRef, field: &str) -> f64 {
            match self.rt.read(at, field).unwrap() {
                Value::Float(number) => number,
                other => panic!("{field} reads as a number, not {other:?}"),
            }
        }

        fn flag(&self, at: &ResourceRef, field: &str) -> bool {
            match self.rt.read(at, field).unwrap() {
                Value::Bool(flag) => flag,
                other => panic!("{field} reads as a flag, not {other:?}"),
            }
        }

        /// The marks of `art`, in the order they were laid.
        fn marks(&self, art: &ResourceRef) -> Vec<ResourceRef> {
            match self.rt.read(art, "marks").unwrap() {
                Value::List(rows) => rows
                    .iter()
                    .map(|row| match row {
                        Value::Ref(r) => ResourceRef::clone(&r.target),
                        other => panic!("a mark, not {other:?}"),
                    })
                    .collect(),
                other => panic!("marks, not {other:?}"),
            }
        }

        /// Paints one stroke with `tool`, from `from` through `through` to `to`, as the surface
        /// reports the hand: a start, a move and a lift.
        fn drag(
            &self,
            art: &ResourceRef,
            tool: &str,
            from: [f64; 2],
            through: [f64; 2],
            to: [f64; 2],
        ) -> InvokeOutcome {
            let point = |[x, y]: [f64; 2]| {
                Value::List(vec![Value::Float(x), Value::Float(y), Value::Float(0.5)])
            };
            self.call(
                "start_paint",
                vec![
                    Value::resource(art),
                    Value::text(tool),
                    Value::text("pen"),
                    Value::none(),
                    point(from),
                    Value::Float(6.0),
                    Value::text("#1d1b19"),
                ],
            );
            self.call(
                "continue_paint",
                vec![
                    Value::resource(art),
                    Value::text(tool),
                    Value::text("pen"),
                    point(through),
                    Value::Float(6.0),
                    Value::text("#1d1b19"),
                ],
            );
            self.call(
                "finish_paint",
                vec![
                    Value::resource(art),
                    Value::text(tool),
                    Value::text("pen"),
                    Value::none(),
                    point(from),
                    point(to),
                    Value::Float(6.0),
                    Value::text("#1d1b19"),
                ],
            )
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

        returned_ref(c.call("add_layer", vec![Value::resource(&art), Value::text("Ink")]));
        let stroke = returned_ref(c.call(
            "add_stroke",
            vec![
                Value::resource(&art),
                Value::none(),
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
            vec![Value::resource(&art), Value::none(), Value::text("#d24b3a")],
        );
        let marks = c.rt.read(&art, "marks").unwrap();
        assert!(
            matches!(marks, Value::List(ref rows) if rows.len() == 1),
            "the selection is filled as one mark: {marks:?}"
        );
    }

    #[test]
    fn each_stroke_the_hand_lays_is_a_mark_of_its_own() {
        let c = Canvas::new();
        let art = c.new_artwork("Two strokes");
        c.drag(&art, "brush", [10.0, 20.0], [15.0, 25.0], [20.0, 30.0]);
        c.drag(&art, "brush", [40.0, 40.0], [45.0, 45.0], [50.0, 50.0]);
        let marks = c.marks(&art);
        assert_eq!(marks.len(), 2, "a lifted stroke is finished: {marks:?}");
        assert_eq!(
            c.text(&marks[0], "brush"),
            "pen",
            "the stroke keeps the preset it was laid with"
        );
        assert!(!c.flag(&marks[0], "open"), "a lifted stroke is closed");
        assert!(!c.flag(&marks[1], "open"));
        // Each stroke holds its own points, with the pressure the surface reported.
        match c.rt.read(&marks[1], "points").unwrap() {
            Value::List(points) => {
                assert_eq!(
                    points.len(),
                    9,
                    "the start, the move and the lift of x, y, pressure"
                )
            }
            other => panic!("points, not {other:?}"),
        }
    }

    #[test]
    fn a_shape_tool_draws_the_outline_the_drag_makes() {
        let c = Canvas::new();
        let art = c.new_artwork("Shapes");
        c.drag(&art, "rect", [0.0, 0.0], [50.0, 25.0], [100.0, 50.0]);
        c.drag(&art, "ellipse", [0.0, 0.0], [25.0, 25.0], [50.0, 50.0]);
        c.drag(&art, "line", [0.0, 0.0], [50.0, 0.0], [100.0, 0.0]);
        let marks = c.marks(&art);
        assert_eq!(marks.len(), 3, "one mark for each drawn shape: {marks:?}");
        assert_eq!(c.text(&marks[0], "kind"), "path");
        assert_eq!(
            c.text(&marks[0], "d"),
            "M 0 0 L 100 0 L 100 50 L 0 50 Z",
            "the rectangle spans the drag"
        );
        assert!(
            c.text(&marks[1], "d").contains("A 25 25"),
            "the ellipse's arcs"
        );
        assert_eq!(
            c.text(&marks[2], "d"),
            "M 0 3 L 100 3 L 100 -3 L 0 -3 Z",
            "a line as wide as the brush"
        );
        // A shape tool starts no stroke while the hand is down.
        for mark in &marks {
            assert_eq!(c.text(mark, "kind"), "path");
        }
    }

    #[test]
    fn the_select_tool_keeps_the_dragged_area_and_fills_it() {
        let c = Canvas::new();
        let art = c.new_artwork("Cut");
        c.drag(&art, "select", [0.0, 0.0], [10.0, 10.0], [100.0, 50.0]);
        assert!(c.marks(&art).is_empty(), "selecting draws nothing");
        let selection = c.rt.read(&art, "selection").unwrap();
        assert!(
            format!("{selection:?}").contains("M 0 0 L 100 0"),
            "the selection keeps the rectangle the drag made: {selection:?}"
        );
        c.call(
            "fill_selection",
            vec![Value::resource(&art), Value::none(), Value::text("#2f7fb8")],
        );
        assert_eq!(c.marks(&art).len(), 1);
    }

    #[test]
    fn layers_are_locked_moved_copied_and_merged() {
        let c = Canvas::new();
        let art = c.new_artwork("Layers");
        c.call("add_layer", vec![Value::resource(&art), Value::text("Ink")]);
        let ink = c.layer(&art, "Ink");

        // A stroke lands on the top layer that takes paint: Ink.
        c.drag(&art, "brush", [1.0, 2.0], [3.0, 4.0], [5.0, 6.0]);
        assert_eq!(c.marks(&art).len(), 1);

        // Held still, Ink takes no more paint; the layer under it moves above it.
        c.call(
            "lock_layer",
            vec![
                Value::resource(&art),
                Value::resource(&ink),
                Value::Bool(true),
            ],
        );
        assert!(c.flag(&ink, "locked"));
        let base = c.layer(&art, "Layer 1");
        c.call(
            "move_layer_up",
            vec![Value::resource(&art), Value::resource(&base)],
        );
        assert_eq!(c.layer_names(&art), ["Ink", "Layer 1"]);

        // The stroke on Ink is copied with the layer, then folded back into it.
        let copy = returned_ref(c.call(
            "duplicate_layer",
            vec![Value::resource(&art), Value::resource(&ink)],
        ));
        assert_eq!(c.text(&copy, "name"), "Ink copy");
        assert_eq!(c.layer_names(&art), ["Ink", "Ink copy", "Layer 1"]);
        assert_eq!(c.marks(&art).len(), 2, "the copy carries the marks");
        c.call(
            "merge_layer_down",
            vec![Value::resource(&art), Value::resource(&copy)],
        );
        assert_eq!(c.layer_names(&art), ["Ink", "Layer 1"]);
        assert_eq!(
            c.marks(&art).len(),
            2,
            "the marks joined the layer under it"
        );
    }

    #[test]
    fn a_picture_placed_in_the_artwork_is_its_own_mark_and_the_artworks_picture() {
        let c = Canvas::new();
        let art = c.new_artwork("A picture");
        let device = Device::default();
        device.0.borrow_mut().insert("h3".to_owned(), vec![1, 2, 3]);
        c.rt.set_blobs(device);
        let file = FileValue::new("Dawn.png", "image/png", 3, "h3");
        let mark = returned_ref(c.call(
            "place_picture",
            vec![Value::resource(&art), Value::file(file)],
        ));
        assert_eq!(c.text(&mark, "kind"), "image");
        let src = c.text(&mark, "src");
        assert!(!src.is_empty(), "the mark names the picture's bytes");
        assert_eq!(c.float(&mark, "width"), 800.0, "an artwork-wide picture");
        assert_eq!(c.text(&mark, "name"), "Dawn.png");
        let pictures = c.marks(&art).len();
        assert_eq!(pictures, 1, "the picture is one mark");
        match c.rt.read(&art, "pictures").unwrap() {
            Value::List(rows) => {
                assert_eq!(rows.len(), 1, "the artwork lists the picture it shows");
                match &rows[0] {
                    Value::Ref(r) => {
                        let held = ResourceRef::clone(&r.target);
                        assert_eq!(c.text(&held, "url"), src);
                    }
                    other => panic!("a picture, not {other:?}"),
                }
            }
            other => panic!("pictures, not {other:?}"),
        }
    }

    #[test]
    fn shapes_combine_ink_is_measured_and_the_reference_compares() {
        let c = Canvas::new();
        let art = c.new_artwork("Balance");
        let log = Rc::new(RefCell::new(Vec::new()));
        let device = Device::default();
        device.0.borrow_mut().insert("h3".to_owned(), vec![1, 2, 3]);
        c.rt.set_blobs(device);
        c.rt.set_canvas(Drawing(log.clone()));
        let one = returned_ref(c.call(
            "add_shape",
            vec![
                Value::resource(&art),
                Value::none(),
                Value::text("M 0 0 L 10 0 L 10 10 Z"),
                Value::text("#2f7fb8"),
            ],
        ));
        let two = returned_ref(c.call(
            "add_shape",
            vec![
                Value::resource(&art),
                Value::none(),
                Value::text("M 2 2 L 12 2 L 12 8 Z"),
                Value::text("#d24b3a"),
            ],
        ));
        let combined = match c.call(
            "combine_marks",
            vec![
                Value::resource(&art),
                Value::List(vec![Value::resource(&one), Value::resource(&two)]),
                Value::text("union"),
            ],
        ) {
            InvokeOutcome::Done(Value::Opt(Some(held))) => match *held {
                Value::Ref(at) => ResourceRef::clone(&at.target),
                other => panic!("the combined mark, not {other:?}"),
            },
            other => panic!("the combined mark, not {other:?}"),
        };
        assert_eq!(c.text(&combined, "d"), "M 0 0 L 12 0 L 12 8 Z");
        assert_eq!(c.text(&combined, "kind"), "path");
        assert_eq!(c.text(&combined, "colour"), "#2f7fb8");
        assert_eq!(
            c.marks(&art).len(),
            1,
            "the shapes became one mark, in the bottom one's place"
        );

        let measured = match c.call(
            "measure_marks",
            vec![
                Value::resource(&art),
                Value::List(vec![Value::resource(&combined)]),
            ],
        ) {
            InvokeOutcome::Done(Value::Opt(Some(held))) => match *held {
                Value::Text(answer) => answer,
                other => panic!("the measurement, not {other:?}"),
            },
            other => panic!("the measurement, not {other:?}"),
        };
        assert!(
            measured.contains("mark-0"),
            "the engine measured the mark: {measured}"
        );
        assert!(
            c.text(&art, "notice").contains("units of ink"),
            "the window says what the ink weighs: {}",
            c.text(&art, "notice")
        );

        let file = FileValue::new("Dawn.png", "image/png", 3, "h3");
        c.call(
            "set_reference",
            vec![Value::resource(&art), Value::file(file), Value::Float(0.5)],
        );
        let compared = c.call(
            "compare_reference",
            vec![
                Value::resource(&art),
                Value::text("overlay"),
                Value::Int(640),
            ],
        );
        match compared {
            InvokeOutcome::Done(Value::Opt(Some(held))) => match *held {
                Value::File(made) => assert_eq!(made.media, "image/png"),
                other => panic!("the comparison, not {other:?}"),
            },
            other => panic!("the comparison, not {other:?}"),
        }
        let seen = log.borrow().last().cloned().expect("the engine compared");
        assert!(
            seen.contains("\"reference\""),
            "the engine is given the reference: {seen}"
        );
        assert!(
            seen.contains("h3"),
            "the reference's picture is lent to the engine: {seen}"
        );
        assert!(seen.contains("\"mode\":\"overlay\""), "the mode: {seen}");

        let actions: Vec<String> =
            c.rt.versions(&art)
                .unwrap()
                .into_iter()
                .map(|version| version.action)
                .collect();
        for action in [
            "combine_marks",
            "measure_marks",
            "set_reference",
            "compare_reference",
        ] {
            let id = c.app.action(action);
            assert!(
                actions.iter().any(|made| made == &id),
                "{action} is Canvas's change in the system's history: {actions:?}"
            );
        }
    }

    #[test]
    fn marks_are_picked_moved_clipped_and_finished() {
        let c = Canvas::new();
        let art = c.new_artwork("Marks");
        let log = Rc::new(RefCell::new(Vec::new()));
        c.rt.set_blobs(Device::default());
        c.rt.set_canvas(Drawing(log.clone()));
        let mut made = Vec::new();
        for d in [
            "M 0 0 L 4 0 L 4 4 Z",
            "M 6 0 L 9 0 L 9 4 Z",
            "M 2 6 L 5 6 L 5 9 Z",
        ] {
            made.push(returned_ref(c.call(
                "add_shape",
                vec![
                    Value::resource(&art),
                    Value::none(),
                    Value::text(d),
                    Value::text("#2f7fb8"),
                ],
            )));
        }
        c.call(
            "pick_marks",
            vec![
                Value::resource(&art),
                Value::List(vec![Value::resource(&made[0]), Value::resource(&made[1])]),
            ],
        );
        match c.rt.read(&art, "picked") {
            Ok(Value::List(rows)) => assert_eq!(rows.len(), 2, "two marks picked"),
            other => panic!("picked, not {other:?}"),
        }
        let combined = c.call(
            "combine_marks",
            vec![
                Value::resource(&art),
                Value::List(Vec::new()),
                Value::text("union"),
            ],
        );
        match combined {
            InvokeOutcome::Done(Value::Opt(Some(_))) => {}
            other => panic!("the combined mark, not {other:?}"),
        }
        let remaining = c.marks(&art);
        assert_eq!(
            remaining.len(),
            2,
            "two picked became one; the third stayed"
        );
        let kept = remaining[0].clone();
        c.call(
            "move_mark",
            vec![Value::resource(&kept), Value::Float(3.0), Value::Float(4.0)],
        );
        c.call(
            "rotate_mark",
            vec![
                Value::resource(&kept),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(90.0),
            ],
        );
        let transform = match c.rt.read(&kept, "transform").unwrap() {
            Value::List(values) => values.len(),
            other => panic!("a transform, not {other:?}"),
        };
        assert_eq!(transform, 6, "a move and a turn compose into one affine");
        c.call(
            "clip_mark",
            vec![Value::resource(&kept), Value::text("icons")],
        );
        c.call(
            "finish_mark",
            vec![
                Value::resource(&kept),
                Value::Bool(true),
                Value::Float(0.5),
                Value::Float(4.0),
                Value::Float(2.0),
                Value::Float(3.0),
                Value::Float(1.0),
                Value::text("#00000040"),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(2.0),
                Value::text(""),
            ],
        );
        c.call(
            "set_dynamics",
            vec![Value::resource(&kept), Value::Float(0.4), Value::Float(0.7)],
        );
        let inspected = c.call(
            "inspect_artwork",
            vec![Value::resource(&art), Value::Bool(false)],
        );
        match inspected {
            InvokeOutcome::Done(Value::Opt(Some(held))) => match *held {
                Value::Text(info) => assert!(info.contains("marks"), "{info}"),
                other => panic!("the inspection, not {other:?}"),
            },
            other => panic!("the inspection, not {other:?}"),
        }
        assert!(
            c.text(&art, "notice").contains("marks listed"),
            "the window says what it listed: {}",
            c.text(&art, "notice")
        );
        c.call(
            "set_curve",
            vec![
                Value::resource(&kept),
                Value::List(vec![
                    Value::List(vec![Value::Float(0.1), Value::Float(0.0)]),
                    Value::List(vec![Value::Float(0.9), Value::Float(1.0)]),
                ]),
            ],
        );
        let layer = c.layers(&art).first().cloned().expect("a layer");
        c.call(
            "adjust_layer",
            vec![
                Value::resource(&layer),
                Value::Float(0.5),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Bool(false),
                Value::Bool(false),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
            ],
        );
        c.call("draw_look", vec![Value::resource(&art), Value::Int(128)]);
        let seen = log.borrow().first().cloned().expect("the engine was asked");
        for field in [
            "\"transform\":[",
            "\"clip\":\"icons\"",
            "\"pixelated\":true",
            "\"grain\":0.5",
            "\"feather\":4.0",
            "\"inner_shadow_blur\":2.0",
            "\"curve\":[[0.1,0.0],[0.9,1.0]]",
            "\"exposure\":0.5",
            "\"flow\":0.4",
            "\"hardness\":0.7",
        ] {
            assert!(seen.contains(field), "the engine reads {field}: {seen}");
        }
    }

    #[test]
    fn selections_lasso_grow_invert_and_remove_along() {
        let c = Canvas::new();
        let art = c.new_artwork("Selecting");
        let log = Rc::new(RefCell::new(Vec::new()));
        c.rt.set_blobs(Device::default());
        c.rt.set_canvas(Drawing(log.clone()));
        let selected = c.call(
            "select_shape",
            vec![
                Value::resource(&art),
                Value::text("rect"),
                Value::List(vec![Value::Float(1.0), Value::Float(1.0)]),
                Value::List(vec![Value::Float(9.0), Value::Float(9.0)]),
                Value::text("add"),
                Value::Float(2.0),
                Value::Float(0.0),
                Value::Bool(false),
            ],
        );
        match selected {
            InvokeOutcome::Done(Value::Opt(Some(held))) => match *held {
                Value::Text(answer) => {
                    assert!(answer.contains("M 0 0 L 9 0"), "{answer}");
                    assert!(answer.contains("\"feather\":1.0"), "{answer}");
                }
                other => panic!("the selection, not {other:?}"),
            },
            other => panic!("the selection, not {other:?}"),
        }
        let lasso = c.call(
            "select_lasso",
            vec![
                Value::resource(&art),
                Value::List(vec![
                    Value::List(vec![Value::Float(0.0), Value::Float(0.0)]),
                    Value::List(vec![Value::Float(5.0), Value::Float(0.0)]),
                    Value::List(vec![Value::Float(5.0), Value::Float(5.0)]),
                ]),
            ],
        );
        match lasso {
            InvokeOutcome::Done(Value::Opt(Some(_))) => {}
            other => panic!("the lassoed area, not {other:?}"),
        }
        c.call(
            "grow_selection",
            vec![Value::resource(&art), Value::Float(3.0)],
        );
        c.call("invert_selection", vec![Value::resource(&art)]);
        for d in [
            "M 0 0 L 1 0 L 1 1 Z",
            "M 2 2 L 3 2 L 3 3 Z",
            "M 4 4 L 5 4 L 5 5 Z",
            "M 6 6 L 7 6 L 7 7 Z",
        ] {
            c.call(
                "add_shape",
                vec![
                    Value::resource(&art),
                    Value::none(),
                    Value::text(d),
                    Value::text("#2f7fb8"),
                ],
            );
        }
        let removed = c.call(
            "remove_along",
            vec![
                Value::resource(&art),
                Value::List(vec![
                    Value::List(vec![Value::Float(0.0), Value::Float(0.0)]),
                    Value::List(vec![Value::Float(9.0), Value::Float(9.0)]),
                ]),
                Value::none(),
            ],
        );
        assert_eq!(removed, InvokeOutcome::Done(Value::Int(2)));
        assert_eq!(c.marks(&art).len(), 2, "the crossed strokes went");
    }

    #[test]
    fn a_version_compares_over_the_surface() {
        let c = Canvas::new();
        let art = c.new_artwork("Versions");
        let log = Rc::new(RefCell::new(Vec::new()));
        c.rt.set_blobs(Device::default());
        c.rt.set_canvas(Drawing(log.clone()));
        c.call(
            "add_shape",
            vec![
                Value::resource(&art),
                Value::none(),
                Value::text("M 0 0 L 4 0 L 4 4 Z"),
                Value::text("#2f7fb8"),
            ],
        );
        let versions = c.rt.versions(&art).unwrap();
        let write = c.app.action("add_shape");
        let change = versions
            .iter()
            .rev()
            .find(|version| version.action == write)
            .expect("the shape's version")
            .change
            .clone();
        c.call(
            "add_shape",
            vec![
                Value::resource(&art),
                Value::none(),
                Value::text("M 8 8 L 12 8 L 12 12 Z"),
                Value::text("#d24b3a"),
            ],
        );
        let compared = c.call(
            "compare_versions",
            vec![Value::resource(&art), Value::text(&change)],
        );
        match compared {
            InvokeOutcome::Done(Value::Opt(Some(held))) => match *held {
                Value::Text(info) => assert!(info.contains("changed_region"), "{info}"),
                other => panic!("the comparison, not {other:?}"),
            },
            other => panic!("the comparison, not {other:?}"),
        }
        assert!(
            c.text(&art, "version_note").contains("Changed"),
            "the window says what changed: {}",
            c.text(&art, "version_note")
        );
        assert_eq!(
            c.rt.read(&art, "version_region").unwrap(),
            Value::List(vec![
                Value::Float(1.0),
                Value::Float(2.0),
                Value::Float(30.0),
                Value::Float(20.0),
            ]),
            "the region the surface lays the comparison over"
        );
        assert!(
            c.text(&art, "version_overlay") != "",
            "the comparison's picture is kept for the surface"
        );
        let seen = log.borrow().last().cloned().expect("the engine compared");
        let (before, rest) = seen.split_once('|').expect("before|after|request");
        let (after, request) = rest.split_once('|').expect("after|request");
        assert!(request.contains("\"overlay\":true"), "{request}");
        assert_eq!(
            before.matches("\"kind\":\"path\"").count(),
            1,
            "the version holds the first shape: {before}"
        );
        assert_eq!(
            after.matches("\"kind\":\"path\"").count(),
            2,
            "the artwork now holds both: {after}"
        );
    }

    #[test]
    fn a_picture_is_cropped_turned_flipped_blended_and_graded() {
        let c = Canvas::new();
        let art = c.new_artwork("A photo");
        let device = Device::default();
        device.0.borrow_mut().insert("h3".to_owned(), vec![1, 2, 3]);
        c.rt.set_blobs(device);
        let log = Rc::new(RefCell::new(Vec::new()));
        c.rt.set_canvas(Drawing(log.clone()));
        let file = FileValue::new("Dawn.png", "image/png", 3, "h3");
        let picture = returned_ref(c.call(
            "place_picture",
            vec![Value::resource(&art), Value::file(file)],
        ));
        c.call(
            "crop_picture",
            vec![
                Value::resource(&picture),
                Value::Float(0.25),
                Value::Float(0.25),
                Value::Float(0.5),
                Value::Float(0.5),
            ],
        );
        c.call(
            "straighten_picture",
            vec![Value::resource(&picture), Value::Float(3.0)],
        );
        c.call(
            "flip_picture",
            vec![
                Value::resource(&picture),
                Value::Bool(true),
                Value::Bool(false),
            ],
        );
        c.call(
            "blend_mark",
            vec![Value::resource(&picture), Value::text("multiply")],
        );
        c.call(
            "adjust_mark",
            vec![
                Value::resource(&picture),
                Value::Float(1.5),  // exposure
                Value::Float(0.2),  // contrast
                Value::Float(0.0),  // highlights
                Value::Float(0.0),  // shadows
                Value::Float(0.0),  // saturation
                Value::Float(0.0),  // vibrance
                Value::Float(0.0),  // hue
                Value::Float(0.0),  // temperature
                Value::Float(0.0),  // tint
                Value::Bool(false), // grayscale
                Value::Bool(false), // invert
                Value::Float(0.0),  // blur
                Value::Float(0.0),  // sharpen
                Value::Float(0.0),  // vignette
            ],
        );
        assert_eq!(c.float(&picture, "crop_x"), 0.25);
        assert_eq!(c.float(&picture, "crop_width"), 0.5);
        assert_eq!(c.float(&picture, "straighten"), 3.0);
        assert!(c.flag(&picture, "flip_h"), "the picture stands mirrored");
        assert_eq!(c.text(&picture, "blend"), "multiply");
        assert_eq!(c.float(&picture, "exposure"), 1.5);
        // The engine reads all of it from the records.
        c.call("draw_look", vec![Value::resource(&art), Value::Int(128)]);
        let seen = log.borrow().first().cloned().expect("the engine was asked");
        for field in [
            "\"crop_x\":0.25",
            "\"straighten\":3.0",
            "\"flip_h\":true",
            "\"blend\":\"multiply\"",
            "\"exposure\":1.5",
        ] {
            assert!(seen.contains(field), "the engine reads {field}: {seen}");
        }
    }

    #[test]
    fn a_picture_is_graded_into_a_new_file() {
        let c = Canvas::new();
        let device = Device::default();
        device.0.borrow_mut().insert("h3".to_owned(), vec![1, 2, 3]);
        c.rt.set_blobs(device);
        c.rt.set_canvas(Drawing::default());
        let file = FileValue::new("Dawn.png", "image/png", 3, "h3");
        let graded = c.call(
            "grade_picture",
            vec![
                Value::file(file),
                Value::Float(0.5), // exposure
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Bool(true), // grayscale
                Value::Bool(false),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(0.0),
            ],
        );
        match graded {
            InvokeOutcome::Done(Value::Opt(Some(held))) => match *held {
                Value::File(made) => assert_eq!(made.media, "image/png"),
                other => panic!("a graded file, not {other:?}"),
            },
            other => panic!("a graded file, not {other:?}"),
        }
    }

    #[test]
    fn a_layer_is_masked_clipped_and_grouped() {
        let c = Canvas::new();
        let art = c.new_artwork("Masks");
        let log = Rc::new(RefCell::new(Vec::new()));
        c.rt.set_blobs(Device::default());
        c.rt.set_canvas(Drawing(log.clone()));
        c.call("add_layer", vec![Value::resource(&art), Value::text("Ink")]);
        let ink = c.layer(&art, "Ink");
        let mask = returned_ref(c.call(
            "add_mask",
            vec![Value::resource(&art), Value::resource(&ink)],
        ));
        assert_eq!(c.text(&mask, "name"), "Ink mask");
        assert_eq!(c.text(&mask, "mask_of"), "Ink");
        assert_eq!(c.layer_names(&art), ["Layer 1", "Ink", "Ink mask"]);
        c.call(
            "set_mask",
            vec![
                Value::resource(&art),
                Value::resource(&mask),
                Value::Float(0.4),
                Value::Bool(true),
                Value::Bool(true),
            ],
        );
        assert_eq!(c.float(&mask, "base"), 0.4);
        assert!(c.flag(&mask, "invert"));
        assert!(c.flag(&mask, "unlinked"));
        c.call(
            "clip_layer",
            vec![
                Value::resource(&art),
                Value::resource(&ink),
                Value::Bool(true),
            ],
        );
        assert!(c.flag(&ink, "clip"));
        c.call(
            "set_layer_group",
            vec![Value::resource(&ink), Value::text("icons")],
        );
        assert_eq!(c.text(&ink, "group"), "icons");
        let stroke = returned_ref(c.call(
            "add_stroke",
            vec![
                Value::resource(&art),
                Value::some(Value::resource(&ink)),
                Value::List(vec![
                    Value::Float(1.0),
                    Value::Float(2.0),
                    Value::Float(1.0),
                ]),
                Value::Float(6.0),
                Value::text("#1d1b19"),
                Value::text(""),
            ],
        ));
        c.call(
            "group_mark",
            vec![Value::resource(&stroke), Value::text("icons")],
        );
        assert_eq!(c.text(&stroke, "group"), "icons");
        c.call("draw_look", vec![Value::resource(&art), Value::Int(128)]);
        let seen = log.borrow().first().cloned().expect("the engine was asked");
        for field in [
            "\"mask_of\":\"Ink\"",
            "\"base\":0.4",
            "\"invert\":true",
            "\"clip\":true",
            "\"group\":\"icons\"",
        ] {
            assert!(seen.contains(field), "the engine reads {field}: {seen}");
        }
    }

    #[test]
    fn the_wand_selects_what_a_colour_touches() {
        let c = Canvas::new();
        let art = c.new_artwork("Wand");
        let device = Device::default();
        device.0.borrow_mut().insert("h3".to_owned(), vec![1, 2, 3]);
        c.rt.set_blobs(device);
        c.rt.set_canvas(Drawing::default());
        let file = FileValue::new("Dawn.png", "image/png", 3, "h3");
        c.call(
            "place_picture",
            vec![Value::resource(&art), Value::file(file)],
        );
        let found = c.call(
            "select_like",
            vec![
                Value::resource(&art),
                Value::List(vec![Value::Float(3.0), Value::Float(3.0)]),
                Value::text(""),
                Value::Float(0.15),
                Value::Bool(false),
            ],
        );
        match found {
            InvokeOutcome::Done(Value::Opt(Some(held))) => match *held {
                Value::Text(d) => assert!(d.contains("M 10 20"), "the area the engine found: {d}"),
                other => panic!("the found area, not {other:?}"),
            },
            other => panic!("the found area, not {other:?}"),
        }
        let selection = c.rt.read(&art, "selection").unwrap();
        assert!(
            format!("{selection:?}").contains("M 10 20 L 30 20"),
            "the found area is the artwork's selection: {selection:?}"
        );
    }

    #[test]
    fn the_subject_and_a_refined_edge_come_back_as_selections() {
        let c = Canvas::new();
        let art = c.new_artwork("Subject");
        c.rt.set_blobs(Device::default());
        c.rt.set_canvas(Drawing::default());
        let subject = c.call(
            "select_subject",
            vec![Value::resource(&art), Value::Float(0.0)],
        );
        match subject {
            InvokeOutcome::Done(Value::Opt(Some(held))) => match *held {
                Value::Text(answer) => {
                    assert!(answer.contains("M 1 1"), "the subject's area: {answer}");
                    assert!(
                        answer.contains("matte_src"),
                        "the soft edge's picture comes with the area: {answer}"
                    );
                    assert!(
                        answer.contains("\"matte_rect\":[1.0,2.0,30.0,20.0]"),
                        "{answer}"
                    );
                }
                other => panic!("the subject's area, not {other:?}"),
            },
            other => panic!("the subject's area, not {other:?}"),
        }
        c.call(
            "select_area",
            vec![
                Value::resource(&art),
                Value::text("M 0 0 L 100 0 L 100 100 Z"),
                Value::Float(0.0),
            ],
        );
        c.call(
            "refine_selection",
            vec![Value::resource(&art), Value::Float(6.0)],
        );
        let selection = c.rt.read(&art, "selection").unwrap();
        let shown = format!("{selection:?}");
        assert!(
            shown.contains("M 1 1 L 9 1"),
            "the refined edge is the artwork's selection: {shown}"
        );
        // The device keeps the engine's soft edge under its own address, with the frame it fills.
        assert!(
            shown.contains("tilo-blob://")
                && shown.contains("Float(30.0)")
                && shown.contains("Float(20.0)"),
            "it keeps the soft edge: {shown}"
        );
    }

    #[test]
    fn text_and_a_gradient_are_marks_the_engine_draws() {
        let c = Canvas::new();
        let art = c.new_artwork("Type");
        let log = Rc::new(RefCell::new(Vec::new()));
        c.rt.set_blobs(Device::default());
        c.rt.set_canvas(Drawing(log.clone()));
        let text = returned_ref(c.call(
            "add_text",
            vec![
                Value::resource(&art),
                Value::none(),
                Value::text("Hello"),
                Value::Float(40.0),
                Value::Float(60.0),
                Value::Float(48.0),
                Value::text("#1d1b19"),
                Value::text("serif"),
                Value::Int(700),
                Value::Bool(true),
                Value::text("center"),
                Value::Float(1.2),
                Value::Float(40.0),
            ],
        ));
        assert_eq!(c.text(&text, "kind"), "text");
        assert_eq!(c.text(&text, "text"), "Hello");
        c.call(
            "set_text",
            vec![Value::resource(&text), Value::text("Hello, Tilo")],
        );
        assert_eq!(c.text(&text, "text"), "Hello, Tilo");
        let shape = returned_ref(c.call(
            "add_shape",
            vec![
                Value::resource(&art),
                Value::none(),
                Value::text("M 0 0 L 100 0 L 100 100 Z"),
                Value::text("#2f7fb8"),
            ],
        ));
        c.call(
            "fill_gradient",
            vec![
                Value::resource(&shape),
                Value::text("linear"),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Float(100.0),
                Value::Float(100.0),
                Value::List(vec![Value::Float(0.0), Value::Float(1.0)]),
                Value::List(vec![Value::text("#2f7fb8"), Value::text("#2f7fb800")]),
            ],
        );
        assert_eq!(c.text(&shape, "gradient"), "linear");
        c.call("draw_look", vec![Value::resource(&art), Value::Int(128)]);
        let seen = log.borrow().first().cloned().expect("the engine was asked");
        for field in [
            "\"kind\":\"text\"",
            "\"text\":\"Hello, Tilo\"",
            "\"weight\":700",
            "\"gradient_kind\":\"linear\"",
            "\"gradient_offsets\":[0.0,1.0]",
        ] {
            assert!(seen.contains(field), "the engine reads {field}: {seen}");
        }
    }

    #[test]
    fn a_layer_is_painted_with_a_saved_medium() {
        let c = Canvas::new();
        let art = c.new_artwork("Oil");
        let log = Rc::new(RefCell::new(Vec::new()));
        c.rt.set_blobs(Device::default());
        c.rt.set_canvas(Drawing(log.clone()));
        let medium = returned_ref(c.call(
            "save_medium",
            vec![
                Value::resource(&art),
                Value::text("studio-oil"),
                Value::text("Studio oil"),
                Value::text("oil"),
                Value::text("Loose and glossy"),
                Value::text("linen"),
            ],
        ));
        assert_eq!(c.text(&medium, "label"), "Studio oil");
        assert_eq!(c.text(&medium, "key"), "studio-oil");
        match c.rt.read(&art, "media").unwrap() {
            Value::List(rows) => assert_eq!(rows.len(), 1, "the medium is saved in the artwork"),
            other => panic!("media, not {other:?}"),
        }
        let layer = c.layers(&art).first().cloned().expect("a layer");
        c.call(
            "set_medium",
            vec![
                Value::resource(&art),
                Value::resource(&layer),
                Value::text("studio-oil"),
            ],
        );
        assert_eq!(c.text(&layer, "medium"), "studio-oil");
        let stroke = returned_ref(c.call(
            "add_stroke",
            vec![
                Value::resource(&art),
                Value::some(Value::resource(&layer)),
                Value::List(vec![
                    Value::Float(1.0),
                    Value::Float(2.0),
                    Value::Float(1.0),
                ]),
                Value::Float(6.0),
                Value::text("#1d1b19"),
                Value::text("round"),
            ],
        ));
        c.call(
            "wet_mark",
            vec![Value::resource(&stroke), Value::Bool(true)],
        );
        assert!(c.flag(&stroke, "wet"));
        c.call("draw_look", vec![Value::resource(&art), Value::Int(128)]);
        let seen = log.borrow().first().cloned().expect("the engine was asked");
        for field in [
            "\"medium\":\"studio-oil\"",
            "\"wet\":true",
            "\"id\":\"studio-oil\"",
            "\"engine\":\"oil\"",
        ] {
            assert!(seen.contains(field), "the engine reads {field}: {seen}");
        }
    }

    #[test]
    fn the_view_zooms_and_pans_through_the_surface() {
        let c = Canvas::new();
        let art = c.new_artwork("View");
        let mut view = c.window(&art);
        let surface = of_kind(view.tree(), "ui.PaintSurface")[0].key.clone();
        let zoom_of =
            |view: &ViewInstance| match view.tree().find(&surface).unwrap().props.get("zoom") {
                Some(Value::Float(zoom)) => *zoom,
                other => panic!("the surface's zoom, not {other:?}"),
            };
        assert_eq!(zoom_of(&view), 1.0, "an artwork opens at its own size");

        // The wheel up zooms in, and a pinch scales what is shown.
        view.fire_with(
            &surface,
            "wheel",
            Some(&Value::List(vec![Value::Float(0.0), Value::Float(-100.0)])),
        )
        .unwrap();
        view.poll_patches().unwrap();
        assert!(
            (zoom_of(&view) - 1.2).abs() < 1e-9,
            "a wheel notch of 100 zooms by a fifth"
        );

        view.fire_with(&surface, "pinch", Some(&Value::Float(2.0)))
            .unwrap();
        view.poll_patches().unwrap();
        assert!(
            (zoom_of(&view) - 2.4).abs() < 1e-9,
            "a pinch scales the view"
        );

        // A middle-button drag pans it, in artwork units.
        view.fire_with(
            &surface,
            "panned",
            Some(&Value::List(vec![Value::Float(10.0), Value::Float(20.0)])),
        )
        .unwrap();
        view.poll_patches().unwrap();
        match view.tree().find(&surface).unwrap().props.get("pan") {
            Some(Value::List(pan)) => assert_eq!(
                pan,
                &vec![Value::Float(10.0), Value::Float(20.0)],
                "the view moved by the drag"
            ),
            other => panic!("the surface's pan, not {other:?}"),
        }
    }

    #[test]
    fn the_engine_is_asked_to_draw_the_artwork_the_app_keeps() {
        let c = Canvas::new();
        let log = Rc::new(RefCell::new(Vec::new()));
        c.rt.set_blobs(Device::default());
        c.rt.set_canvas(Drawing(log.clone()));
        let art = c.new_artwork("Seen");
        c.drag(&art, "brush", [1.0, 2.0], [3.0, 4.0], [5.0, 6.0]);
        c.call("draw_look", vec![Value::resource(&art), Value::Int(128)]);
        let seen = log
            .borrow()
            .first()
            .cloned()
            .expect("the engine was asked to draw the artwork");
        for field in [
            "\"layers\"",
            "\"marks\"",
            "\"kind\":\"stroke\"",
            "\"colour\"",
        ] {
            assert!(
                seen.contains(field),
                "the engine reads {field} of the artwork: {seen}"
            );
        }
    }

    #[test]
    fn a_saved_artwork_opens_with_its_layers_its_paint_and_its_selection() {
        let c = Canvas::drawing();
        let file = FileValue::new("study.artwork", "application/vnd.tilo.artwork+zip", 3, "h3");
        let art = returned_ref(c.call(
            "open_artwork",
            vec![Value::resource(&c.root), Value::file(file)],
        ));
        assert_eq!(
            c.text(&art, "notice"),
            "",
            "the file opens without a complaint"
        );
        assert_eq!(c.text(&art, "title"), "A study");
        assert_eq!(c.float(&art, "width"), 800.0);
        assert_eq!(c.text(&art, "ground_texture"), "linen");
        assert_eq!(c.text(&art, "ground_colour"), "#ffffff");

        let ink = c.layer(&art, "Ink");
        assert_eq!(c.float(&ink, "opacity"), 0.5);
        assert_eq!(c.float(&ink, "texture"), 0.25);
        assert!(
            c.flag(&ink, "locked"),
            "a held-still layer opens held still"
        );

        let marks = c.marks(&art);
        assert_eq!(
            marks.len(),
            2,
            "the paint the file held came back: {marks:?}"
        );
        assert_eq!(c.text(&marks[0], "kind"), "stroke");
        assert_eq!(c.text(&marks[0], "brush"), "pen");
        assert_eq!(c.text(&marks[0], "colour"), "#d24b3a");
        assert_eq!(c.float(&marks[0], "opacity"), 0.8);
        match c.rt.read(&marks[0], "points").unwrap() {
            Value::List(points) => assert_eq!(points.len(), 6, "two points of x, y, pressure"),
            other => panic!("points, not {other:?}"),
        }
        assert_eq!(c.text(&marks[1], "kind"), "path");
        assert_eq!(c.text(&marks[1], "d"), "M 0 0 L 10 10 Z");
        assert_eq!(c.text(&marks[1], "colour"), "#2f7fb8");

        let selection = c.rt.read(&art, "selection").unwrap();
        assert!(
            format!("{selection:?}").contains("M 0 0 L 5 5 Z"),
            "the selection the file kept is the artwork's: {selection:?}"
        );
    }

    #[test]
    fn the_artwork_offers_the_window_its_tools_and_export() {
        let c = Canvas::new();
        let art = c.new_artwork("Tools");
        let mut view = c.window(&art);
        view.set_lazy_rows(false).unwrap();
        let tree = view.tree();
        let named = |kind: &str, label: &str| {
            of_kind(tree, kind)
                .into_iter()
                .find(|node| text_prop(node, "label") == label)
        };
        // The paint: a menu of every engine preset, its size and its colour.
        let brush = named("ui.MenuButton", "Brush: round")
            .unwrap_or_else(|| panic!("the window offers the brushes in\n{}", dump(tree)));
        let presets: Vec<String> = of_kind(brush, "ui.MenuItem")
            .into_iter()
            .map(|node| text_prop(node, "label"))
            .collect();
        for preset in ["round", "pen", "pencil", "marker", "eraser", "pixel"] {
            assert!(
                presets.iter().any(|held| held == preset),
                "the brushes offer {preset}: {presets:?}"
            );
        }
        assert!(named("ui.Slider", "Size").is_some(), "the brush's size");
        assert!(
            named("ui.ColorPicker", "Paint").is_some(),
            "the paint's colour"
        );
        // What the artwork leaves as.
        let export = named("ui.MenuButton", "Export")
            .unwrap_or_else(|| panic!("the window offers the artwork's export in\n{}", dump(tree)));
        let formats: Vec<String> = of_kind(export, "ui.MenuItem")
            .into_iter()
            .map(|node| text_prop(node, "label"))
            .collect();
        for format in [
            "PNG, full size",
            "JPEG, full size",
            "WebP, full size",
            "SVG",
        ] {
            assert!(
                formats.iter().any(|held| held == format),
                "the export offers {format}: {formats:?}"
            );
        }
        assert!(
            named("ui.Button", "Fit the view").is_some(),
            "the view resets to fit"
        );
        assert!(
            !of_kind(tree, "ui.PaintSurface").is_empty(),
            "the artwork is painted on a surface"
        );
    }
}
