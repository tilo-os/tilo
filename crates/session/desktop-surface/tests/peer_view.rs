//! Another device's shared window, as this desktop shows it: a stream arriving from a peer is
//! decoded into its tab's tile, published there as a resource with a picture, and driven by the
//! actions that resource publishes.

#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::panic,
    reason = "test fixtures: a failed lookup or a wrong message is the assertion"
)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tilo_desktop_model::{IdMinter, Tab, TabId, TileContent as ModelTile, Workspace};
use tilo_desktop_surface::{
    DesktopHandle, DesktopSurface, NeighbourMsg, PeerId, PeerOutcome, PeerRead, PeerStreamStart,
    PeerSurface, PeerSurfaces, Standing, SurfaceId, TileSource, seed_tree,
};
use tilo_window_share::streaming::InputEvent;
use ui::headless::FrameHarness;
use ui::widget::{TileContent, TileHost};

/// The surface the sharing device names its window by.
const SURFACE: SurfaceId = 42;
/// The device sharing the window.
const SHARER: PeerId = 7;
/// This device, as the sender knows whoever sends input back.
const LOCAL: PeerId = 11;
/// The app whose window is shared.
const APP: &str = "so.tilo.writer";
/// The picture size the sender announces and encodes.
const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

/// A host that holds only a notice for each tab, so a shared window taking its tab over is what the
/// tile shows.
struct HostTiles;

impl TileSource for HostTiles {
    fn tile(&mut self, _workspace: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

/// A tab another device shares its window in.
fn peer_tab(minter: &mut IdMinter) -> Tab {
    Tab {
        id: minter.mint(),
        title: "Writer".into(),
        tile: ModelTile::HostedWindow {
            app_id: APP.into(),
            label: "Writer".into(),
            host: "ab".repeat(32),
            host_label: "Laptop".into(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

/// A desktop showing one window another device shares, in the toolkit's own frame pipeline.
fn desk() -> (FrameHarness, DesktopHandle, TabId) {
    let mut minter = IdMinter::new(7);
    let tab = peer_tab(&mut minter);
    let tab_id = tab.id;
    let tree = seed_tree(&mut minter, "Desk", vec![tab]);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(HostTiles));
    handle.set_viewport_width(1200.0);
    let mut driver = FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.set_caller_palette_enabled(false);
    driver.relayout();
    (driver, handle, tab_id)
}

/// The stream start the sender's announcement carries, as this desktop reads it: the same tab on
/// both devices, and whether it may type into the window.
fn stream_start(tab: TabId, writable: bool) -> PeerStreamStart {
    let announcement = NeighbourMsg::SurfaceStreamStart {
        surface: SURFACE,
        source_peer: SHARER,
        width: WIDTH,
        height: HEIGHT,
        codec: "vp9".into(),
        ts: 0,
        sender_monotonic_ns_at_send: 0,
        colorimetry: None,
        hdr_static_metadata: None,
        bit_depth: 0,
        app_id: APP.into(),
        tab,
    };
    PeerStreamStart::announced(&announcement, LOCAL, "Laptop", "Writer", writable)
        .expect("a stream start is what the announcement is")
}

/// The frames of `count` keyframes the crate's own VP9 encoder makes from a gradient, so what
/// arrives is a real stream rather than bytes shaped like one.
fn keyframes(count: usize) -> Vec<Vec<u8>> {
    let mut encoder =
        tilo_callstream::VideoEncoder::new(tilo_callstream::StreamCodec::Vp9, WIDTH, HEIGHT, 30)
            .expect("a VP9 encoder");
    let mut units = Vec::new();
    for step in 0..count {
        encoder.force_keyframe();
        for unit in encoder.encode(&gradient(step)).expect("an encoded frame") {
            units.push(unit.data);
        }
    }
    units
}

/// A gradient that moves with `step` , so each frame is a different picture.
fn gradient(step: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity((WIDTH as usize) * (HEIGHT as usize) * 4);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let shift = u8::try_from(step).unwrap_or(0);
            out.push(u8::try_from(x).unwrap_or(0).wrapping_add(shift));
            out.push(u8::try_from(y).unwrap_or(0));
            out.push(128);
            out.push(255);
        }
    }
    out
}

/// The resource the tile at `tab` publishes, once the desktop has published it.
fn published(driver: &FrameHarness, surface: SurfaceId) -> Option<tilo_actions::ResourceSpec> {
    let id = format!("desktop.peer-surface:{surface}");
    driver
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == id)
}

/// Drive frames until the tile has its picture, and say what it publishes then: decoding happens
/// off the frame the unit is pushed on, as in a host.
fn settle(driver: &mut FrameHarness, handle: &DesktopHandle, tab: TabId) -> PeerRead {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        driver.relayout();
        let read = handle.peer_surface_read(tab).expect("the share has a tile");
        if read.pictured() || Instant::now() >= deadline {
            return read;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Push `unit` at `tile` and settle its picture, as the desktop's frame passes do: a push never
/// waits, so a picture still decoding is collected once it is out. A window's paint is what the
/// tile has not had in between.
fn push_settled(tile: &Rc<RefCell<PeerSurface>>, seq: u64, unit: &[u8]) -> PeerOutcome {
    let outcome = tile.borrow_mut().push(seq, unit);
    if outcome != PeerOutcome::Pending {
        return outcome;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(outcome) = tile.borrow_mut().collect_late_picture() {
            return outcome;
        }
        if !tile.borrow().picture_pending() || Instant::now() >= deadline {
            return PeerOutcome::DecodeFailed;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_received_stream_is_pictured_in_its_tile() {
    let (mut driver, handle, tab) = desk();
    assert_eq!(
        handle.show_peer_surface(&stream_start(tab, true)),
        Some(tab),
        "the tab showing the peer's window takes its share"
    );
    driver.relayout();

    let before = handle.peer_surface_read(tab).expect("the share has a tile");
    assert_eq!(before.standing, Standing::Started);
    assert_eq!(
        before.picture, None,
        "nothing is pictured before the first unit"
    );
    let resource = published(&driver, SURFACE).expect("the share publishes its resource");
    assert_eq!(
        resource.presentation.as_ref().unwrap().preview.as_deref(),
        Some("Waiting for its first picture")
    );

    for (index, unit) in keyframes(1).into_iter().enumerate() {
        let seq = u64::try_from(index).unwrap_or(0).saturating_add(1);
        assert!(
            handle
                .peer_surface_picture(SHARER, SURFACE, seq, &unit)
                .is_some(),
            "the tile takes the stream's unit"
        );
    }

    let read = settle(&mut driver, &handle, tab);
    assert_eq!(read.standing, Standing::Pictured);
    assert_eq!(
        read.picture,
        Some((WIDTH, HEIGHT, (WIDTH as usize) * (HEIGHT as usize) * 4)),
        "the whole picture of the size the sender announced"
    );
    let resource = published(&driver, SURFACE).expect("the tile still publishes it");
    assert_eq!(
        resource.presentation.as_ref().unwrap().preview.as_deref(),
        Some("Live")
    );
}

#[test]
fn three_pushes_at_a_held_paint_leave_the_newest_picture_in_the_resource() {
    let mut minter = IdMinter::new(7);
    let tab = peer_tab(&mut minter);
    let tab_id = tab.id;
    let tree = seed_tree(&mut minter, "Desk", vec![tab]);
    let mut peers = PeerSurfaces::default();
    let tab = peers
        .start(&tree, &stream_start(tab_id, true))
        .expect("the peer's window has a tab");

    let tile = Rc::clone(peers.tile(tab).expect("the share has a tile"));
    let units = keyframes(3);
    assert_eq!(units.len(), 3, "one keyframe each");
    for (index, unit) in units.iter().enumerate() {
        let seq = u64::try_from(index).unwrap_or(0).saturating_add(1);
        let outcome = push_settled(&tile, seq, unit);
        assert_ne!(outcome, PeerOutcome::DecodeFailed, "a keyframe decodes");
        if index < 2 {
            assert_eq!(
                outcome,
                PeerOutcome::Decoded,
                "the tile held under its ceiling"
            );
        } else {
            assert_eq!(
                outcome,
                PeerOutcome::DroppedOldest,
                "the third drops the oldest"
            );
        }
    }

    let read = tile.borrow().read();
    assert_eq!(
        read.picture_seq, 3,
        "the newest unit is what the tile shows"
    );
    assert_eq!(read.frames_held, 2, "no more than the ceiling waits");
    assert_eq!(
        read.frames_dropped, 1,
        "exactly the one the ceiling evicted"
    );
    assert_eq!(read.standing, Standing::Pictured);
    let resource = tile.borrow().resource();
    assert_eq!(
        resource.presentation.as_ref().unwrap().preview.as_deref(),
        Some("Live")
    );
    assert_eq!(
        read.picture.map(|picture| (picture.0, picture.1)),
        Some((WIDTH, HEIGHT))
    );
}

#[test]
fn a_pointer_scroll_keys_and_text_reach_a_writable_share_and_a_read_only_one_refuses_them() {
    let (mut driver, handle, tab) = desk();
    handle.show_peer_surface(&stream_start(tab, true));
    driver.relayout();
    let id = format!("desktop.peer-surface:{SURFACE}");

    let motion = serde_json::Map::from_iter([
        ("x".to_string(), serde_json::Value::from(12.5)),
        ("y".to_string(), serde_json::Value::from(34.0)),
    ]);
    // A point on the picture is the sender's own pixel, whatever size this desktop drew the window
    // at.
    driver
        .invoke_published(&id, "pointer-motion", &motion)
        .expect("a writable share takes pointer motion");
    let button = serde_json::Map::from_iter([
        ("button".to_string(), serde_json::Value::from(272)),
        ("pressed".to_string(), serde_json::Value::from(true)),
    ]);
    driver
        .invoke_published(&id, "pointer-button", &button)
        .expect("a writable share takes a click");
    let key = serde_json::Map::from_iter([
        ("keysym".to_string(), serde_json::Value::from(30)),
        ("pressed".to_string(), serde_json::Value::from(true)),
        ("modifiers".to_string(), serde_json::Value::from(0)),
    ]);
    driver
        .invoke_published(&id, "key", &key)
        .expect("a writable share takes a keystroke");
    let scroll = serde_json::Map::from_iter([
        ("dx".to_string(), serde_json::Value::from(0.0)),
        ("dy".to_string(), serde_json::Value::from(45.0)),
    ]);
    driver
        .invoke_published(&id, "scroll", &scroll)
        .expect("a writable share takes a scroll");
    let text = serde_json::Map::from_iter([("text".to_string(), serde_json::Value::from("é"))]);
    driver
        .invoke_published(&id, "text", &text)
        .expect("a writable share takes composed text");

    let events: Vec<InputEvent> = handle
        .take_peer_outgoing()
        .into_iter()
        .map(|message| match message {
            NeighbourMsg::RemoteInput {
                peer,
                surface,
                event,
            } => {
                assert_eq!(
                    (peer, surface),
                    (LOCAL, SURFACE),
                    "input goes back to its owner"
                );
                postcard::from_bytes(&event).expect("an input event is what the wire carries")
            }
            _ => panic!("input is all the tile asks of a writable share"),
        })
        .collect();
    assert_eq!(
        events,
        vec![
            InputEvent::PointerMotion { x: 12.5, y: 34.0 },
            InputEvent::PointerButton {
                button: 272,
                pressed: true
            },
            InputEvent::Key {
                keysym: 30,
                pressed: true,
                modifiers: 0
            },
            InputEvent::PointerAxis { dx: 0.0, dy: 45.0 },
            InputEvent::Text { text: "é".into() },
        ]
    );

    let (mut driver, handle, tab) = desk();
    handle.show_peer_surface(&stream_start(tab, false));
    driver.relayout();
    driver
        .invoke_published(&id, "pointer-motion", &motion)
        .expect_err("a window shared for watching takes no pointer");
    driver
        .invoke_published(&id, "key", &key)
        .expect_err("a window shared for watching takes no keys");
    assert!(
        handle.take_peer_outgoing().is_empty(),
        "nothing is sent for input the share refuses"
    );
}

#[test]
fn ending_the_share_removes_the_tile_s_resource() {
    let (mut driver, handle, tab) = desk();
    handle.show_peer_surface(&stream_start(tab, true));
    driver.relayout();
    assert!(
        published(&driver, SURFACE).is_some(),
        "the live share publishes its tile"
    );

    assert_eq!(
        handle.end_peer_surface(SHARER, SURFACE, "the sender stopped sharing"),
        Some(tab)
    );
    driver.relayout();
    assert!(
        published(&driver, SURFACE).is_none(),
        "the ended share leaves nothing to drive"
    );
    assert!(
        handle.peer_surface_read(tab).is_none(),
        "the tab is the desktop's own window again"
    );
}
