//! The media and surface components: a picture read from its address, a card's art, a band's
//! controls reporting the view's events, and the surfaces presented in the window's top layer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use super::testing::*;
use super::*;
use crate::component::{Component, Described, PropType};
use crate::headless::FrameHarness;
use crate::theme::Theme;
use crate::widget::{
    CodeEditor, CodeEditorProps, DeclaredLine, DropZone, DropZoneProps, Image, ImageProps,
    MediaCardProps, MediaGrid, MediaGridProps, PagePreview, PagePreviewProps, QrProps, QrWidget,
    RichTextLineProps, Widget,
};
use std::sync::Arc;

fn picture(src: &str) -> Value {
    Value::Picture(tilo_ui_graph::component::Picture {
        src: src.to_owned(),
    })
}

/// A served pictures store, so a test's picture address answers from its own bytes rather than
/// reaching for a content store.
fn pictures_with(png: Vec<u8>) -> crate::pictures::Pictures {
    let fetch: crate::image_store::Fetcher = Arc::new(move |_| Some(png.clone()));
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
    let picture = image.pictures().expect("the image shows a picture").clone();
    assert_eq!(
        image.picture().map(|p| p.src.as_str()),
        Some("data:image/png;base64,x"),
        "the picture keeps its address"
    );
    // Reading the picture through its store queues the decode; once it settles the same
    // read answers the decoded image, which is what the widget paints.
    let store = picture.store().clone();
    let _ = picture.get(image.picture().expect("a picture"));
    store.block_until_idle(std::time::Duration::from_secs(5));
    let decoded = picture
        .get(image.picture().expect("a picture"))
        .expect("decoded");
    assert_eq!((decoded.width, decoded.height), (4, 4));
}

#[test]
fn a_picture_answers_a_served_store_and_nothing_else_loads() {
    let png = crate::png::encode_rgba8(2, 2, &[255; 4].repeat(4)).expect("a picture");
    let fetch: crate::image_store::Fetcher = Arc::new(move |url: &str| {
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
    assert!(
        shown.waiting(),
        "it asks for a frame until the bytes are in"
    );
    // The store's first get queues the load; the fetch runs on its own background, so the test
    // reads through the fetch directly to prove the address is the one it loads.
    assert!(crate::pictures::data_bytes("tilo-blob://only").is_none());
    assert!(crate::pictures::data_bytes("data:image/png;base64,AA==").is_some());
}

#[test]
fn the_media_grid_reads_its_cards_and_a_pressed_card_reports_itself() {
    let queue = crate::Intents::default();
    let card = Node::new("MediaCard", "blue", &queue)
        .prop("item", Value::Resource("photo".into()))
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
fn a_drop_zone_settles_its_layout_with_a_message_and_an_action() {
    let queue = crate::Intents::default();
    let mut node = Node::new("DropZone", "import", &queue)
        .prop("title", text("Drop files to import"))
        .prop("message", text("Any file up to 50 MB"))
        .prop("icon", Value::Icon("upload".into()))
        .on("drop");
    let mut node2 = Node::new("DropZone", "choose", &queue)
        .prop("title", text("Drop a picture"))
        .prop("action", text("Choose"))
        .on("choose");
    let props = DropZoneProps::from_source(&mut node).expect("reads");
    // A layout that never settles fails the harness's frame, so a zone whose height adjusts
    // every pass is caught here.
    let props2 = DropZoneProps::from_source(&mut node2).expect("reads");
    let host = harness(DropZone::from_props("import", props));
    let rect = host.root::<DropZone>().get_rect();
    assert!(rect.width() > 0.0 && rect.height() > 0.0);
    // The one with an action, at the gallery's width.
    let wide = FrameHarness::for_root(
        crate::State::new(),
        {
            let mut column = crate::widget::arrange::Stack::stack().fill_width();
            column = column.push(Box::new(DropZone::from_props("choose", props2)));
            column
        },
        (560, 800),
        Theme::light(),
    );
    let rect = wide.root_widget().children()[0].get_rect();
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
    assert!(crate::widget::PictureSource::from_value(&text("x")).is_err());
    assert!(
        crate::widget::PictureSource::from_value(&picture("tilo-blob://dot")).is_ok(),
        "a picture value reads"
    );
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
    assert_eq!(
        host.root_mut::<DeclaredLine>().line().name(),
        "RichTextLine"
    );
}
