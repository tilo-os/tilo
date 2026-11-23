//! A window's page another device publishes, as this desktop draws it: the objects its publisher's
//! engine sent rebuild the very frame the publisher holds, and the tile draws that frame with this
//! toolkit's own Vello and text, so a viewer sees the page the window shows.

#![cfg(not(target_arch = "wasm32"))]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use tilo_desktop_model::{IdMinter, Layout, Tab, TabId, TileContent as ModelTile, Workspace};
use tilo_desktop_surface::page_view::PageStream;
use tilo_desktop_surface::{
    DesktopHandle, DesktopSurface, NeighbourMsg, PeerId, PeerStreamStart, PeerSurfaces,
    StreamState, SurfaceId, TileSource,
};
use tilo_session_core::desktop::seed_tree;
use tilo_webview::test_engine::TestEngine;
use tilo_webview::{WebViewCommand, WebViewEngine};
use ui::headless::FrameHarness;
use ui::offscreen::OffscreenRenderer;
use ui::webview::paint::stream::wire::Payload;
use ui::webview::paint::{PageCanvas, PaintEngine};

/// The engine's page: a snapshot with a header, a sidebar and a button.
const TOP: &str = include_str!("../../../browser/paint-stream/tests/fixtures/layers.top.json");

/// The surface the publishing device names the window by.
const SURFACE: SurfaceId = 42;
/// The device publishing the page.
const PUBLISHER: PeerId = 7;
/// This device, as the publisher knows whoever watches.
const LOCAL: PeerId = 11;
/// The app whose window is shown.
const APP: &str = "so.tilo.browser";
const WIDTH: u32 = 800;
const HEIGHT: u32 = 600;

/// The page's engine, painting what it holds as the paint stream a window hands the session.
fn engine() -> Arc<PaintEngine> {
    let test = TestEngine::new();
    test.set_paint_snapshot(TOP);
    let engine = PaintEngine::with_interval(test, Duration::ZERO);
    engine.send(WebViewCommand::Resize {
        width: WIDTH,
        height: HEIGHT,
        scale_factor: 1.0,
    });
    for _ in 0..4 {
        drop(engine.paint_frame());
        drop(engine.drain_events());
    }
    engine
}

/// Every object of the page, as its publisher puts them on the wire.
fn payloads(engine: &PaintEngine) -> Vec<Vec<u8>> {
    engine
        .take_paint_stream(true)
        .iter()
        .map(|payload| payload.encode().expect("an encoded payload"))
        .collect()
}

/// The announcement of a page stream, as the device hosting the window sends it.
fn announcement(tab: TabId) -> NeighbourMsg {
    NeighbourMsg::SurfaceStreamStart {
        surface: SURFACE,
        source_peer: PUBLISHER,
        width: WIDTH,
        height: HEIGHT,
        codec: ui::webview::paint::stream::wire::CODEC.into(),
        ts: 0,
        sender_monotonic_ns_at_send: 0,
        colorimetry: None,
        hdr_static_metadata: None,
        bit_depth: 8,
        app_id: APP.into(),
        tab,
    }
}

/// A shell with one window another device shows.
fn shell(minter: &mut IdMinter) -> (Layout, TabId) {
    let tab = Tab {
        id: minter.mint(),
        title: "Browser".into(),
        tile: ModelTile::HostedWindow {
            app_id: APP.into(),
            label: "Browser".into(),
            host: "ab".repeat(32),
            host_label: "Laptop".into(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let id = tab.id;
    (seed_tree(minter, "Office", vec![tab]), id)
}

/// The frame the tile draws for `stream` at `(WIDTH, HEIGHT)`: the pixels a viewer sees.
fn viewed(stream: &Rc<RefCell<PageStream>>) -> Vec<u8> {
    let harness = FrameHarness::new(
        ui::state::State::new(),
        PageStream::view(stream, "Browser".into()),
        (WIDTH, HEIGHT),
    );
    harness.render_rgba()
}

/// The frame `stream`'s publisher holds, drawn the way the tile draws it: the pixels the
/// publisher would show.
fn published(stream: &Rc<RefCell<PageStream>>) -> Vec<u8> {
    let page = stream.borrow();
    let frame = page.frame().expect("the viewer holds the frame").clone();
    drop(page);
    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT);
    let mut text = ui::TextRenderer::new();
    let mut scene = ui::vello::Scene::new();
    {
        let blobs = move |_: &[u8; 32]| None::<Arc<Vec<u8>>>;
        let mut canvas = PageCanvas {
            scene: &mut scene,
            transform: ui::kurbo::Affine::IDENTITY,
            text: &mut text,
            blobs: &blobs,
        };
        ui::webview::paint::PageRenderer::new().draw(&mut canvas, &frame);
    }
    renderer.render(&scene, ui::Theme::dark().tokens.colors.surfaces.content)
}

/// One page, published once and drawn two ways: the objects its publisher sent rebuild the
/// frame the publisher holds, and the tile draws that frame. Both pictures are drawn in one
/// process: the first frame a process renders pays for its renderer, and each was a test of its
/// own that paid it again over the same page.
#[test]
fn a_viewer_draws_the_frame_the_publisher_holds() {
    let engine = engine();
    let objects = payloads(&engine);
    let mut stream = PageStream::new();
    for object in &objects {
        stream.apply(object);
    }
    assert_eq!(
        stream.frame(),
        engine.paint_frame().as_deref(),
        "the objects rebuild the publisher's frame"
    );
    let stream = Rc::new(RefCell::new(stream));
    let direct = viewed(&stream);
    assert_eq!(
        OffscreenRenderer::pixels_differing_beyond_aa(&direct, &published(&stream)),
        0,
        "the tile draws the page the publisher holds"
    );
}

/// A form with every common control, its first field being edited, as an engine laid it out
/// in a window of 1280 by 800.
const FORM: &str = include_str!("../../../browser/paint-stream/tests/fixtures/controls.json");

/// The engine's number for the element of the form whose `id` is `id`.
fn form_node(id: &str) -> u64 {
    let form: serde_json::Value = serde_json::from_str(FORM).expect("the fixture");
    let string = |index: &serde_json::Value| {
        index
            .as_u64()
            .and_then(|at| form["strings"].get(usize::try_from(at).ok()?))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let nodes = &form["documents"][0]["nodes"];
    let found = nodes["attributes"]
        .as_array()
        .expect("attributes")
        .iter()
        .position(|held| {
            held.as_array().is_some_and(|held| {
                held.chunks_exact(2)
                    .any(|pair| string(&pair[0]) == "id" && string(&pair[1]) == id)
            })
        })
        .expect("an element of that id");
    nodes["backendNodeId"][found].as_u64().expect("a node")
}

/// The form as the tile of a watching shell draws it in `theme`, 1280 by 800 pixels.
fn form_tile(theme: &ui::Theme) -> Vec<u8> {
    let test = TestEngine::new();
    test.set_paint_snapshot(FORM);
    test.set_paint_shown([(form_node("date"), "03/01/2027".to_owned())]);
    let engine = PaintEngine::with_interval(test, Duration::ZERO);
    engine.send(WebViewCommand::Resize {
        width: 1280,
        height: 800,
        scale_factor: 1.0,
    });
    for _ in 0..6 {
        drop(engine.paint_frame());
        drop(engine.drain_events());
    }
    let mut stream = PageStream::new();
    for object in payloads(&engine) {
        stream.apply(&object);
    }
    let stream = Rc::new(RefCell::new(stream));
    let mut harness = FrameHarness::new_with_theme(
        ui::state::State::new(),
        PageStream::view(&stream, "Browser".into()),
        (1280, 800),
        theme.clone(),
    );
    harness.relayout();
    harness.render_rgba()
}

/// The pixels of `picture` (1280 wide) in the box from (`left`, `top`), `width` by `height`.
fn pixels_in(picture: &[u8], left: usize, top: usize, width: usize, height: usize) -> Vec<[u8; 3]> {
    let across = left..left.saturating_add(width);
    let down = top..top.saturating_add(height);
    picture
        .chunks_exact(4)
        .enumerate()
        .filter(|(index, _)| across.contains(&(index % 1280)) && down.contains(&(index / 1280)))
        .map(|(_, pixel)| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// What the engine's own theme paints of a form reaches a watching shell as the sharing
/// person sees it: the ring around the field being edited, a select's mark, a ticked box in
/// the accent, a date as the engine words it. The page is the page's: it is drawn the same
/// in a dark theme and in a light one.
#[test]
fn a_form_is_drawn_with_what_the_engines_theme_paints_of_it_in_any_theme() {
    let dark = form_tile(&ui::Theme::dark());
    let light = form_tile(&ui::Theme::light());
    assert_eq!(
        OffscreenRenderer::pixels_differing_beyond_aa(&dark, &light),
        0,
        "a page is drawn in its own colours whatever the shell's theme"
    );
    let is_dark = |pixel: &[u8; 3]| pixel.iter().all(|channel| *channel < 96);
    // The field being edited ("Name", from 57 to 263 across, 86 to 110 down) wears the
    // engine's ring, two pixels wide just outside its edge; the field after it wears none.
    let ring = pixels_in(&dark, 55, 90, 2, 12);
    assert!(
        ring.iter().all(is_dark),
        "the ring around the field being edited: {ring:?}"
    );
    let no_ring = pixels_in(&dark, 316, 90, 2, 12);
    assert!(
        !no_ring.iter().any(is_dark),
        "no ring around a field nobody edits: {no_ring:?}"
    );
    // The select's mark, in the sixteen pixels before its right edge.
    let mark = pixels_in(&dark, 116, 250, 16, 16);
    assert!(
        mark.iter().filter(|pixel| is_dark(pixel)).count() >= 8,
        "a select says that it opens"
    );
    // The ticked box, filled with the accent, its tick white.
    let ticked = pixels_in(&dark, 12, 289, 13, 13);
    let accent = ticked
        .iter()
        .filter(|[r, g, b]| *r < 60 && (90..150).contains(g) && *b > 200)
        .count();
    let tick = ticked
        .iter()
        .filter(|pixel| pixel.iter().all(|c| *c > 200))
        .count();
    assert!(
        accent >= 60 && tick >= 6,
        "{accent} of the accent, {tick} of the tick"
    );
    // The date, as the engine words it: ten letters of ink in the field's room, which the
    // eight of what it holds would not reach.
    let date = pixels_in(&dark, 51, 210, 100, 16);
    let inked: Vec<usize> = date
        .iter()
        .enumerate()
        .filter(|(_, pixel)| is_dark(pixel))
        .map(|(index, _)| index % 100)
        .collect();
    assert!(
        inked.iter().max().is_some_and(|right| *right >= 60),
        "the date is drawn: ink reaches {:?}",
        inked.iter().max()
    );
}

/// What a device is sent is not always a payload. Bytes that are none change nothing: a viewer
/// holds no frame before its publisher's first, and keeps the one it holds after.
#[test]
fn bytes_that_are_no_payload_leave_a_viewer_the_page_it_holds() {
    use ui::webview::paint::stream::wire::MAX_PAYLOAD_BYTES;

    let objects = payloads(&engine());
    let (_, cut) = objects
        .last()
        .and_then(|whole| whole.split_last())
        .expect("the page has an object, and it has bytes");
    let none: &[u8] = &[];
    let noise = [0xff_u8; 64];
    let oversized = vec![0_u8; MAX_PAYLOAD_BYTES.saturating_add(1)];
    let refused: [(&[u8], &str); 4] = [
        (none, "no bytes"),
        (&noise[..], "noise"),
        (cut, "a payload cut short by a byte"),
        (&oversized[..], "more bytes than a payload may be"),
    ];
    let mut stream = PageStream::new();
    for (bytes, case) in refused {
        assert!(!stream.apply(bytes), "{case}: nothing to show changed");
        assert!(stream.frame().is_none(), "{case}: made no frame");
    }
    for object in &objects {
        stream.apply(object);
    }
    let held = stream
        .frame()
        .cloned()
        .expect("the publisher's whole frame");
    for (bytes, case) in refused {
        assert!(!stream.apply(bytes), "{case}: the frame shown is the same");
        assert_eq!(
            stream.frame(),
            Some(&held),
            "{case}: the viewer keeps its frame"
        );
    }
}

#[test]
fn a_page_stream_takes_the_tab_of_its_window_and_lets_it_go_when_it_ends() {
    let mut minter = IdMinter::new(7);
    let (tree, tab) = shell(&mut minter);
    let mut surfaces = PeerSurfaces::default();
    let started = PeerStreamStart::announced(&announcement(tab), LOCAL, "Laptop", "Browser", false)
        .expect("a page stream is announced like any other");
    assert_eq!(surfaces.start(&tree, &started), Some(tab));
    let read = surfaces.read(tab).expect("the tab holds the tile");
    assert!(read.page, "the tile draws the window's page");
    assert_eq!(read.state, tilo_desktop_surface::StreamState::Started);

    let engine = engine();
    let objects = payloads(&engine);
    let mut changed = Vec::new();
    for object in &objects {
        let outcome = surfaces.push_page(PUBLISHER, SURFACE, object);
        assert!(
            outcome.is_some(),
            "every object of the page reaches the tile"
        );
        changed.push(outcome.unwrap_or(false));
    }
    assert_eq!(changed.first(), Some(&true), "the whole frame is drawn");
    assert!(
        surfaces
            .push_page(PUBLISHER + 1, SURFACE, &objects[0])
            .is_none(),
        "another device's stream is not this tile's"
    );
    let read = surfaces.read(tab).expect("the tab holds the tile");
    assert_eq!(read.state, tilo_desktop_surface::StreamState::Pictured);
    assert_eq!(
        surfaces.end(
            PUBLISHER,
            SURFACE,
            tilo_desktop_surface::WhyEnded::SenderStopped
        ),
        Some(tab)
    );
    assert!(
        surfaces.read(tab).is_none(),
        "the tile went with the stream"
    );
}

#[test]
fn a_late_viewer_starts_at_the_whole_frame() {
    let engine = engine();
    // A viewer that joined now takes the whole frame alone, without the patches before it.
    let whole: Vec<Vec<u8>> = payloads(&engine)
        .into_iter()
        .filter(|object| Payload::is_key(object))
        .collect();
    assert!(!whole.is_empty(), "the publisher starts from a whole frame");
    let mut stream = PageStream::new();
    for object in &whole {
        stream.apply(object);
    }
    assert_eq!(stream.frame(), engine.paint_frame().as_deref());
}

#[test]
fn a_tile_draws_the_page_the_shell_gives_it() {
    let mut minter = IdMinter::new(7);
    let (tree, tab) = shell(&mut minter);
    let peers = Rc::new(RefCell::new(PeerSurfaces::default()));
    let started = PeerStreamStart::announced(&announcement(tab), LOCAL, "Laptop", "Browser", false)
        .expect("a page stream is announced like any other");
    peers.borrow_mut().start(&tree, &started);
    let mut source = tilo_desktop_surface::PeerTiles::new(Box::new(NoTiles), Rc::clone(&peers));
    let workspace: &Workspace = &tree.workspaces[0];
    let failed = source.tile(
        workspace,
        &tree.workspaces[0].strips[0].window_groups[0].tabs[0],
    );
    assert!(
        !matches!(failed.content(), ui::widget::TileContent::Notice { .. }),
        "the shell hands the tab the page's tile"
    );
    let engine = engine();
    for object in payloads(&engine) {
        peers.borrow_mut().push_page(PUBLISHER, SURFACE, &object);
    }
    assert_eq!(
        peers.borrow().read(tab).map(|read| read.state),
        Some(tilo_desktop_surface::StreamState::Pictured)
    );
}

/// A shell whose one tab lists a window another device runs, in the toolkit's own frame pipeline.
fn shown_shell() -> (FrameHarness, DesktopHandle, TabId) {
    let mut minter = IdMinter::new(7);
    let (tree, tab) = shell(&mut minter);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(NoTiles));
    handle.set_viewport_width(1200.0);
    let mut driver = FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::MemberId::person("tester"),
    );
    driver.set_caller_palette_enabled(false);
    driver.relayout();
    (driver, handle, tab)
}

/// The names of the widgets the first widget called `under` is built of, itself first: what a
/// tile holds in its place.
fn built_of(root: &dyn ui::widget::Widget, under: &str) -> Vec<String> {
    fn all(widget: &dyn ui::widget::Widget, out: &mut Vec<String>) {
        out.push(widget.name().to_owned());
        widget.for_each_child(&mut |child| all(child, out));
    }
    fn find(widget: &dyn ui::widget::Widget, under: &str, out: &mut Vec<String>) -> bool {
        if widget.name() == under {
            all(widget, out);
            return true;
        }
        let mut found = false;
        widget.for_each_child(&mut |child| {
            if !found {
                found = find(child, under, out);
            }
        });
        found
    }
    let mut out = Vec::new();
    find(root, under, &mut out);
    out
}

/// A person who cannot see the tile is told what one who can sees there: the tile is a document
/// called what it is, holding the text its page shows, each run where the tile draws it. That is
/// also what anything listing the shell's named elements reads of the page.
#[test]
fn a_tile_names_the_text_its_page_shows_where_it_draws_it() {
    let engine = engine();
    let mut stream = PageStream::new();
    for object in payloads(&engine) {
        stream.apply(&object);
    }
    let stream = Rc::new(RefCell::new(stream));
    let mut harness = FrameHarness::new(
        ui::state::State::new(),
        PageStream::view(&stream, "Browser on Laptop".into()),
        (WIDTH, HEIGHT),
    );
    harness.relayout();
    let texts = ui::a11y::texts(harness.root_widget());
    assert!(
        texts.iter().any(|text| text == "Browser on Laptop"),
        "the tile says what it is: {texts:?}"
    );
    for shown in ["Paint stream", "Alpha"] {
        assert!(
            texts.iter().any(|text| text.contains(shown)),
            "the tile names {shown:?}, which its page shows: {texts:?}"
        );
    }
    let placed: Vec<(String, [f64; 4])> = harness
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            Some((
                node.label()?.to_owned(),
                [bounds.x0, bounds.y0, bounds.x1, bounds.y1],
            ))
        })
        .collect();
    let (_, [x0, y0, x1, y1]) = placed
        .iter()
        .find(|(name, _)| name.contains("Alpha"))
        .expect("the run is exported with where it stands");
    assert!(
        *x0 >= 0.0
            && *y0 >= 0.0
            && *x1 <= f64::from(WIDTH)
            && *y1 <= f64::from(HEIGHT)
            && x1 > x0
            && y1 > y0,
        "the run stands in the tile's box: {placed:?}"
    );
}

/// The tab a shell lists another device's window in shows the arriving card while only the start
/// of that window's page came, and the page once its whole frame did: the tile is built again
/// from what it holds, with no part of the placeholder left in it.
#[test]
fn the_tab_listing_a_window_leaves_its_arriving_card_for_the_page_that_came() {
    let (mut driver, handle, tab) = shown_shell();
    let started = PeerStreamStart::announced(&announcement(tab), LOCAL, "Laptop", "Browser", false)
        .expect("a page stream is announced like any other");
    assert_eq!(
        handle.show_peer_surface(&started),
        Some(tab),
        "the tab that lists the window takes its page"
    );
    driver.relayout();
    let waiting = built_of(driver.root_widget(), "PeerView");
    let holds = |built: &[String], name: &str| built.iter().any(|part| part == name);
    assert!(
        holds(&waiting, "ArrivingView") && !holds(&waiting, "PageView"),
        "a page of which only the start came waits in the arriving card: {waiting:?}"
    );
    assert_eq!(
        handle.peer_surface_read(tab).map(|read| read.state),
        Some(StreamState::Started)
    );
    let texts = ui::a11y::texts(driver.root_widget());
    assert!(
        !texts.iter().any(|text| text.contains("Paint stream")),
        "nothing of the page is named before it came: {texts:?}"
    );

    for object in payloads(&engine()) {
        assert!(
            handle
                .peer_surface_page(PUBLISHER, SURFACE, &object)
                .is_some(),
            "every object of the page reaches the tile"
        );
    }
    driver.relayout();
    driver.relayout();
    let drawn = built_of(driver.root_widget(), "PeerView");
    assert!(
        holds(&drawn, "PageView") && !holds(&drawn, "ArrivingView"),
        "the tile draws the page that came, and no placeholder: {drawn:?}"
    );
    assert_eq!(
        handle.peer_surface_read(tab).map(|read| read.state),
        Some(StreamState::Pictured)
    );
    let texts = ui::a11y::texts(driver.root_widget());
    assert!(
        texts.iter().any(|text| text == "Browser on Laptop"),
        "the tile says whose window it is: {texts:?}"
    );
    for shown in ["Paint stream", "Alpha"] {
        assert!(
            texts.iter().any(|text| text.contains(shown)),
            "the shell names {shown:?}, which the page shows: {texts:?}"
        );
    }
}

/// A host with nothing of its own in the tab.
struct NoTiles;

impl TileSource for NoTiles {
    fn tile(&mut self, _workspace: &Workspace, tab: &Tab) -> ui::widget::TileHost {
        ui::widget::TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}
