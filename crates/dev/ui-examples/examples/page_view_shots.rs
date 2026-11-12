//! Offscreen shots of a window's page another device published, as this toolkit draws it.
//!
//! A hosted Browser window's page travels the workspace's live plane as a display list
//! (`tilo-page/1`), and a desk draws it with the same Vello and text stack it draws everything
//! else with. These shots prove what a viewer sees:
//!
//! - `page-desk-{dark,light}.png` — a desk whose window is another device's: the page the
//!   publisher's engine drew, in the window's tile, with the desk's own chrome.
//! - `page-tile-{dark,light}.png` — that tile alone, which is what a browser tab mounts for the
//!   same window: the same code, in a page.
//!
//! Writes into `OUT_DIR` (else the first argument, else `target/look/page-view`).
//!
//! Run: `cargo run -p ui-examples --example page-view-shots [OUT_DIR]`

#![allow(
    clippy::expect_used,
    reason = "a capture that cannot write its shots must fail the process"
)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use tilo_desktop_model::{IdMinter, Tab, TabId, TileContent as ModelTile, Workspace};
use tilo_desktop_surface::page_view::PageStream;
use tilo_desktop_surface::{
    DesktopSurface, NeighbourMsg, PeerId, PeerStreamStart, PeerSurfaces, PeerTiles, SurfaceId,
    TileSource, seed_tree,
};
use tilo_ui::headless::FrameHarness;
use tilo_ui::theme::Theme;
use tilo_ui::webview::paint::PaintEngine;
use tilo_ui::webview::test_engine::TestEngine;
use tilo_ui::webview::{WebViewCommand, WebViewEngine};
use tilo_ui::widget::{TileContent, TileHost};

/// The page the publisher's engine drew: a header, a sidebar and a button.
const PAGE: &str = include_str!("../../../browser/paint-stream/tests/fixtures/layers.top.json");

const SURFACE: SurfaceId = 42;
const PUBLISHER: PeerId = 7;
const LOCAL: PeerId = 11;
const APP: &str = "so.tilo.browser";
const WIDTH: u32 = 800;
const HEIGHT: u32 = 600;

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::var("OUT_DIR").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("target/look/page-view"));
    std::fs::create_dir_all(&out).expect("the shots' folder");
    for theme in [Theme::dark(), Theme::light()] {
        let name = theme.name.clone();
        write(
            &out,
            &format!("page-desk-{name}.png"),
            &desk(&theme),
            1200,
            800,
        );
        write(
            &out,
            &format!("page-tile-{name}.png"),
            &tile(&theme),
            WIDTH,
            HEIGHT,
        );
    }
    println!("wrote {}", out.display());
}

fn write(out: &Path, name: &str, rgba: &[u8], width: u32, height: u32) {
    let path = out.join(name);
    tilo_ui::png::write_rgba8(&path, width, height, rgba).expect("a PNG");
    println!("{}", path.display());
}

/// The publisher's page as its engine hands it over: a whole frame, then the images it names.
fn engine() -> std::sync::Arc<PaintEngine> {
    let test = TestEngine::new();
    test.set_paint_snapshot(PAGE);
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

fn objects(engine: &PaintEngine) -> Vec<Vec<u8>> {
    engine
        .take_paint_stream(true)
        .iter()
        .map(|payload| payload.encode().expect("an encoded payload"))
        .collect()
}

fn announcement(tab: TabId) -> NeighbourMsg {
    NeighbourMsg::SurfaceStreamStart {
        surface: SURFACE,
        source_peer: PUBLISHER,
        width: WIDTH,
        height: HEIGHT,
        codec: tilo_ui::webview::paint::stream::wire::CODEC.into(),
        ts: 0,
        sender_monotonic_ns_at_send: 0,
        colorimetry: None,
        hdr_static_metadata: None,
        bit_depth: 8,
        app_id: APP.into(),
        tab,
    }
}

/// A desk with one window another device hosts, its tab in the workspace.
fn desk_tree() -> (tilo_desktop_model::DesktopTree, TabId) {
    let mut minter = IdMinter::new(7);
    let tab = Tab {
        id: minter.mint(),
        title: "Browser".into(),
        tile: ModelTile::HostedWindow {
            app_id: APP.into(),
            label: "Browser".into(),
            host: "ab".repeat(32),
            host_label: "Browser VM".into(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let id = tab.id;
    let tree = seed_tree(&mut minter, "Workspace", vec![tab]);
    (tree, id)
}

/// The window whose page reached this desk, as the desk draws it: the desktop's own chrome around
/// the page the publisher holds.
fn desk(theme: &Theme) -> Vec<u8> {
    let (tree, tab) = desk_tree();
    let peers = Rc::new(RefCell::new(PeerSurfaces::default()));
    let start =
        PeerStreamStart::announced(&announcement(tab), LOCAL, "Browser VM", "Browser", false)
            .expect("a page stream");
    peers.borrow_mut().start(&tree, &start);
    for object in objects(&engine()) {
        peers.borrow_mut().push_page(PUBLISHER, SURFACE, &object);
    }
    let source = PeerTiles::new(Box::new(NoTiles), Rc::clone(&peers));
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(source));
    handle.set_viewport_width(1200.0);
    let mut harness = FrameHarness::new_with_theme(
        tilo_ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        theme.clone(),
    );
    harness.relayout();
    harness.render_rgba()
}

/// The tile alone, which is what a browser tab mounts for the same window.
fn tile(theme: &Theme) -> Vec<u8> {
    let mut stream = PageStream::new();
    for object in objects(&engine()) {
        stream.apply(&object);
    }
    let stream = Rc::new(RefCell::new(stream));
    let mut harness = FrameHarness::new_with_theme(
        tilo_ui::state::State::new(),
        PageStream::view(&stream, "Browser".into()),
        (WIDTH, HEIGHT),
        theme.clone(),
    );
    harness.relayout();
    harness.render_rgba()
}

/// A host with nothing of its own in the window's tab.
struct NoTiles;

impl TileSource for NoTiles {
    fn tile(&mut self, _workspace: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}
