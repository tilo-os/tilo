#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "in a test these are the assertion; fixture sizes are small; the frame builder lives for the whole scenario, which reads top to bottom"
)]

use std::sync::Arc;
use std::time::Instant;

use automerge::{AutoCommit, ROOT};

use crate::collab::flatpak_share::SharedFlatpakSession;
use crate::config::{EncoderChoice, FlatpakShareConfig};
use crate::network::protocol::NeighbourMsg;
use crate::shell::frame::FrameBuilder;
use crate::shell::peer_view::PeerStreamStart;
use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth, next_id};

/// The host member's device key, as the room's document names it.
const HOST: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
/// The host's peer handle.
const HOST_PEER: u64 = 0xA1;
/// This device's peer handle.
const LOCAL_PEER: u64 = 0xB2;
const SURFACE: u64 = 31;
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;

/// A terminal-like picture whose caret stands at a place `index` moves.
fn terminal(index: u32) -> Vec<u8> {
    let caret = (index * 9) % WIDTH;
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let text = (y / 18) % 2 == 0 && x % 11 < 7 && x < 420;
            let shade: u8 = if x.abs_diff(caret) < 2 && y < 18 {
                230
            } else if text {
                200
            } else {
                24
            };
            rgba.extend_from_slice(&[shade, shade, shade, 255]);
        }
    }
    rgba
}

fn settle(fb: &mut FrameBuilder) {
    for _ in 0..3 {
        let mut doc = AutoCommit::new();
        fb.pump_apps(&mut doc, &ROOT);
        fb.relayout();
        let _ = fb.paint();
    }
}

#[test]
fn another_members_host_window_is_a_live_stream_tile() {
    let mut fb = FrameBuilder::new((1600, 900));
    let (group, tab) = (next_id(), next_id());
    fb.with_chrome(|st| {
        let mut tree = st.tree_mut();
        let room = tree.desktop.active_workspace().expect("a workspace").room();
        assert!(
            !tree.room_layout(room).is_personal(),
            "a fresh room shares its layout"
        );
        let workspace = tree.desktop.active_workspace_mut().expect("a workspace");
        let strip = workspace.strips.first_mut().expect("a strip");
        strip.window_groups.clear();
        strip.window_groups.push(WindowGroup {
            id: group,
            tabs: vec![Tab {
                id: tab,
                title: "Terminal".into(),
                tile: TileContent::HostedWindow {
                    app_id: "xterm".into(),
                    label: "Terminal".into(),
                    host: HOST.into(),
                    host_label: "Ana's laptop".into(),
                },
                app_title: None,
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let strip_id = strip.id;
        tree.desktop.set_active_group(strip_id, group);
        drop(tree);
        st.bump();
    });
    settle(&mut fb);

    // The host's share of the window, encoding in software.
    let session = SharedFlatpakSession::start_with_metrics(
        HOST_PEER,
        "xterm",
        SURFACE,
        WIDTH,
        HEIGHT,
        &FlatpakShareConfig {
            max_bitrate_kbps: 4000,
            target_fps: 60,
            encoder: EncoderChoice::Software,
            hdr: None,
        },
        &[media::realtime::CodecCapabilities {
            software_decode: [media::StreamCodec::H264, media::StreamCodec::Vp9].into(),
            ..media::realtime::CodecCapabilities::default()
        }],
        None,
    )
    .expect("a software encoder")
    .with_tab(tab);
    session.add_viewer(LOCAL_PEER, true);
    let NeighbourMsg::SurfaceStreamStart {
        codec,
        width,
        height,
        app_id,
        tab: named,
        ..
    } = session.stream_start_msg()
    else {
        panic!("a stream start");
    };
    assert_eq!(named, tab, "the stream names the window's tab");
    let mounted = fb.host_mut().mount_peer_stream(&PeerStreamStart {
        source_peer: HOST_PEER,
        surface: SURFACE,
        local_peer: LOCAL_PEER,
        app_id,
        codec,
        display_size: Some((width, height)),
        hdr_static_metadata: None,
        input_sink: Arc::new(|_| {}),
        keyframe_sink: Arc::new(|_| {}),
        tab: named,
    });
    assert!(mounted, "the stream mounts in the window's tab");
    assert!(fb.host().has_peer_stream(HOST_PEER, SURFACE));
    let view = fb
        .host_mut()
        .peer_stream_view_mut(HOST_PEER, SURFACE)
        .expect("the tab shows the stream");
    assert!(
        view.latest_frame().is_none(),
        "nothing before the host encodes"
    );

    // The window changes; each change is encoded and shown.
    let mut decoded = 0;
    for index in 0..30 {
        // The window changes no faster than a software encoder's frame
        // rate.
        std::thread::sleep(std::time::Duration::from_millis(34));
        session.deposit_captured_rgba(terminal(index));
        let frame = session
            .encode_next_frame(Instant::now())
            .expect("a changed window is encoded");
        let NeighbourMsg::SurfaceFrame {
            encoded_h264, seq, ..
        } = frame.into_neighbour_msg()
        else {
            panic!("a frame");
        };
        if fb
            .host_mut()
            .push_peer_stream_frame(HOST_PEER, SURFACE, seq, &encoded_h264, None)
        {
            decoded += 1;
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(34));
    assert!(
        session.encode_next_frame(Instant::now()).is_none(),
        "a window that did not change is not encoded again"
    );
    let shown = tilo_test_env::wait_for("the tile shows a picture", || {
        let _ = fb.host_mut().collect_late_peer_pictures();
        fb.host_mut()
            .peer_stream_view_mut(HOST_PEER, SURFACE)
            .and_then(|view| view.latest_frame().map(|frame| (frame.width, frame.height)))
    });
    assert!(decoded > 0, "the tile decodes the stream");
    assert_eq!(
        shown,
        (WIDTH, HEIGHT),
        "the tile shows just the window, at its size"
    );
    settle(&mut fb);
}
