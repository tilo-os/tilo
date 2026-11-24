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
        AmStore, EnumValue, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Space, UiNode,
        Value, ViewInstance,
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
                .open_view(
                    &me(),
                    &self.app.view("deck"),
                    vec![Value::resource(&self.deck)],
                )
                .unwrap_or_else(|e| panic!("deck: {e}"));
            view.set_lazy_rows(false).unwrap();
            view
        }

        /// Why the app refused `action` with `given`, as its `require` says it.
        fn refused(&self, action: &str, given: &[(&str, Value)]) -> String {
            let args = self.app.args(&self.rt, action, given);
            let id = self.app.action(action);
            match self.rt.invoke_action(&me(), (id.as_str(), args)) {
                Ok(other) => panic!("{action} was expected to be refused, and answered {other:?}"),
                Err(error) => format!("{error:?}"),
            }
        }

        /// Every change the deck's document holds, oldest first.
        fn changes(&self) -> Vec<tilolang_runtime::ChangeNote> {
            self.rt.history(&self.deck.doc).unwrap()
        }
    }

    /// Whether a prop or a state holds yes, as the view gave it: the language compiles `true` to
    /// the whole number one, and a state's slot boxes a scalar.
    fn is_true(value: Option<&Value>) -> bool {
        match value {
            Some(Value::Bool(on)) => *on,
            Some(Value::Int(on)) => *on != 0,
            Some(Value::Opt(Some(inner))) => is_true(Some(inner)),
            _ => false,
        }
    }

    /// A view state holding yes or no, as the view's slot keeps it: the slot boxes a scalar, and
    /// a `Bool` the view wrote arrives as the whole number the language compiles it to.
    fn state_flag(view: &ViewInstance, name: &str) -> Option<bool> {
        match view.view_state(name)? {
            Value::Opt(Some(inner)) => match *inner {
                Value::Bool(on) => Some(on),
                Value::Int(on) => Some(on != 0),
                _ => None,
            },
            Value::Bool(on) => Some(on),
            _ => None,
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

    /// The deck's window shell, as the view builds it.
    fn nav_shell(view: &ViewInstance) -> &UiNode {
        let mut shells = Vec::new();
        of_kind(view.tree(), "ui.NavShell", &mut shells);
        shells
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("the deck builds its window's shell"))
    }

    /// The button the deck labels `label`.
    fn button_named<'a>(view: &'a ViewInstance, label: &str) -> &'a UiNode {
        let mut buttons = Vec::new();
        of_kind(view.tree(), "ui.Button", &mut buttons);
        buttons
            .into_iter()
            .find(|node| {
                matches!(node.props.get("label"), Some(Value::Text(text)) if text == label)
            })
            .unwrap_or_else(|| panic!("the deck offers a `{label}` button"))
    }

    /// Whether the shell prop `name` asks yes.
    fn prop_flag(node: &UiNode, name: &str) -> Option<bool> {
        node.props.get(name).map(|value| is_true(Some(value)))
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
        assert_eq!(
            s.rt.read(&third, "layout").unwrap(),
            s.case("Layout", "Layout.two_column")
        );

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
        assert_eq!(
            s.slides()[1].id_string(),
            copy.id_string(),
            "a copy follows the slide"
        );
        s.call(
            "move_slide",
            vec![
                Value::resource(&s.deck),
                Value::resource(&copy),
                Value::Int(0),
            ],
        );
        assert_eq!(
            s.slides()[0].id_string(),
            copy.id_string(),
            "a slide moves to the front"
        );
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
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("Agenda")),
            ],
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
        assert_eq!(
            s.rt.read(&rect, "w").unwrap(),
            Value::Int(16),
            "a size floors at 16"
        );
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
        assert_eq!(
            order.last().unwrap().id_string(),
            text.id_string(),
            "the text came to the front"
        );
        s.call(
            "send_to_back",
            vec![Value::resource(&slide), Value::resource(&text)],
        );
        assert_eq!(
            s.elements(&slide)[0].id_string(),
            text.id_string(),
            "and back again"
        );
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
        assert_eq!(
            s.rt.read(&copy, "x").unwrap(),
            Value::Int(116),
            "the copy stands off by 16"
        );
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
        assert_eq!(
            s.elements(&slide).len(),
            before + 1,
            "a bad paste changes nothing"
        );
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
        s.call_with(
            "add_picture",
            &[
                ("s", Value::resource(&slide)),
                ("file", Value::File(std::rc::Rc::new(picture))),
            ],
        );
        let made = s.elements(&slide).pop().expect("the picture landed");
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
        // The insert tool the deck's window draws: the deck calls its tools row inside the
        // shell's content part, and the anchor walk reaches it through the views the deck
        // calls, templates included.
        let insert = s.app.anchor("deck", "insert_text");
        assert!(matches!(
            view.press(&insert).unwrap(),
            InvokeOutcome::Done(_)
        ));
        view.poll_patches().unwrap();

        let elements = s.elements(&s.slide(0));
        let made = elements.last().expect("the press made a box");
        assert_eq!(
            view.view_state("chosen"),
            Some(Value::text(&made.id_string())),
            "the box the press made is the chosen one"
        );
        assert_eq!(
            state_flag(&view, "editing"),
            Some(true),
            "and it is open for typing"
        );

        // The box it made is the editor the keyboard lands in: the editor the stage places on
        // the slide, bound to that box's words, asks for the keyboard as it is built.
        use tilolang_runtime::{BindTarget, Binding};
        let mut editors = Vec::new();
        of_kind(view.tree(), "ui.BlockEditor", &mut editors);
        let editor = editors
            .iter()
            .find(|node| {
                matches!(
                    &node.binding,
                    Some(Binding {
                        target: BindTarget::Field { target, .. },
                        ..
                    }) if target.id_string() == made.id_string()
                )
            })
            .unwrap_or_else(|| panic!("the made box's editor: {}", dump(view.tree())));
        assert!(
            is_true(editor.props.get("focus")),
            "the box the press made takes the keyboard: {:?}",
            editor.props.get("focus")
        );
    }
    #[test]
    fn an_empty_box_opens_on_the_click_that_chooses_it() {
        use tilolang_runtime::{BindTarget, Binding};

        let s = Slides::new();
        let slide = s.slide(0);
        // A box already written on: the first click chooses it, and the next one aims at a word.
        let title = s.elements(&slide)[0].clone();
        s.call(
            "set_text",
            vec![
                Value::resource(&s.deck),
                Value::resource(&slide),
                Value::text("A written title"),
                Value::text(""),
            ],
        );
        // An empty one, which a person means to write in: it takes the click that chooses it.
        let empty = s.add_element(
            &slide,
            &[
                ("kind", s.case("ElementKind", "ElementKind.text")),
                ("x", Value::Int(120)),
                ("y", Value::Int(420)),
            ],
        );
        let mut view = s.window();
        let stage = {
            let mut stages = Vec::new();
            of_kind(view.tree(), "ui.Stage", &mut stages);
            stages
                .iter()
                .find(|node| node.props.get("name") == Some(&Value::text("Slide")))
                .unwrap_or_else(|| panic!("the slide's stage: {}", dump(view.tree())))
                .key
                .clone()
        };
        // The stage's select carries the place's item and the page point it was pressed at,
        // as the parts the event declares.
        let chosen_at = Value::List(vec![
            Value::text(&empty.id_string()),
            Value::List(vec![Value::Int(120), Value::Int(420)]),
        ]);
        view.fire_with(&stage, "select", Some(&chosen_at)).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            state_flag(&view, "editing"),
            Some(true),
            "the click that chose the empty box opened it for typing"
        );

        // The box's editor, the one the stage places on the slide, asks for the keyboard.
        let mut editors = Vec::new();
        of_kind(view.tree(), "ui.BlockEditor", &mut editors);
        let editor = editors
            .iter()
            .find(|node| {
                matches!(
                    &node.binding,
                    Some(Binding {
                        target: BindTarget::Field { target, .. },
                        ..
                    }) if target.id_string() == empty.id_string()
                )
            })
            .unwrap_or_else(|| panic!("the empty box's editor: {}", dump(view.tree())));
        assert!(
            is_true(editor.props.get("focus")),
            "the empty box takes the keyboard on the same click: {:?}",
            editor.props.get("focus")
        );

        // A box with words is only chosen by the first click: the person aims at a word with
        // the next one, or with Enter.
        let chosen_at = Value::List(vec![
            Value::text(&title.id_string()),
            Value::List(vec![Value::Int(96), Value::Int(110)]),
        ]);
        view.fire_with(&stage, "select", Some(&chosen_at)).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            state_flag(&view, "editing"),
            Some(false),
            "a box that has words waits for the click after the one that chose it"
        );
    }

    #[test]
    fn add_element_refuses_what_it_cannot_make() {
        let s = Slides::new();
        let slide = s.slide(0);
        let before = s.elements(&slide).len();
        let some = |value: Value| Value::Opt(Some(Box::new(value)));
        let text = s.case("ElementKind", "ElementKind.text");
        let image = s.case("ElementKind", "ElementKind.image");

        let why = s.refused(
            "add_element",
            &[("s", Value::resource(&slide)), ("kind", image.clone())],
        );
        assert!(why.contains("A picture needs its source"), "{why}");

        let why = s.refused(
            "add_element",
            &[
                ("s", Value::resource(&slide)),
                ("kind", image),
                (
                    "text",
                    some(Value::Rich(tilolang_runtime::rich::RichValue::from_text(
                        "words",
                    ))),
                ),
            ],
        );
        assert!(why.contains("Only text takes words"), "{why}");

        let why = s.refused(
            "add_element",
            &[
                ("s", Value::resource(&slide)),
                ("kind", text),
                ("src", some(Value::text("tilo-blob://abc"))),
            ],
        );
        assert!(why.contains("Only a picture takes a source"), "{why}");

        assert_eq!(
            s.elements(&slide).len(),
            before,
            "a refused add makes nothing"
        );
    }

    #[test]
    fn deleting_an_element_is_one_change_and_lands_at_once() {
        let s = Slides::new();
        let slide = s.slide(0);
        let held = s.elements(&slide).len();
        let made = s.add_element(
            &slide,
            &[("kind", s.case("ElementKind", "ElementKind.rect"))],
        );
        assert_eq!(s.elements(&slide).len(), held + 1);
        let before = s.changes();

        s.call(
            "delete_element",
            vec![Value::resource(&slide), Value::resource(&made)],
        );
        assert_eq!(s.elements(&slide).len(), held, "the deletion lands at once");

        // Every change the deletion made is one invocation: one step for a person to undo.
        let made = &s.changes()[before.len()..];
        assert!(!made.is_empty(), "the deletion changed the deck");
        let invocation = made[0].invocation.clone();
        assert!(invocation.is_some(), "the deletion is a run of its own");
        assert!(
            made.iter().all(|note| note.invocation == invocation),
            "the deletion is one step: {made:?}"
        );
    }

    #[test]
    fn the_last_slide_cannot_be_deleted() {
        let s = Slides::new();
        let only = s.slide(0);
        let why = s.refused(
            "delete_slide",
            &[
                ("d", Value::resource(&s.deck)),
                ("s", Value::resource(&only)),
            ],
        );
        assert!(why.contains("A deck keeps at least one slide"), "{why}");
        assert_eq!(s.slides().len(), 1, "a deck keeps its one slide");
    }

    #[test]
    fn a_script_renames_the_deck_through_the_title_edit_the_bar_uses() {
        let s = Slides::new();
        let before = s.changes();
        let args = s.app.args(
            &s.rt,
            "set_title",
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("Q3 review")),
            ],
        );
        s.call("set_title", args);
        assert_eq!(
            s.rt.read(&s.deck, "title").unwrap(),
            Value::text("Q3 review")
        );

        let made = &s.changes()[before.len()..];
        assert_eq!(made.len(), 1, "a rename is one change: {made:?}");
        assert_eq!(
            made[0].action,
            s.app.action("set_title"),
            "the change is the action a script and the bar alike call"
        );

        let view = s.window();
        let mut shells = Vec::new();
        of_kind(view.tree(), "ui.NavShell", &mut shells);
        assert!(
            shells
                .iter()
                .any(|node| node.props.get("title") == Some(&Value::text("Q3 review"))),
            "the bar names the deck by its new title: {}",
            dump(view.tree())
        );
    }

    #[test]
    fn every_change_reads_as_a_sentence_to_an_agent() {
        let s = Slides::new();
        let did = |name: &str| {
            s.app
                .meta()
                .actions
                .iter()
                .find(|action| action.name == name)
                .unwrap_or_else(|| panic!("no action {name}"))
                .did
                .clone()
        };
        for (name, sentence) in [
            ("add_slide", "added a slide"),
            ("add_slides", "added slides from an outline"),
            ("duplicate_slide", "duplicated a slide"),
            ("delete_slide", "deleted a slide"),
            ("move_slide", "moved a slide"),
            ("set_text", "set the slide's words"),
            ("add_element", "added an object"),
            ("insert_element", "pasted an object"),
            ("delete_element", "deleted an object"),
            ("bring_to_front", "brought an object to the front"),
            ("send_to_back", "sent an object to the back"),
            ("move_element", "moved an object"),
            ("resize_element", "resized an object"),
            ("set_style", "restyled an object"),
            ("set_layout", "changed the slide's layout"),
            ("set_background", "changed the slide's background"),
            ("set_palette", "changed the deck's theme"),
            ("set_look", "changed the deck's look"),
            ("set_title", "renamed the deck"),
            ("set_notes", "wrote the speaker notes"),
            ("add_picture", "inserted a picture"),
            ("open_presentation", "opened a presentation"),
            ("adopt", "opened a presentation"),
            ("begin_presenting", "started the presentation"),
        ] {
            assert_eq!(
                did(name).as_deref(),
                Some(sentence),
                "history reads `{name}` as a sentence an agent and a person both read"
            );
        }
        // A read names no change: it has nothing to say.
        assert_eq!(did("read_deck"), None, "a read declares no change");
    }

    #[test]
    fn the_help_sheet_is_a_dialog_that_answers_its_own_dismiss() {
        let s = Slides::new();
        let mut view = s.window();
        view.set_view_state("help", Value::Bool(true));
        view.poll_patches().unwrap();

        let mut dialogs = Vec::new();
        of_kind(view.tree(), "ui.ModalDialog", &mut dialogs);
        let dialog = *dialogs
            .first()
            .unwrap_or_else(|| panic!("the keyboard reference: {}", dump(view.tree())));
        assert!(
            matches!(
                dialog.props.get("open"),
                Some(Value::Bool(true)) | Some(Value::Int(1))
            ),
            "the reference is open over the editor: {:?}",
            dialog.props
        );
        assert!(
            dialog.events.contains_key("dismiss"),
            "the dialog takes the Escape the reference lists: {:?}",
            dialog.events.keys()
        );
        view.fire(&dialog.key, "dismiss").unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            state_flag(&view, "help"),
            Some(false),
            "dismissing the dialog closes the reference"
        );
    }

    #[test]
    fn a_compact_window_shows_the_speakers_notes_under_the_slide() {
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
        let notes_editors = |view: &ViewInstance| {
            let mut editors = Vec::new();
            of_kind(view.tree(), "ui.BlockEditor", &mut editors);
            editors
                .iter()
                .filter(|node| {
                    matches!(
                        &node.binding,
                        Some(Binding {
                            target: BindTarget::Field { target, .. },
                            ..
                        }) if target.id_string() == shown.id_string()
                    )
                })
                .count()
        };

        let mut view = s.window();
        assert_eq!(
            notes_editors(&view),
            1,
            "a wide window keeps the notes in the inspector alone"
        );
        // The compact width is the shell's own: just under its boundary the notes stand under
        // the slide too, and just over it they are the inspector's alone.
        assert!(
            view.set_space("", Space::from_rem(29.0, 40.0)),
            "the window narrowed to under the shell's compact width"
        );
        view.poll_patches().unwrap();
        assert_eq!(
            notes_editors(&view),
            2,
            "under the shell's compact width the notes stand under the slide"
        );
        assert!(
            view.set_space("", Space::from_rem(31.0, 40.0)),
            "the window widened past it"
        );
        view.poll_patches().unwrap();
        assert_eq!(
            notes_editors(&view),
            1,
            "over it the inspector's notes serve alone"
        );
    }

    #[test]
    fn the_slide_list_toggle_opens_and_closes_the_rail() {
        let s = Slides::new();
        let mut view = s.window();
        let shell = nav_shell(&view);
        assert_eq!(
            shell.props.get("sidebar_collapse"),
            Some(&Value::text("drawer")),
            "a narrow window lays the rail over the slide as a drawer"
        );
        assert_eq!(
            prop_flag(shell, "sidebar_open"),
            Some(true),
            "the rail is open as the window opens"
        );

        let toggle = button_named(&view, "Slide list").key.clone();
        view.press(&toggle).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            state_flag(&view, "rail_open"),
            Some(false),
            "the button hides the rail"
        );
        assert_eq!(
            prop_flag(nav_shell(&view), "sidebar_open"),
            Some(false),
            "the shell closes the rail it was asked to"
        );

        view.press(&toggle).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            state_flag(&view, "rail_open"),
            Some(true),
            "and shows it again"
        );
        assert_eq!(
            prop_flag(nav_shell(&view), "sidebar_open"),
            Some(true),
            "the shell shows the rail again"
        );
    }

    #[test]
    fn the_properties_toggle_opens_and_closes_the_inspector_drawer() {
        let s = Slides::new();
        let mut view = s.window();
        let shell = nav_shell(&view);
        assert_eq!(
            shell.props.get("inspector_collapse"),
            Some(&Value::text("drawer")),
            "a narrow window lays the properties over the slide, from its trailing edge"
        );
        assert_eq!(
            prop_flag(shell, "inspector_open"),
            Some(true),
            "the properties are open as the window opens"
        );

        let toggle = button_named(&view, "Properties").key.clone();
        view.press(&toggle).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            state_flag(&view, "properties"),
            Some(false),
            "the button hides the properties"
        );
        assert_eq!(
            prop_flag(nav_shell(&view), "inspector_open"),
            Some(false),
            "the shell closes the drawer it was asked to"
        );

        view.press(&toggle).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            state_flag(&view, "properties"),
            Some(true),
            "and shows them again"
        );
        assert_eq!(
            prop_flag(nav_shell(&view), "inspector_open"),
            Some(true),
            "the shell opens the drawer again"
        );
    }

    /// The slide's notes as the window binds them: the editor the inspector (and a compact
    /// window's content) places for the notes of `slide`.
    fn notes_binders(view: &ViewInstance, slide: &ResourceRef) -> usize {
        use tilolang_runtime::{BindTarget, Binding};

        let mut editors = Vec::new();
        of_kind(view.tree(), "ui.BlockEditor", &mut editors);
        editors
            .iter()
            .filter(|node| {
                matches!(
                    &node.binding,
                    Some(Binding {
                        target: BindTarget::Field { target, .. },
                        ..
                    }) if target.id_string() == slide.id_string()
                )
            })
            .count()
    }

    #[test]
    fn the_notes_follow_the_slide_the_deck_shows_and_never_a_deleted_one() {
        let s = Slides::new();
        // Two slides someone made: the deck's emptied starter gives way to the first, so a ref
        // kept to it names a slide that is no longer in the deck.
        let first = returned_ref(s.call_with(
            "add_slide",
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("First")),
            ],
        ));
        let second = returned_ref(s.call_with(
            "add_slide",
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("Second")),
            ],
        ));
        let write = |slide: &ResourceRef, words: &str| {
            let args = s.app.args(
                &s.rt,
                "set_notes",
                &[
                    ("s", Value::resource(slide)),
                    (
                        "notes",
                        Value::Rich(tilolang_runtime::rich::RichValue::from_text(words)),
                    ),
                ],
            );
            s.call("set_notes", args);
        };
        write(&first, "the first slide's words");
        write(&second, "the second slide's words");

        let mut view = s.window();
        view.set_view_state("current", Value::text(&second.id_string()));
        view.poll_patches().unwrap();
        assert_eq!(
            notes_binders(&view, &second),
            1,
            "the notes editor binds the slide the deck shows: {}",
            dump(view.tree())
        );
        assert_eq!(notes_binders(&view, &first), 0);

        // The shown slide goes, as the rail's Delete takes it: the deck lands on the slide it
        // still holds, and its notes are the ones the editor binds. Nothing edits the gone
        // slide's notes any more.
        s.call(
            "delete_slide",
            vec![Value::resource(&s.deck), Value::resource(&second)],
        );
        view.poll_patches().unwrap();
        assert_eq!(
            notes_binders(&view, &second),
            0,
            "the notes of a deleted slide close: {}",
            dump(view.tree())
        );
        assert_eq!(
            notes_binders(&view, &first),
            1,
            "the notes editor follows the slide that is shown now"
        );
    }

    #[test]
    fn a_slide_change_rebinds_the_notes_and_the_other_slides_words_stay() {
        let s = Slides::new();
        // Two slides someone made: the first add replaces the deck's emptied starter.
        let first = returned_ref(s.call_with(
            "add_slide",
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("First")),
            ],
        ));
        let second = returned_ref(s.call_with(
            "add_slide",
            &[
                ("d", Value::resource(&s.deck)),
                ("title", Value::text("Second")),
            ],
        ));
        let mut view = s.window();
        assert_eq!(notes_binders(&view, &first), 1);
        assert_eq!(notes_binders(&view, &second), 0);

        view.set_view_state("current", Value::text(&second.id_string()));
        view.poll_patches().unwrap();
        assert_eq!(
            notes_binders(&view, &second),
            1,
            "a slide change moves the notes editor to the new slide"
        );
        assert_eq!(notes_binders(&view, &first), 0);

        // Writing the notes the window shows lands on the slide it shows, and the slide that
        // was left keeps its words.
        let args = s.app.args(
            &s.rt,
            "set_notes",
            &[
                ("s", Value::resource(&second)),
                (
                    "notes",
                    Value::Rich(tilolang_runtime::rich::RichValue::from_text("new words")),
                ),
            ],
        );
        s.call("set_notes", args);
        let read = |slide: &ResourceRef| match s.rt.read(slide, "notes").unwrap() {
            Value::Rich(rich) => rich.written_text(),
            other => panic!("notes read as {other:?}"),
        };
        assert_eq!(read(&second), "new words");
        assert_eq!(read(&first), "", "the slide left behind keeps its words");
    }
}
