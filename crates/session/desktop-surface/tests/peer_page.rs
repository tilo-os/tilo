//! A window's page another device publishes, as this desktop draws it: the objects its publisher's
//! engine sent rebuild the very frame the publisher holds, and the tile draws that frame with this
//! toolkit's own Vello and text, so a viewer sees the page the window shows.

#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup or a wrong message is the assertion"
)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use tilo_desktop_model::{DesktopTree, IdMinter, Tab, TabId, TileContent as ModelTile, Workspace};
use tilo_desktop_surface::page_view::PageStream;
use tilo_desktop_surface::{
    NeighbourMsg, PeerId, PeerStreamStart, PeerSurfaces, SurfaceId, TileSource, seed_tree,
};
use ui::headless::FrameHarness;
use ui::offscreen::OffscreenRenderer;
use ui::webview::paint::stream::wire::Payload;
use ui::webview::paint::{PageCanvas, PaintEngine};
use ui::webview::test_engine::TestEngine;
use ui::webview::{WebViewCommand, WebViewEngine};

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

/// A desk with one window another device shows.
fn desk(minter: &mut IdMinter) -> (DesktopTree, TabId) {
    let tab = Tab {
        id: minter.mint(),
        title: "Browser".into(),
        tile: ModelTile::PeerWindow {
            app_id: APP.into(),
            label: "Browser".into(),
            note: String::new(),
            missing: None,
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let id = tab.id;
    (seed_tree(minter, "Desk", vec![tab]), id)
}

/// The frame drawn for `stream` at `(width, height)`, and the frame its publisher holds drawn the
/// same way: the pixels a viewer sees beside the pixels the publisher would show.
fn drawn(stream: &Rc<RefCell<PageStream>>) -> (Vec<u8>, Vec<u8>) {
    let harness = FrameHarness::new(
        ui::state::State::new(),
        PageStream::view(stream, "Browser".into()),
        (WIDTH, HEIGHT),
    );
    let viewed = harness.render_rgba();
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
    let published = renderer.render(&scene, ui::Theme::dark().tokens.colors.surfaces.content);
    (viewed, published)
}

#[test]
fn a_viewer_draws_the_frame_its_publisher_holds() {
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
    let (viewed, published) = drawn(&stream);
    assert_eq!(
        OffscreenRenderer::pixels_differing_beyond_aa(&viewed, &published),
        0,
        "the tile draws the page the publisher holds"
    );
}

#[test]
fn a_page_stream_takes_the_tab_of_its_window_and_lets_it_go_when_it_ends() {
    let mut minter = IdMinter::new(7);
    let (tree, tab) = desk(&mut minter);
    let mut surfaces = PeerSurfaces::default();
    let started = PeerStreamStart::announced(&announcement(tab), LOCAL, "Laptop", "Browser", false)
        .expect("a page stream is announced like any other");
    assert_eq!(surfaces.start(&tree, &started), Some(tab));
    let read = surfaces.read(tab).expect("the tab holds the tile");
    assert!(read.page, "the tile draws the window's page");
    assert_eq!(read.standing, tilo_desktop_surface::Standing::Started);

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
    assert_eq!(read.standing, tilo_desktop_surface::Standing::Pictured);
    assert_eq!(
        surfaces.end(PUBLISHER, SURFACE, "the sender stopped"),
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
fn a_tile_draws_the_page_the_desk_gives_it() {
    let mut minter = IdMinter::new(7);
    let (tree, tab) = desk(&mut minter);
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
        "the desk hands the tab the page's tile"
    );
    let engine = engine();
    for object in payloads(&engine) {
        peers.borrow_mut().push_page(PUBLISHER, SURFACE, &object);
    }
    assert_eq!(
        peers.borrow().read(tab).map(|read| read.standing),
        Some(tilo_desktop_surface::Standing::Pictured)
    );
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
