//! Offscreen shots of a window's page another device published, as this toolkit draws it.
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
    TileSource,
};
use tilo_session_core::desktop::seed_tree;
use tilo_ui::headless::FrameHarness;
use tilo_ui::theme::Theme;
use tilo_ui::webview::paint::PaintEngine;
use tilo_ui::widget::{TileContent, TileHost};
use tilo_webview::test_engine::TestEngine;
use tilo_webview::{WebViewCommand, WebViewEngine};

/// The page the publisher's engine drew: a header, a sidebar and a button.
const PAGE: &str = include_str!("../../../browser/paint-stream/tests/fixtures/layers.top.json");
/// A form with every common control, its first field being edited, as an engine laid it out
/// in a window of 1280 by 800: what the engine's theme paints of each control is drawn by the
/// publisher from the layout.
const FORM: &str = include_str!("../../../browser/paint-stream/tests/fixtures/controls.json");
/// What the engine says the form's date and time show.
const FORM_SHOWN: [(&str, &str); 3] = [
    ("date", "03/01/2027"),
    ("nodate", "mm/dd/yyyy"),
    ("time", "02:30\u{202f}PM"),
];
const FORM_WIDTH: u32 = 1280;
const FORM_HEIGHT: u32 = 800;

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
            &format!("page-shell-{name}.png"),
            &shell(&theme),
            1200,
            800,
        );
        write(
            &out,
            &format!("page-tile-{name}.png"),
            &tile(&theme, &engine(PAGE, (WIDTH, HEIGHT), &[]), (WIDTH, HEIGHT)),
            WIDTH,
            HEIGHT,
        );
        write(
            &out,
            &format!("form-tile-{name}.png"),
            &tile(
                &theme,
                &engine(FORM, (FORM_WIDTH, FORM_HEIGHT), &FORM_SHOWN),
                (FORM_WIDTH, FORM_HEIGHT),
            ),
            FORM_WIDTH,
            FORM_HEIGHT,
        );
    }
    println!("wrote {}", out.display());
}

fn write(out: &Path, name: &str, rgba: &[u8], width: u32, height: u32) {
    let path = out.join(name);
    tilo_ui::png::write_rgba8(&path, width, height, rgba).expect("a PNG");
    println!("{}", path.display());
}

/// The engine's number for the element of the snapshot `page` whose `id` is `id`.
fn node_of(page: &serde_json::Value, id: &str) -> Option<u64> {
    let strings = page.get("strings")?.as_array()?;
    let string = |index: &serde_json::Value| {
        index
            .as_u64()
            .and_then(|at| strings.get(usize::try_from(at).ok()?))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let nodes = page.pointer("/documents/0/nodes")?;
    let found = nodes
        .get("attributes")?
        .as_array()?
        .iter()
        .position(|held| {
            held.as_array().is_some_and(|held| {
                held.chunks_exact(2).any(|pair| match pair {
                    [name, value] => string(name) == "id" && string(value) == id,
                    _ => false,
                })
            })
        })?;
    nodes.get("backendNodeId")?.get(found)?.as_u64()
}

/// The publisher's page as its engine hands it over: a whole frame, then the images it names.
/// `shown` is what the engine says fields of the page show, by their `id`.
fn engine(page: &str, size: (u32, u32), shown: &[(&str, &str)]) -> std::sync::Arc<PaintEngine> {
    let test = TestEngine::new();
    test.set_paint_snapshot(page);
    if !shown.is_empty() {
        let read: serde_json::Value = serde_json::from_str(page).expect("a layout snapshot");
        test.set_paint_shown(
            shown
                .iter()
                .filter_map(|(id, text)| Some((node_of(&read, id)?, (*text).to_owned()))),
        );
    }
    let engine = PaintEngine::with_interval(test, Duration::ZERO);
    engine.send(WebViewCommand::Resize {
        width: size.0,
        height: size.1,
        scale_factor: 1.0,
    });
    for _ in 0..6 {
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

/// A shell with one window another device hosts, its tab in the workspace.
fn shell_tree() -> (tilo_desktop_model::Layout, TabId) {
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

/// The window whose page reached this shell, as the shell draws it: the desktop's own chrome around
/// the page the publisher holds.
fn shell(theme: &Theme) -> Vec<u8> {
    let (tree, tab) = shell_tree();
    let peers = Rc::new(RefCell::new(PeerSurfaces::default()));
    let start =
        PeerStreamStart::announced(&announcement(tab), LOCAL, "Browser VM", "Browser", false)
            .expect("a page stream");
    peers.borrow_mut().start(&tree, &start);
    for object in objects(&engine(PAGE, (WIDTH, HEIGHT), &[])) {
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
fn tile(theme: &Theme, engine: &PaintEngine, size: (u32, u32)) -> Vec<u8> {
    let mut stream = PageStream::new();
    for object in objects(engine) {
        stream.apply(&object);
    }
    let stream = Rc::new(RefCell::new(stream));
    let mut harness = FrameHarness::new_with_theme(
        tilo_ui::state::State::new(),
        PageStream::view(&stream, "Browser".into()),
        size,
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
