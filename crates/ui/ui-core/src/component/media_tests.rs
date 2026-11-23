//! The media and surface components: a picture read from its address, a card's art, a band's controls
//! reporting the view's events, and the surfaces presented in the window's top layer.

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
    CodeEditor, CodeEditorProps, DescribedCards, DropZone, DropZoneProps, Image, ImageProps,
    MediaCardProps, MediaGrid, MediaGridProps, PagePreview, PagePreviewProps, QrProps, QrWidget,
    Widget,
};
use std::sync::Arc;

fn picture(src: &str) -> Value {
    Value::Picture(tilo_ui_graph::component::Picture {
        src: src.to_owned(),
    })
}

fn pictures_with(png: Vec<u8>) -> crate::pictures::Pictures {
    let fetch: crate::image_store::Fetcher = Arc::new(move |_| Some(png.clone()));
    crate::pictures::Pictures::new(crate::ImageStore::new(&fetch))
}

fn card_of(props: MediaCardProps) -> crate::widget::MediaCard {
    let mut grid = MediaGrid::from_props(
        "library",
        MediaGridProps {
            size: None,
            justified: false,
            items: DescribedCards::given(vec![props]),
        },
    );
    grid.take_card(0).expect("one card")
}

tilo_test_env::scenario! {
    fn the_media_components_read_their_pictures_and_parts;

    phase an_image_is_a_picture_by_its_address_and_shows_its_bytes_when_they_arrive;
    {
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
        let store = picture.store().clone();
        let _ = picture.get(image.picture().expect("a picture"));
        store.block_until_idle(std::time::Duration::from_secs(5));
        let decoded = picture
            .get(image.picture().expect("a picture"))
            .expect("decoded");
        assert_eq!((decoded.width, decoded.height), (4, 4));
    }

    phase a_picture_answers_a_served_store_and_nothing_else_loads;
    {
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
        assert!(crate::pictures::data_bytes("tilo-blob://only").is_none());
        assert!(crate::pictures::data_bytes("data:image/png;base64,AA==").is_some());
    }

    phase the_media_grid_reads_its_cards_and_a_pressed_card_reports_itself;
    {
        let queue = crate::Intents::default();
        let card = Node::new("MediaCard", "blue", &queue)
            .prop("item", Value::Resource("photo".into()))
            .prop("title", text("Blue"))
            .prop("picture", picture("data:image/png;base64,x"))
            .on("activate");
        let mut grid = Node::new("MediaGrid", "library", &queue).holding("items", card);
        let props = MediaGridProps::from_source(&mut grid).expect("reads");
        assert_eq!(props.items.held().len(), 1);
        assert_eq!(props.items.held()[0].title.as_deref(), Some("Blue"));
    }

    phase a_pressed_card_opens_what_it_stands_for;
    {
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

    phase a_qr_code_re_encodes_a_changed_payload_in_place;
    {
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

    phase a_page_preview_clamps_its_page_into_its_count;
    {
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

    phase a_drop_zone_settles_its_layout_with_a_message_and_an_action;
    {
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

    phase a_code_editor_takes_the_language_its_view_names;
    {
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

    phase a_prop_of_an_undescribed_type_does_not_read;
    {
        // A picture is a `Picture` value, never text: the wrong kind is refused, not coerced.
        assert!(crate::widget::PictureSource::from_value(&text("x")).is_err());
        assert!(
            crate::widget::PictureSource::from_value(&picture("tilo-blob://dot")).is_ok(),
            "a picture value reads"
        );
    }

}

tilo_test_env::scenario! {
    fn a_views_video_player_offers_its_controls_and_reports_what_the_person_chooses;

    phase a_player_reads_its_speed_quality_and_captions_and_reports_the_menu_choices;
    {
        let queue = crate::Intents::default();
        let mut node = Node::new("VideoPlayer", "watch", &queue)
            .prop("playing", Value::Bool(false))
            .prop("rate", Value::Float(1.0))
            .prop(
                "qualities",
                Value::List(vec![text("1080p"), text("720p")]),
            )
            .prop(
                "caption_tracks",
                Value::Map(vec![
                    (text("en"), text("English")),
                    (text("fr"), text("Français")),
                ]),
            )
            .prop(
                "captions",
                text("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n"),
            )
            .on("set_rate")
            .on("select_quality")
            .on("select_captions");
        let props = crate::widget::VideoPlayerProps::from_source(&mut node).expect("reads");
        assert_eq!(props.qualities, ["1080p", "720p"]);
        assert_eq!(
            props.caption_tracks.get("en").map(String::as_str),
            Some("English")
        );
        let mut video = crate::widget::DeclaredVideo::from_props("watch", props);
        assert!(
            video.player().setting("playback-rate").is_some(),
            "the player offers a speed"
        );
        assert!(video.player().setting("quality").is_some());
        assert!(video.player().setting("captions").is_some());
        let twice = crate::widget::PLAYBACK_RATES
            .iter()
            .position(|rate| (*rate - 2.0).abs() < f64::EPSILON)
            .expect("2× is one of the speeds");
        assert!(video.player().choose_setting("playback-rate", twice));
        assert!(video.player().choose_setting("quality", 1));
        assert!(video.player().choose_setting("captions", 1));
        let doc = automerge::AutoCommit::new();
        crate::widget::Widget::sync(&mut video, &doc, &automerge::ROOT);
        assert_eq!(
            queue.take(),
            [
                fired("watch", "set_rate", Some(Value::Float(2.0))),
                fired("watch", "select_quality", Some(text("720p"))),
                fired("watch", "select_captions", Some(text("en"))),
            ],
            "the view is told the speed, the quality and the track the person chose"
        );
    }

    phase turning_captions_off_reports_the_empty_track;
    {
        let queue = crate::Intents::default();
        let mut node = Node::new("VideoPlayer", "watch", &queue)
            .prop(
                "caption_tracks",
                Value::Map(vec![(text("en"), text("English"))]),
            )
            .prop("caption_language", text("en"))
            .on("select_captions");
        let props = crate::widget::VideoPlayerProps::from_source(&mut node).expect("reads");
        let mut video = crate::widget::DeclaredVideo::from_props("watch", props);
        assert!(video.player().choose_setting("captions", 0));
        let doc = automerge::AutoCommit::new();
        crate::widget::Widget::sync(&mut video, &doc, &automerge::ROOT);
        assert_eq!(
            queue.take(),
            [fired("watch", "select_captions", Some(text("")))]
        );
    }
}
