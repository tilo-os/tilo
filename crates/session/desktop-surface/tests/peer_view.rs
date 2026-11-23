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
    DesktopHandle, DesktopSurface, Elsewhere, NeighbourMsg, PeerAction, PeerId, PeerOutcome,
    PeerRead, PeerStreamStart, PeerSurface, PeerSurfaces, StreamState, SurfaceId, TileSource,
    WhyEnded,
};
use tilo_session_core::desktop::seed_tree;
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

/// A host with nothing to draw for a window that runs on another device, as the native shell's own
/// tile source has: it answers with the place of a window still to come, which paints nothing.
struct NothingToDraw;

impl TileSource for NothingToDraw {
    fn tile(&mut self, _workspace: &Workspace, _tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Arriving)
    }
}

/// A desktop showing one window another device shares, in the toolkit's own frame pipeline.
fn shell() -> (FrameHarness, DesktopHandle, TabId) {
    let mut minter = IdMinter::new(7);
    let tab = peer_tab(&mut minter);
    shell_showing(minter, tab, Box::new(HostTiles))
}

/// A desktop whose one window runs on the device the shell calls `device` , over a host that has
/// nothing to draw for it.
fn shell_of_device(device: &str) -> (FrameHarness, DesktopHandle, TabId) {
    let mut minter = IdMinter::new(7);
    let mut tab = peer_tab(&mut minter);
    if let ModelTile::HostedWindow { host_label, .. } = &mut tab.tile {
        device.clone_into(host_label);
    }
    shell_showing(minter, tab, Box::new(NothingToDraw))
}

/// A desktop showing `tab` over `tiles` , in the toolkit's own frame pipeline.
fn shell_showing(
    mut minter: IdMinter,
    tab: Tab,
    tiles: Box<dyn TileSource>,
) -> (FrameHarness, DesktopHandle, TabId) {
    let tab_id = tab.id;
    let tree = seed_tree(&mut minter, "Office", vec![tab]);
    let (surface, handle) = DesktopSurface::new(tree, 7, tiles);
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
    let (mut driver, handle, tab) = shell();
    assert_eq!(
        handle.show_peer_surface(&stream_start(tab, true)),
        Some(tab),
        "the tab showing the peer's window takes its share"
    );
    driver.relayout();

    let before = handle.peer_surface_read(tab).expect("the share has a tile");
    assert_eq!(before.state, StreamState::Started);
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
    assert_eq!(read.state, StreamState::Pictured);
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
    let texts = ui::a11y::texts(driver.root_widget());
    assert!(
        texts.iter().any(|text| text == "Writer on Laptop"),
        "the picture is named by the window it shows and the device it runs on: {texts:?}"
    );
}

/// What the tile publishes of itself is read in the language of the person reading it: whose
/// window it shows, whether this device may type into it, and how its stream stands. None of it
/// is a sentence built in code.
#[test]
fn a_tile_says_whose_window_it_is_and_how_it_stands_in_the_readers_language() {
    let mut minter = IdMinter::new(7);
    let tab = peer_tab(&mut minter);
    let tab_id = tab.id;
    let tree = seed_tree(&mut minter, "Shell", vec![tab]);
    let mut peers = PeerSurfaces::default();
    let tab = peers
        .start(&tree, &stream_start(tab_id, false))
        .expect("the peer's window has a tab");
    let tile = Rc::clone(peers.tile(tab).expect("the share has a tile"));
    let said = || {
        let presentation = tile
            .borrow()
            .resource()
            .presentation
            .expect("the tile says what it shows");
        [
            presentation.detail,
            presentation.badge,
            presentation.preview,
        ]
        .map(Option::unwrap_or_default)
    };
    let in_german = || {
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = said();
        ui::i18n::I18nManager::set_locale("en-US");
        german
    };

    assert_eq!(
        said(),
        [
            "Laptop's window",
            "View only",
            "Waiting for its first picture"
        ]
    );
    assert_eq!(
        in_german(),
        [
            "Fenster von Laptop",
            "Nur ansehen",
            "Wartet auf das erste Bild"
        ]
    );

    // A share that ended says why, to each reader in their own language: it keeps the reason, not
    // the sentence it was in when it ended.
    tile.borrow_mut().end(WhyEnded::SenderStopped);
    assert_eq!(said()[2], "Ended: the sender stopped sharing it");
    assert_eq!(in_german()[2], "Beendet: der Absender teilt es nicht mehr");
}

#[test]
fn three_pushes_at_a_held_paint_leave_the_newest_picture_in_the_resource() {
    let mut minter = IdMinter::new(7);
    let tab = peer_tab(&mut minter);
    let tab_id = tab.id;
    let tree = seed_tree(&mut minter, "Office", vec![tab]);
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
    assert_eq!(read.state, StreamState::Pictured);
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
    let (mut driver, handle, tab) = shell();
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

    let (mut driver, handle, tab) = shell();
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
    let (mut driver, handle, tab) = shell();
    handle.show_peer_surface(&stream_start(tab, true));
    driver.relayout();
    assert!(
        published(&driver, SURFACE).is_some(),
        "the live share publishes its tile"
    );

    assert_eq!(
        handle.end_peer_surface(SHARER, SURFACE, WhyEnded::SenderStopped),
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

/// What the shell's tab says of a window that is not shown in it: the notice's title, and the
/// sentence under it, as a reader who cannot see the shell and a tool listing it read them.
fn notice(driver: &FrameHarness) -> Option<(String, String)> {
    driver.a11y_nodes().into_iter().find_map(|(_, node)| {
        let title = node.label()?;
        (title.contains(" runs on ") || title.contains(" läuft auf ")).then(|| {
            (
                title.to_owned(),
                node.description().unwrap_or_default().to_owned(),
            )
        })
    })
}

/// The notice `driver` 's shell shows once `handle` was told the window in `tab` stands as
/// `state` .
fn notice_for(
    driver: &mut FrameHarness,
    handle: &DesktopHandle,
    tab: TabId,
    state: &Elsewhere,
) -> Option<(String, String)> {
    handle.set_elsewhere(vec![(tab, state.clone())]);
    driver.relayout();
    driver.relayout();
    assert_eq!(
        &handle.elsewhere(tab),
        state,
        "the shell keeps what it was told"
    );
    notice(driver)
}

/// A notice's title and the sentence under it.
fn words(title: &str, why: &str) -> (String, String) {
    (title.to_owned(), why.to_owned())
}

/// A window that runs on another device and is not shown here never leaves its tab empty. Its
/// host has nothing to draw for it, so the tab says where the window runs, and under that why it
/// is not here, as the session read it: not shared, shared and still on its way, on a device
/// nothing is heard of, or on a device the workspace no longer admits. A shell nobody told says
/// only where it runs.
#[test]
fn a_window_on_another_device_says_where_it_runs_and_why_it_is_not_here() {
    let (mut driver, handle, tab) = shell_of_device("Laptop");
    assert_eq!(
        notice(&driver),
        Some(words("Writer runs on Laptop", "")),
        "a shell nobody told says where the window runs, and no more"
    );
    let waits_for_keys = "Waiting for this workspace's keys to reach this device";
    for (state, why) in [
        (
            Elsewhere::NotShared,
            "It is not shared with this workspace right now",
        ),
        (
            Elsewhere::Awaited {
                waiting_for: String::new(),
            },
            "Waiting for Laptop to send it",
        ),
        (
            Elsewhere::Awaited {
                waiting_for: waits_for_keys.to_owned(),
            },
            waits_for_keys,
        ),
        (
            Elsewhere::Away,
            "Laptop is not connected to this workspace right now",
        ),
        (Elsewhere::Left, "Laptop is no longer in this workspace"),
    ] {
        assert_eq!(
            notice_for(&mut driver, &handle, tab, &state),
            Some(words("Writer runs on Laptop", why))
        );
    }
    // A tab the session says nothing of any more says only where its window runs again.
    handle.set_elsewhere(Vec::new());
    driver.relayout();
    assert_eq!(notice(&driver), Some(words("Writer runs on Laptop", "")));
}

/// A device nothing names is another device of the workspace: the tab never shows its key, nor
/// a blank where its name would be.
#[test]
fn a_device_nothing_names_is_another_device_of_the_workspace_and_never_its_key() {
    let key = "ab".repeat(32);
    for label in ["", "  ", key.as_str()] {
        let (mut driver, handle, tab) = shell_of_device(label);
        let title = "Writer runs on another device of this workspace";
        for (state, why) in [
            (
                Elsewhere::NotShared,
                "It is not shared with this workspace right now",
            ),
            (
                Elsewhere::Awaited {
                    waiting_for: String::new(),
                },
                "Waiting for its device to send it",
            ),
            (
                Elsewhere::Away,
                "Its device is not connected to this workspace right now",
            ),
            (Elsewhere::Left, "Its device is no longer in this workspace"),
        ] {
            assert_eq!(
                notice_for(&mut driver, &handle, tab, &state),
                Some(words(title, why)),
                "a device labelled {label:?}"
            );
        }
        for (_, node) in driver.a11y_nodes() {
            for said in [node.label(), node.description(), node.value()] {
                assert!(
                    !said.unwrap_or_default().contains(&key),
                    "the shell shows a device's key: {said:?}"
                );
            }
        }
    }
}

/// The tab reads in the language of the person reading it now: a notice built in one language is
/// built again when they choose another.
#[test]
fn a_window_on_another_device_is_said_in_the_language_the_person_reads_now() {
    let (mut driver, handle, tab) = shell_of_device("Laptop");
    assert_eq!(
        notice_for(&mut driver, &handle, tab, &Elsewhere::NotShared),
        Some(words(
            "Writer runs on Laptop",
            "It is not shared with this workspace right now"
        ))
    );
    ui::i18n::I18nManager::set_locale("de-DE");
    driver.relayout();
    driver.relayout();
    let german = notice(&driver);
    ui::i18n::I18nManager::set_locale("en-US");
    driver.relayout();
    driver.relayout();
    assert_eq!(
        german,
        Some(words(
            "Writer läuft auf Laptop",
            "Es wird gerade nicht mit diesem Arbeitsbereich geteilt"
        ))
    );
    assert_eq!(
        notice(&driver),
        Some(words(
            "Writer runs on Laptop",
            "It is not shared with this workspace right now"
        ))
    );
}

/// A share takes the tab from the notice, and gives it back when its sender stops: starting, the
/// tile shows the window arriving and no notice; going out, the window's picture; ended by its
/// sender, the notice again, saying what the session reads of the window now.
#[test]
fn a_share_takes_the_tab_from_the_notice_and_gives_it_back_when_its_sender_stops() {
    let (mut driver, handle, tab) = shell_of_device("Laptop");
    let not_shared = Some(words(
        "Writer runs on Laptop",
        "It is not shared with this workspace right now",
    ));
    assert_eq!(
        notice_for(&mut driver, &handle, tab, &Elsewhere::NotShared),
        not_shared
    );

    // Starting: the stream is announced and nothing of the window is here.
    assert_eq!(
        handle.show_peer_surface(&stream_start(tab, true)),
        Some(tab)
    );
    driver.relayout();
    assert_eq!(
        handle.peer_surface_read(tab).map(|read| read.state),
        Some(StreamState::Started)
    );
    assert_eq!(
        notice(&driver),
        None,
        "a share that started shows the window arriving, not where it runs"
    );
    assert!(
        published(&driver, SURFACE).is_some(),
        "the arriving window's tile is the share's"
    );

    // Going out: its picture is in the tile.
    for (index, unit) in keyframes(1).into_iter().enumerate() {
        let seq = u64::try_from(index).unwrap_or(0).saturating_add(1);
        handle.peer_surface_picture(SHARER, SURFACE, seq, &unit);
    }
    assert_eq!(
        settle(&mut driver, &handle, tab).state,
        StreamState::Pictured
    );
    driver.relayout();
    assert_eq!(notice(&driver), None, "a pictured window shows its picture");
    assert!(
        ui::a11y::texts(driver.root_widget())
            .iter()
            .any(|text| text == "Writer on Laptop"),
        "the picture is named by its window and device"
    );

    // Ended by its sender: the tab is not left with nothing in it.
    assert_eq!(
        handle.end_peer_surface(SHARER, SURFACE, WhyEnded::SenderStopped),
        Some(tab)
    );
    driver.relayout();
    driver.relayout();
    assert_eq!(notice(&driver), not_shared);
    assert!(published(&driver, SURFACE).is_none());
}

/// A window this device let go is still shared by its device, so its tab does not say it is not
/// shared: it says the share ended here and why, in the language read now, shows no picture that
/// no longer follows the window, and refuses what is asked of it in that language too.
#[test]
fn a_window_this_device_let_go_says_so_and_refuses_in_the_language_read_now() {
    let (mut driver, handle, tab) = shell_of_device("Laptop");
    handle.set_elsewhere(vec![(tab, Elsewhere::NotShared)]);
    handle.show_peer_surface(&stream_start(tab, true));
    for (index, unit) in keyframes(1).into_iter().enumerate() {
        let seq = u64::try_from(index).unwrap_or(0).saturating_add(1);
        handle.peer_surface_picture(SHARER, SURFACE, seq, &unit);
    }
    assert_eq!(
        settle(&mut driver, &handle, tab).state,
        StreamState::Pictured
    );
    let id = format!("desktop.peer-surface:{SURFACE}");
    driver
        .invoke_published(&id, "end", &serde_json::Map::new())
        .expect("the tile lets the window go");
    driver.relayout();
    driver.relayout();
    assert_eq!(
        handle.peer_surface_read(tab).map(|read| read.state),
        Some(StreamState::Ended {
            why: WhyEnded::LetGo
        })
    );
    assert_eq!(
        notice(&driver),
        Some(words(
            "Writer runs on Laptop",
            "Ended: this device let the window go"
        ))
    );
    assert!(
        !ui::a11y::texts(driver.root_widget())
            .iter()
            .any(|text| text == "Writer on Laptop"),
        "an ended share shows no picture of the window"
    );
    let motion = serde_json::Map::from_iter([
        ("x".to_string(), serde_json::Value::from(1.0)),
        ("y".to_string(), serde_json::Value::from(1.0)),
    ]);
    let refusal = |driver: &mut FrameHarness| {
        let refused = driver
            .invoke_published(&id, "pointer-motion", &motion)
            .expect_err("an ended share takes no pointer");
        format!("{refused:?}")
    };
    let english = refusal(&mut driver);
    assert!(
        english.contains("This window's share has ended"),
        "{english}"
    );

    ui::i18n::I18nManager::set_locale("de-DE");
    driver.relayout();
    driver.relayout();
    let german = (notice(&driver), refusal(&mut driver));
    ui::i18n::I18nManager::set_locale("en-US");
    assert_eq!(
        german.0,
        Some(words(
            "Writer läuft auf Laptop",
            "Beendet: dieses Gerät hat das Fenster losgelassen"
        ))
    );
    assert!(
        german.1.contains("Dieses Fenster wird nicht mehr geteilt"),
        "{}",
        german.1
    );
}

/// What can be done to another device's window is listed, and refused, in the language of the
/// person reading it: no action's name, no word of what it asks for and no refusal is a sentence
/// written in code.
#[test]
fn a_tiles_actions_are_named_and_refused_in_the_readers_language() {
    let mut minter = IdMinter::new(7);
    let tab = peer_tab(&mut minter);
    let tab_id = tab.id;
    let tree = seed_tree(&mut minter, "Shell", vec![tab]);
    let mut peers = PeerSurfaces::default();
    let tab = peers
        .start(&tree, &stream_start(tab_id, false))
        .expect("the peer's window has a tab");
    let tile = Rc::clone(peers.tile(tab).expect("the share has a tile"));
    let in_german = |read: &dyn Fn() -> Vec<String>| {
        ui::i18n::I18nManager::set_locale("de-DE");
        let german = read();
        ui::i18n::I18nManager::set_locale("en-US");
        german
    };

    let names = || -> Vec<String> {
        tile.borrow()
            .resource()
            .actions
            .iter()
            .map(|action| action.label.clone())
            .collect()
    };
    assert_eq!(
        names(),
        [
            "Move the pointer",
            "Press a pointer button",
            "Press a key",
            "Scroll",
            "Type text",
            "Ask for the whole picture again",
            "Let the window go",
        ]
    );
    assert_eq!(
        in_german(&names),
        [
            "Zeiger bewegen",
            "Zeigertaste drücken",
            "Taste drücken",
            "Scrollen",
            "Text eingeben",
            "Das ganze Bild erneut anfordern",
            "Das Fenster loslassen",
        ]
    );
    // What the first of them asks for, and what each thing it is told means.
    let asks = || -> Vec<String> {
        let resource = tile.borrow().resource();
        let motion = resource.actions.first().expect("the tile's first action");
        motion
            .params()
            .iter()
            .map(|param| param.description.to_string())
            .collect()
    };
    assert_eq!(
        asks(),
        [
            "How far across the window, in its own pixels",
            "How far down the window, in its own pixels",
        ]
    );
    assert_eq!(
        in_german(&asks),
        [
            "Wie weit quer über das Fenster, in dessen eigenen Pixeln",
            "Wie weit das Fenster hinunter, in dessen eigenen Pixeln",
        ]
    );

    let motion = serde_json::Map::from_iter([
        ("x".to_string(), serde_json::Value::from(1.0)),
        ("y".to_string(), serde_json::Value::from(1.0)),
    ]);
    let nothing = serde_json::Map::new();
    let refused = |action: PeerAction, arguments: &serde_json::Map<String, serde_json::Value>| {
        tile.borrow_mut()
            .act(action, arguments)
            .expect_err("the tile refuses it")
    };
    // A window shared for watching, an argument that is not what the action takes, and a share
    // that ended.
    let watching_only = || vec![refused(PeerAction::PointerMotion, &motion)];
    assert_eq!(watching_only(), ["This window is shared for watching only"]);
    assert_eq!(
        in_german(&watching_only),
        ["Dieses Fenster ist nur zum Ansehen geteilt"]
    );
    let not_a_number = || vec![refused(PeerAction::PointerMotion, &nothing)];
    assert_eq!(not_a_number(), ["“x” is not a number"]);
    assert_eq!(in_german(&not_a_number), ["„x“ ist keine Zahl"]);
    let no_text = || vec![refused(PeerAction::Text, &nothing)];
    assert_eq!(no_text(), ["There is no text to type"]);
    assert_eq!(in_german(&no_text), ["Es gibt keinen Text einzugeben"]);
    tile.borrow_mut().end(WhyEnded::SenderStopped);
    let ended = || vec![refused(PeerAction::PointerMotion, &motion)];
    assert_eq!(ended(), ["This window's share has ended"]);
    assert_eq!(
        in_german(&ended),
        ["Dieses Fenster wird nicht mehr geteilt"]
    );
}
