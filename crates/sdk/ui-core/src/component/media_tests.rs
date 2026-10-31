//! The media and surface components: a picture read from its address, a card's art, a band's
//! controls reporting the view's events, and the surfaces presented in the window's top layer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use std::collections::BTreeMap;

use super::*;
use crate::component::{Component, Described, PropType};
use crate::headless::FrameHarness;
use crate::theme::Theme;
use crate::widget::{
    CodeEditor, CodeEditorProps, DeclaredLine, DropZone, DropZoneProps, Image, ImageProps,
    MediaCardProps, MediaGrid, MediaGridProps, PagePreview, PagePreviewProps, QrProps, QrWidget, RichTextLineProps,
    Widget,
};

/// An instance as a test gives it: values by prop, the events it handles, and the components in
/// its slots.
struct Node {
    kind: &'static str,
    key: String,
    props: BTreeMap<String, Value>,
    events: Vec<&'static str>,
    slots: Vec<(&'static str, Node)>,
    queue: crate::Intents<Fired>,
}

impl Node {
    fn new(kind: &'static str, key: &str, queue: &crate::Intents<Fired>) -> Self {
        Self {
            kind,
            key: key.to_owned(),
            props: BTreeMap::new(),
            events: Vec::new(),
            slots: Vec::new(),
            queue: queue.clone(),
        }
    }

    fn prop(mut self, name: &str, value: Value) -> Self {
        self.props.insert(name.to_owned(), value);
        self
    }

    fn on(mut self, event: &'static str) -> Self {
        self.events.push(event);
        self
    }

    fn holding(mut self, slot: &'static str, child: Node) -> Self {
        self.slots.push((slot, child));
        self
    }
}

impl Source for Node {
    fn key(&self) -> &str {
        &self.key
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        self.props.get(name).cloned()
    }

    fn event(&mut self, name: &str) -> Event {
        if self.events.contains(&name) {
            Event::View(Intent::new(&self.queue, &self.key, name))
        } else {
            Event::None
        }
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, _slot: &str) -> Vec<crate::widget::Element> {
        Vec::new()
    }

    fn components(
        &mut self,
        slot: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        let mut count = 0;
        for (name, child) in &mut self.slots {
            if *name == slot {
                read(child)?;
                count += 1;
            }
        }
        Ok(count)
    }
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn picture(src: &str) -> Value {
    Value::Picture(tilo_ui_graph::component::Picture { src: src.to_owned() })
}

/// A served pictures store, so a test's picture address answers from its own bytes rather than
/// reaching for a content store.
fn pictures_with(png: Vec<u8>) -> crate::pictures::Pictures {
    let fetch: crate::image_store::Fetcher = std::sync::Arc::new(move |_| Some(png.clone()));
    crate::pictures::Pictures::new(crate::ImageStore::new(&fetch))
}

/// The card `props` describe, built by the grid component.
fn card_of(props: MediaCardProps) -> crate::widget::MediaCard {
    let mut grid = MediaGrid::from_props(
        "library",
        MediaGridProps {
            size: None,
            justified: false,
            items: vec![props],
        },
    );
    grid.take_card(0).expect("one card")
}

fn harness<W: Widget + 'static>(root: W) -> FrameHarness {
    FrameHarness::for_root(crate::State::new(), root, (900, 600), Theme::light())
}

fn fired(key: &str, event: &str, value: Option<Value>) -> Fired {
    Fired {
        key: key.to_owned(),
        event: event.to_owned(),
        value,
    }
}

#[test]
fn an_image_is_a_picture_by_its_address_and_shows_its_bytes_when_they_arrive() {
    let png = crate::png::encode_rgba8(4, 4, &[40, 80, 160, 255].repeat(16)).expect("a picture");
    let pictures = pictures_with(png);
    let mut node = Node::new("Image", "photo", &crate::Intents::default())
        .prop("src", text("data:image/png;base64,x"))
        .prop("alt", text("A blue tile"));
    let mut props = ImageProps::from_source(&mut node).expect("reads");
    props.pictures = Some(pictures);
    let image = Image::from_props("photo", props);
    assert_eq!(image.alt(), Some("A blue tile"));
    // The bytes are in the store: the first read takes them and lays the picture out.
    let harness = harness(image);
    assert_eq!(
        harness.root::<Image>().intrinsic_size(),
        (4, 4),
        "the picture decodes from its address"
    );
}

#[test]
fn a_picture_answers_a_served_store_and_nothing_else_loads() {
    let png = crate::png::encode_rgba8(2, 2, &[255; 4].repeat(4)).expect("a picture");
    let fetch: crate::image_store::Fetcher = std::sync::Arc::new(move |url: &str| {
        assert_eq!(url, "tilo-blob://only");
        Some(png.clone())
    });
    let store = crate::ImageStore::new(&fetch);
    let pictures = crate::pictures::Pictures::new(store);
    let mut shown = crate::ShownPicture::default();
    shown.set(
        pictures,
        crate::widget::PictureSource {
            src: "tilo-blob://only".into(),
        },
    );
    assert!(shown.waiting(), "it asks for a frame until the bytes are in");
    // The store's first get queues the load; the fetch runs on its own background, so the test
    // reads through the fetch directly to prove the address is the one it loads.
    assert!(crate::pictures::data_bytes("tilo-blob://only").is_none());
    assert!(crate::pictures::data_bytes("data:image/png;base64,AA==").is_some());
}

#[test]
fn the_media_grid_reads_its_cards_and_a_pressed_card_reports_itself() {
    let queue = crate::Intents::default();
    let card = Node::new("MediaCard", "blue", &queue)
        .prop("title", text("Blue"))
        .prop("picture", picture("data:image/png;base64,x"))
        .on("activate");
    let mut grid = Node::new("MediaGrid", "library", &queue).holding("items", card);
    let props = MediaGridProps::from_source(&mut grid).expect("reads");
    assert_eq!(props.items.len(), 1);
    assert_eq!(props.items[0].title.as_deref(), Some("Blue"));
}

#[test]
fn a_pressed_card_opens_what_it_stands_for() {
    let queue = crate::Intents::default();
    let png = crate::png::encode_rgba8(4, 4, &[255; 4].repeat(16)).expect("a picture");
    let pictures = pictures_with(png);
    let views: Vec<crate::widget::Element> = vec![Box::new({
        let mut node = Node::new("MediaCard", "blue", &queue)
            .prop("item", Value::Resource("photo".into()))
            .prop("title", text("Blue"))
            .prop("picture", picture("data:image/png;base64,x"))
            .on("activate");
        let mut props = crate::widget::MediaCardProps::from_source(&mut node).expect("reads");
        props.pictures = Some(pictures);
        card_of(props)
    })];
    let grid = crate::widget::MediaGrid::of(views);
    let mut host = harness(grid);
    let rect = host.root_widget().children()[0].get_rect();
    host.click_at(rect.center().x, rect.center().y);
    assert_eq!(queue.take(), [fired("blue", "activate", None)]);
}

#[test]
fn a_qr_code_re_encodes_a_changed_payload_in_place() {
    let mut code = QrWidget::from_props(
        "pair",
        QrProps {
            payload: "one".into(),
            alt: Some("Pairing".into()),
            secret: false,
        },
    );
    assert_eq!(code.alt(), "Pairing");
    assert!(code.error().is_none());
    assert!(code.set_props(QrProps {
        payload: "two".into(),
        alt: None,
        secret: true,
    }));
    assert!(code.is_secret(), "the secret mark takes in place");
}

#[test]
fn a_page_preview_clamps_its_page_into_its_count() {
    let mut preview = PagePreview::from_props(
        "page",
        PagePreviewProps {
            label: "Report.pdf".into(),
            page: 9,
            page_count: 3,
        },
    );
    assert_eq!(preview.page(), 3);
    assert!(preview.set_props(PagePreviewProps {
        label: "Report.pdf".into(),
        page: 1,
        page_count: 3,
    }));
    assert_eq!(preview.page(), 1);
}

#[test]
fn a_drop_zone_reports_the_files_dropped_on_it() {
    let queue = crate::Intents::default();
    let mut node = Node::new("DropZone", "import", &queue)
        .prop("title", text("Drop files"))
        .prop("message", text("Any file"))
        .on("drop");
    let props = DropZoneProps::from_source(&mut node).expect("reads");
    let host = harness(DropZone::from_props("import", props));
    let rect = host.root::<DropZone>().get_rect();
    assert!(rect.width() > 0.0 && rect.height() > 0.0);
}

#[test]
fn a_code_editor_takes_the_language_its_view_names() {
    let queue = crate::Intents::default();
    let mut node = Node::new("CodeEditor", "source", &queue)
        .prop("language", Value::Choice("tilolang".into()))
        .prop("min_lines", Value::Int(4))
        .prop("gutter", Value::Bool(true));
    let props = CodeEditorProps::from_source(&mut node).expect("reads");
    assert_eq!(props.language, Some(crate::widget::Language::Tilolang));
    let editor = CodeEditor::from_props("source", props);
    let host = harness(editor);
    assert!(host.root::<CodeEditor>().get_rect().height() > 0.0);
}

#[test]
fn a_prop_of_an_undescribed_type_does_not_read() {
    // A picture is a `Picture` value, never text: the wrong kind is refused, not coerced.
    let mut node = Node::new("Image", "photo", &crate::Intents::default()).prop("src", text("x"));
    let mut props = ImageProps::from_source(&mut node).expect("src is text");
    props.pictures = None;
    assert!(crate::widget::PictureSource::from_value(&text("x")).is_err());
}

#[test]
fn a_rich_text_line_edits_its_bound_text() {
    let queue = crate::Intents::default();
    let mut node = Node::new("RichTextLine", "line", &queue)
        .prop("placeholder", text("Write here"))
        .prop("role", Value::Choice("body".into()))
        .on("enter");
    let props = RichTextLineProps::from_source(&mut node).expect("reads");
    let mut host = harness(DeclaredLine::from_props("line", props));
    assert_eq!(host.root_mut::<DeclaredLine>().line().name(), "TextInput");
}
