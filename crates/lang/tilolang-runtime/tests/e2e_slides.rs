//! Slides (`apps/tl/slides/app.tl`) end to end: a deck begun with a title slide, slides added
//! from an outline, elements placed, moved, sized and restacked through its actions, the deck
//! read as text, and its window drawn on a stage of the slide's own size.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::{
        AmStore, EnumValue, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
        ViewInstance,
    };

    const SOURCE: &str = include_str!("../../../../apps/tl/slides/app.tl");

    struct Slides {
        rt: Runtime,
        app: App,
        deck: ResourceRef,
    }

    impl Slides {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let deck = rt.create_root("Deck").unwrap();
            Self { rt, app, deck }
        }

        fn id(&self, name: &str) -> String {
            self.app
                .compiled
                .ids
                .get(name)
                .unwrap_or_else(|| panic!("no id {name}"))
                .to_owned()
        }

        fn case(&self, ty: &str, variant: &str) -> Value {
            Value::Enum(EnumValue {
                ty: self.id(ty),
                variant: self.id(variant),
                fields: std::collections::BTreeMap::default(),
            })
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn call_with(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            let args = self.app.args(&self.rt, action, given);
            self.call(action, args)
        }

        fn slides(&self) -> Vec<ResourceRef> {
            refs(self.rt.read(&self.deck, "slides").unwrap())
        }

        fn slide(&self, at: usize) -> ResourceRef {
            let slides = self.slides();
            slides
                .get(at)
                .unwrap_or_else(|| panic!("no slide {at} of {}", slides.len()))
                .clone()
        }

        fn elements(&self, slide: &ResourceRef) -> Vec<ResourceRef> {
            refs(self.rt.read(slide, "elements").unwrap())
        }

        fn add_element(&self, slide: &ResourceRef, fields: &[(&str, Value)]) -> ResourceRef {
            let mut given: Vec<(&str, Value)> = vec![("s", Value::resource(slide))];
            given.extend(fields.iter().cloned());
            let args = self.app.args(&self.rt, "add_element", &given);
            returned_ref(self.call("add_element", args))
        }

        fn window(&self) -> ViewInstance {
            let mut view = self
                .rt
                .open_view(&me(), &self.app.view("deck"), vec![Value::resource(&self.deck)])
                .unwrap_or_else(|e| panic!("deck: {e}"));
            view.set_lazy_rows(false).unwrap();
            view
        }
    }

    /// The resources a collection read holds, in their order.
    fn refs(read: Value) -> Vec<ResourceRef> {
        match read {
            Value::List(items) => items
                .into_iter()
                .map(|item| match item {
                    Value::Ref(reference) => std::rc::Rc::unwrap_or_clone(reference.target),
                    other => panic!("an item read as {other:?}"),
                })
                .collect(),
            other => panic!("a collection read as {other:?}"),
        }
    }

    fn of_kind<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
        if node.kind == kind {
            out.push(node);
        }
        for child in &node.children {
            of_kind(child, kind, out);
        }
    }

    #[test]
    fn a_deck_starts_on_a_title_slide_and_grows_slide_by_slide() {
        let s = Slides::new();
        assert_eq!(s.slides().len(), 1, "a deck starts with one slide");
        let first = s.slide(0);
        assert_eq!(
            s.rt.read(&first, "layout").unwrap(),
            s.case("Layout", "Layout.title"),
            "the first slide is the title slide"
        );

        let second = returned_ref(s.call_with(
            "add_slide",
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("The plan")),
            ],
        ));
        let third = returned_ref(s.call_with(
            "add_slide",
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("Two columns")),
                ("body", Value::text("Left and right")),
                ("layout", s.case("Layout", "Layout.two_column")),
            ],
        ));
        assert_eq!(
            s.slides().len(),
            2,
            "the first slide anyone adds takes the empty starter's place"
        );
        assert_eq!(
            s.slides()[0].id_string(),
            second.id_string(),
            "a slide lands after the shown one"
        );
        assert_eq!(s.rt.read(&third, "layout").unwrap(), s.case("Layout", "Layout.two_column"));

        // The template a slide comes from carries its boxes, and the words go in the first two.
        let boxes = s.elements(&third);
        assert!(boxes.len() >= 2, "a two-column slide starts with its boxes");
        assert_eq!(
            s.rt.read(&third, "notes").unwrap(),
            Value::Rich(tilolang_runtime::rich::RichValue::default()),
            "a slide opens with no speaker notes"
        );

        let read = match s.call("read_deck", vec![Value::resource(&s.deck)]) {
            InvokeOutcome::Done(Value::Text(text)) => text,
            other => panic!("read_deck answered {other:?}"),
        };
        assert!(read.contains("(2 slides)"), "{read}");
        assert!(read.contains("The plan"), "{read}");
        assert!(read.contains("Two columns"), "{read}");

        // A copy of a slide carries its words, and it can be moved and deleted again.
        let copy = returned_ref(s.call(
            "duplicate_slide",
            vec![Value::resource(&s.deck), Value::resource(&second)],
        ));
        assert_eq!(s.slides().len(), 3);
        assert_eq!(s.slides()[1].id_string(), copy.id_string(), "a copy follows the slide");
        s.call(
            "move_slide",
            vec![
                Value::resource(&s.deck),
                Value::resource(&copy),
                Value::Int(0),
            ],
        );
        assert_eq!(s.slides()[0].id_string(), copy.id_string(), "a slide moves to the front");
        s.call(
            "delete_slide",
            vec![Value::resource(&s.deck), Value::resource(&copy)],
        );
        assert_eq!(s.slides().len(), 2, "a slide is deleted");

        // The template's first two boxes take a slide's title and body.
        s.call(
            "set_text",
            vec![
                Value::resource(&s.deck),
                Value::resource(&third),
                Value::text("Two columns"),
                Value::text("Left and right"),
            ],
        );
        let boxes = s.elements(&third);
        let first_words = s.rt.read(&boxes[0], "body").unwrap();
        assert!(
            matches!(&first_words, Value::Rich(rich) if rich.written_text().contains("Two columns")),
            "the title lands in the slide's first box: {first_words:?}"
        );
    }

    #[test]
    fn the_first_slide_someone_adds_takes_the_untouched_starters_place() {
        let s = Slides::new();
        let starter = s.slide(0);
        let made = returned_ref(s.call_with(
            "add_slide",
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("Kickoff")),
            ],
        ));
        assert_eq!(s.slides().len(), 1, "the empty starter gave way");
        assert_eq!(s.slides()[0].id_string(), made.id_string());
        assert_ne!(starter.id_string(), made.id_string());

        // A deck someone wrote on keeps its slides: the next one lands after them.
        let written = s.slides()[0].clone();
        s.call(
            "set_text",
            vec![
                Value::resource(&s.deck),
                Value::resource(&written),
                Value::text("Written"),
                Value::text(""),
            ],
        );
        let later = returned_ref(s.call_with(
            "add_slide",
            &[("d", Value::resource(&s.deck)), ("title", Value::text("Agenda"))],
        ));
        assert_eq!(s.slides().len(), 2, "the slide someone wrote on stays");
        assert_eq!(s.slides()[1].id_string(), later.id_string());
    }

    #[test]
    fn an_element_is_placed_moved_sized_styled_and_restacked() {
        let s = Slides::new();
        let slide = s.slide(0);
        let text = s.add_element(
            &slide,
            &[
                ("kind", s.case("ElementKind", "ElementKind.text")),
                ("x", Value::Int(96)),
                ("y", Value::Int(90)),
                ("w", Value::Int(600)),
                ("h", Value::Int(120)),
                ("font_size", Value::Opt(Some(Box::new(Value::Float(48.0))))),
            ],
        );
        let rect = s.add_element(
            &slide,
            &[
                ("kind", s.case("ElementKind", "ElementKind.rect")),
                ("x", Value::Int(100)),
                ("y", Value::Int(100)),
                ("w", Value::Int(200)),
                ("h", Value::Int(120)),
                ("fill", Value::Opt(Some(Box::new(Value::text("accent"))))),
                ("radius", Value::Opt(Some(Box::new(Value::Float(8.0))))),
            ],
        );
        assert_eq!(
            s.rt.read(&rect, "kind").unwrap(),
            s.case("ElementKind", "ElementKind.rect")
        );
        assert_eq!(s.rt.read(&rect, "fill").unwrap(), Value::text("accent"));

        // A drag reports where, and the element lands there.
        s.call(
            "move_element",
            vec![
                Value::resource(&slide),
                Value::resource(&rect),
                Value::Int(240),
                Value::Int(160),
            ],
        );
        assert_eq!(s.rt.read(&rect, "x").unwrap(), Value::Int(240));
        assert_eq!(s.rt.read(&rect, "y").unwrap(), Value::Int(160));

        // A drag of its corner reports the size it took, never smaller than it can be seen.
        s.call(
            "resize_element",
            vec![
                Value::resource(&slide),
                Value::resource(&rect),
                Value::List(vec![Value::Int(4), Value::Int(200)]),
            ],
        );
        assert_eq!(s.rt.read(&rect, "w").unwrap(), Value::Int(16), "a size floors at 16");
        assert_eq!(s.rt.read(&rect, "h").unwrap(), Value::Int(200));

        // The inspector's swatches write the element's own fields.
        let some = |value: Value| Value::Opt(Some(Box::new(value)));
        let args = s.app.args(
            &s.rt,
            "set_style",
            &[
                ("e", Value::resource(&rect)),
                ("stroke", some(Value::text("warning"))),
                ("radius", some(Value::Float(24.0))),
            ],
        );
        s.call("set_style", args);
        assert_eq!(s.rt.read(&rect, "stroke").unwrap(), Value::text("warning"));
        assert_eq!(s.rt.read(&rect, "radius").unwrap(), Value::Float(24.0));

        // The stage's move of what it names by key: the drag reports the id, not the value.
        s.call(
            "move_element_to",
            vec![
                Value::resource(&slide),
                Value::text(&rect.id_string()),
                Value::List(vec![Value::Int(320), Value::Int(64)]),
            ],
        );
        assert_eq!(s.rt.read(&rect, "x").unwrap(), Value::Int(320));

        // Arranging: the rect was drawn after the text, and the text comes to the front.
        let order = s.elements(&slide);
        assert_eq!(order.last().unwrap().id_string(), rect.id_string());
        s.call(
            "bring_to_front",
            vec![Value::resource(&slide), Value::resource(&text)],
        );
        let order = s.elements(&slide);
        assert_eq!(order.last().unwrap().id_string(), text.id_string(), "the text came to the front");
        s.call(
            "send_to_back",
            vec![Value::resource(&slide), Value::resource(&text)],
        );
        assert_eq!(s.elements(&slide)[0].id_string(), text.id_string(), "and back again");
    }

    #[test]
    fn an_element_is_duplicated_and_one_pasted_from_json_lands_whole() {
        let s = Slides::new();
        let slide = s.slide(0);
        let rect = s.add_element(
            &slide,
            &[
                ("kind", s.case("ElementKind", "ElementKind.rect")),
                ("x", Value::Int(100)),
                ("y", Value::Int(100)),
                ("w", Value::Int(200)),
                ("h", Value::Int(120)),
                ("fill", Value::Opt(Some(Box::new(Value::text("accent"))))),
            ],
        );
        let copy = returned_ref(s.call(
            "duplicate_element",
            vec![Value::resource(&slide), Value::resource(&rect)],
        ));
        assert_eq!(s.rt.read(&copy, "x").unwrap(), Value::Int(116), "the copy stands off by 16");
        assert_eq!(s.rt.read(&copy, "y").unwrap(), Value::Int(116));
        assert_eq!(s.rt.read(&copy, "w").unwrap(), Value::Int(200));
        assert_eq!(s.rt.read(&copy, "fill").unwrap(), Value::text("accent"));

        // A paste carries an element whole, as another deck's copy wrote it.
        let held = r#"{"id":"elsewhere","kind":"rect","x":32.0,"y":48.0,"w":220.0,"h":140.0,"text":"","src":"","fill":"success","stroke":"warning","stroke_width":4.0,"font_size":32.0,"align":"center","bold":false,"italic":false,"radius":12.0}"#;
        let before = s.elements(&slide).len();
        s.call(
            "insert_element",
            vec![Value::resource(&slide), Value::text(held)],
        );
        let after = s.elements(&slide);
        assert_eq!(after.len(), before + 1, "the paste lands an element");
        let pasted = after.last().unwrap();
        assert_eq!(s.rt.read(pasted, "x").unwrap(), Value::Int(32));
        assert_eq!(s.rt.read(pasted, "fill").unwrap(), Value::text("success"));
        assert_eq!(s.rt.read(pasted, "stroke").unwrap(), Value::text("warning"));
        assert_eq!(
            s.rt.read(pasted, "align").unwrap(),
            s.case("Align", "Align.center"),
            "the pasted words keep their alignment"
        );

        // Words that are no element land nothing.
        s.call(
            "insert_element",
            vec![Value::resource(&slide), Value::text("not an element")],
        );
        assert_eq!(s.elements(&slide).len(), before + 1, "a bad paste changes nothing");
    }

    /// The device's content store: a file's hash is its bytes, kept as a file's name says.
    #[derive(Clone, Default)]
    struct Store(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl BlobHost for Store {
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

    #[test]
    fn a_deck_survives_a_trip_through_a_presentation_file() {
        let s = Slides::new();
        s.rt.set_blobs(Store::default());
        s.call(
            "set_title",
            vec![Value::resource(&s.deck), Value::text("Round trip")],
        );
        let first = s.slide(0);
        s.call(
            "set_text",
            vec![
                Value::resource(&s.deck),
                Value::resource(&first),
                Value::text("Written in Tilo"),
                Value::text("and read back from a file"),
            ],
        );
        s.call_with(
            "add_slide",
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("Second")),
            ],
        );
        assert_eq!(s.slides().len(), 2);

        // The deck written as .pptx, and the deck replaced by what the file holds.
        let written = match s.call("export_pptx", vec![Value::resource(&s.deck)]) {
            InvokeOutcome::Done(Value::File(file)) => file,
            other => panic!("export_pptx answered {other:?}"),
        };
        s.call_with(
            "add_slide",
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("Thrown away")),
            ],
        );
        assert_eq!(s.slides().len(), 3);
        s.call(
            "adopt",
            vec![Value::resource(&s.deck), Value::File(written)],
        );
        assert_eq!(
            s.rt.read(&s.deck, "title").unwrap(),
            Value::text("Round trip"),
            "the deck's name comes back from the file"
        );
        assert_eq!(s.slides().len(), 2, "the file's slides replace the deck's");
        let words = s.rt.read(&s.slide(0), "elements").unwrap();
        let boxes = refs(words);
        let first_words = s.rt.read(&boxes[0], "body").unwrap();
        assert!(
            matches!(&first_words, Value::Rich(rich) if rich.written_text().contains("Written in Tilo")),
            "the first slide's title comes back: {first_words:?}"
        );
        assert!(
            matches!(&s.rt.read(&s.deck, "notice").unwrap(), Value::Text(notice) if notice.is_empty()),
            "a file that opened says nothing"
        );
    }

    #[test]
    fn the_window_stands_the_slide_on_a_stage_of_its_own_size() {
        let s = Slides::new();
        let slide = s.slide(0);
        let added = s.add_element(
            &slide,
            &[
                ("kind", s.case("ElementKind", "ElementKind.text")),
                ("x", Value::Int(100)),
                ("y", Value::Int(100)),
                ("w", Value::Int(500)),
                ("h", Value::Int(160)),
                ("font_size", Value::Opt(Some(Box::new(Value::Float(40.0))))),
            ],
        );
        let view = s.window();
        let mut stages = Vec::new();
        of_kind(view.tree(), "ui.Stage", &mut stages);
        let stage = stages
            .iter()
            .find(|stage| stage.props.get("name") == Some(&Value::text("Slide")))
            .expect("the editor shows the slide on a stage");
        assert_eq!(
            stage.props.get("canvas"),
            Some(&Value::List(vec![Value::Int(1280), Value::Int(720)])),
            "the stage is the slide's own size, so its words break as they will on the slide"
        );
        let mut pins = Vec::new();
        of_kind(stage, "ui.StagePin", &mut pins);
        assert_eq!(
            pins.len(),
            s.elements(&slide).len(),
            "every element of the slide stands on the stage"
        );
        let pin = pins
            .iter()
            .find(|pin| pin.props.get("item") == Some(&Value::text(&added.id_string())))
            .expect("the element just added stands on the stage");
        assert_eq!(
            pin.props.get("x"),
            Some(&Value::Int(100)),
            "a place stands at its own point on the page"
        );
        let mut rail = Vec::new();
        of_kind(view.tree(), "ui.Stage", &mut rail);
        assert!(
            rail.len() > 1,
            "the rail shows the slide as a picture of itself beside the stage"
        );
    }
    #[test]
    fn a_picture_the_workspace_holds_goes_onto_a_slide() {
        use tilolang_runtime::FileValue;

        let s = Slides::new();
        s.rt.set_blobs(Store::default());
        let slide = s.slide(0);
        let picture = FileValue::new("logo.png", "image/png", 4, "h4");
        let url = picture.url();
        let made = returned_ref(s.call_with(
            "add_picture",
            &[
                ("s", Value::resource(&slide)),
                ("file", Value::File(picture)),
            ],
        ));
        assert_eq!(
            s.rt.read(&made, "kind").unwrap(),
            s.case("ElementKind", "ElementKind.image"),
            "a picture is an image element"
        );
        assert_eq!(
            s.rt.read(&made, "src").unwrap(),
            Value::text(&url),
            "it stands by the address the content store gave it"
        );

        // The window draws it as a picture, wherever the slide shows.
        let view = s.window();
        let mut images = Vec::new();
        of_kind(view.tree(), "ui.Image", &mut images);
        assert!(
            !images.is_empty(),
            "the slide's picture reaches the stage: {}",
            dump(view.tree())
        );
    }

    #[test]
    fn lists_written_in_a_box_survive_the_round_trip() {
        let s = Slides::new();
        s.rt.set_blobs(Store::default());
        let first = s.slide(0);
        s.call(
            "set_text",
            vec![
                Value::resource(&s.deck),
                Value::resource(&first),
                Value::text("Packing"),
                Value::text("- socks\n- maps"),
            ],
        );
        let body = s.rt.read(&s.elements(&first)[1], "body").unwrap();
        let Value::Rich(written) = &body else {
            panic!("a body is rich text: {body:?}");
        };
        assert_eq!(
            written
                .blocks
                .iter()
                .filter(|block| block.kind == "list_item")
                .count(),
            2,
            "the words are two list items: {body:?}"
        );

        // Written as a presentation file and read back, the list is still a list.
        let written = match s.call("export_pptx", vec![Value::resource(&s.deck)]) {
            InvokeOutcome::Done(Value::File(file)) => file,
            other => panic!("export_pptx answered {other:?}"),
        };
        s.call(
            "adopt",
            vec![Value::resource(&s.deck), Value::File(written)],
        );
        let body = s.rt.read(&s.elements(&s.slide(0))[1], "body").unwrap();
        let Value::Rich(back) = &body else {
            panic!("a body is rich text: {body:?}");
        };
        assert_eq!(
            back.blocks
                .iter()
                .filter(|block| block.kind == "list_item")
                .count(),
            2,
            "both items come back as list items: {body:?}"
        );
        assert!(
            back.written_text().contains("socks") && back.written_text().contains("maps"),
            "and their words: {body:?}"
        );
    }

    #[test]
    fn the_presentation_clock_starts_holds_and_runs_on() {
        let s = Slides::new();
        s.call("begin_presenting", vec![Value::resource(&s.deck)]);
        assert!(
            matches!(
                s.rt.read(&s.deck, "presenting_since").unwrap(),
                Value::Opt(Some(_))
            ),
            "the clock starts when the presentation does"
        );
        assert_eq!(
            s.rt.read(&s.deck, "clock_paused").unwrap(),
            Value::Bool(false)
        );

        s.call("pause_clock", vec![Value::resource(&s.deck)]);
        assert_eq!(
            s.rt.read(&s.deck, "clock_paused").unwrap(),
            Value::Bool(true),
            "pausing holds the clock"
        );

        s.call("resume_clock", vec![Value::resource(&s.deck)]);
        assert_eq!(
            s.rt.read(&s.deck, "clock_paused").unwrap(),
            Value::Bool(false),
            "and running on releases it"
        );

        s.call("end_presenting", vec![Value::resource(&s.deck)]);
        assert!(
            matches!(
                s.rt.read(&s.deck, "presenting_since").unwrap(),
                Value::Opt(None)
            ),
            "the clock stops with the presentation"
        );
        assert_eq!(s.rt.read(&s.deck, "clock_held").unwrap(), Value::Int(0));
    }

    #[test]
    fn the_notes_editor_binds_the_slide_that_shows() {
        use tilolang_runtime::{BindTarget, Binding};

        let s = Slides::new();
        let shown = s.slide(0);
        let args = s.app.args(
            &s.rt,
            "set_notes",
            &[
                ("s", Value::resource(&shown)),
                (
                    "notes",
                    Value::Rich(tilolang_runtime::rich::RichValue::from_text("Say hello")),
                ),
            ],
        );
        s.call("set_notes", args);

        let view = s.window();
        let mut editors = Vec::new();
        of_kind(view.tree(), "ui.BlockEditor", &mut editors);
        let notes = editors.iter().find(|node| {
            matches!(
                &node.binding,
                Some(Binding {
                    target: BindTarget::Field { target, .. },
                    ..
                }) if target.id_string() == shown.id_string()
            )
        });
        assert!(
            notes.is_some(),
            "the notes editor edits the slide the window shows: {}",
            dump(view.tree())
        );
    }

    #[test]
    fn a_new_text_box_is_the_one_that_takes_the_keyboard() {
        let s = Slides::new();
        let mut view = s.window();
        let insert = s.app.anchor("deck", "insert_text");
        assert!(matches!(view.press(&insert).unwrap(), InvokeOutcome::Done(_)));
        view.poll_patches().unwrap();

        let elements = s.elements(&s.slide(0));
        let made = elements.last().expect("the press made a box");
        assert_eq!(
            view.view_state("chosen"),
            Some(Value::text(&made.id_string())),
            "the box the press made is the chosen one"
        );
        assert_eq!(
            view.view_state("editing"),
            Some(Value::Bool(true)),
            "and it is open for typing"
        );
    }

}
