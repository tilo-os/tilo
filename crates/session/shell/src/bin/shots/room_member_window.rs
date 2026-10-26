//! Another member's terminal, hosted on their device, as this device shows
//! it in a room that shares its layout: the tab is a live stream tile. The
//! host's share is real (software encoder) and the tile decodes what it
//! sends, so the scene shows what a member sees, not a picture of it.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "a fixture picture: divisors are nonzero cell constants, sums stay within its small size"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use shell::collab::flatpak_share::SharedFlatpakSession;
use shell::config::{EncoderChoice, FlatpakShareConfig};
use shell::network::NeighbourMsg;
use shell::shell::peer_view::PeerStreamStart;
use shell::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};

use super::ShellDriver;

const HOST: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const HOST_PEER: u64 = 0xA1;
const LOCAL_PEER: u64 = 0xB2;
const SURFACE: u64 = 31;
const WIDTH: u32 = 960;
const HEIGHT: u32 = 600;
/// A glyph cell of the host's terminal, in pixels.
const CELL: (u32, u32) = (9, 18);

/// What the host's terminal shows: a prompt, a command and its output, as
/// lines of glyph-sized marks on a dark ground.
fn terminal() -> Vec<u8> {
    // Per line: (indent, glyph runs separated by one cell, colour).
    let lines: &[(&[u32], [u8; 3])] = &[
        (&[4, 1, 6, 2], [120, 200, 120]),
        (&[3, 5, 8, 4, 10], [210, 210, 210]),
        (&[7, 4, 12, 3], [210, 210, 210]),
        (&[5, 9, 6], [210, 210, 210]),
        (&[11, 3, 7, 5], [210, 210, 210]),
        (&[], [0, 0, 0]),
        (&[4, 1, 6, 2], [120, 200, 120]),
        (&[3, 14], [210, 210, 210]),
        (&[6, 6, 6, 6, 6], [150, 170, 230]),
        (&[6, 6, 6, 6], [150, 170, 230]),
        (&[], [0, 0, 0]),
        (&[4, 1, 6, 2], [120, 200, 120]),
    ];
    let background = [22_u8, 24, 28];
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        let row = (y / CELL.1) as usize;
        let in_glyph = y % CELL.1 >= 4 && y % CELL.1 <= 14;
        for x in 0..WIDTH {
            let column = x / CELL.0;
            let mut colour = background;
            if in_glyph
                && x % CELL.0 >= 1
                && x % CELL.0 <= 7
                && let Some((runs, ink)) = lines.get(row)
            {
                let mut at = 1;
                for run in *runs {
                    if column >= at && column < at + run {
                        colour = *ink;
                    }
                    at += run + 1;
                }
                // The caret after the last prompt.
                if row + 1 == lines.len() && column == at {
                    colour = [230, 230, 230];
                }
            }
            rgba.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
    }
    rgba
}

/// The host's share behind the stream tile, kept by the driver between
/// the window opening and its first picture.
struct RoomMemberShare {
    session: SharedFlatpakSession,
}

impl ShellDriver {
    /// The active strip holds one window: another member's terminal, hosted
    /// on their laptop, streaming live into its tab.
    pub(super) fn scene_room_member_window(&mut self) {
        self.open_room_member_window();
        self.show_room_member_picture();
        self.step_frames(3);
    }

    /// Another member's terminal opens here as a stream tile whose pictures
    /// have not come yet: the place its window arrives in, on its own.
    pub(crate) fn open_room_member_window(&mut self) {
        let (group, tab) = (0x5EED_0000_0000_0101, 0x5EED_0000_0000_0102);
        self.with_chrome(|st| {
            let mut tree = st.tree_mut();
            let Some(workspace) = tree.desktop.active_workspace_mut() else {
                return;
            };
            let Some(strip) = workspace.strips.first_mut() else {
                return;
            };
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
        // The tab's document, as opening a window makes one: binding never
        // creates it.
        self.open_tab_records(&[tab]);
        self.step_frames(3);
        let Ok(session) = SharedFlatpakSession::start_with_metrics(
            HOST_PEER,
            "xterm",
            SURFACE,
            WIDTH,
            HEIGHT,
            &FlatpakShareConfig {
                max_bitrate_kbps: 8000,
                target_fps: 60,
                encoder: EncoderChoice::Software,
                hdr: None,
            },
            &[media::realtime::CodecCapabilities {
                software_decode: [media::StreamCodec::H264, media::StreamCodec::Vp9].into(),
                ..media::realtime::CodecCapabilities::default()
            }],
            None,
        ) else {
            eprintln!("room-member-window: no software encoder");
            return;
        };
        let session = session.with_tab(tab);
        session.add_viewer(LOCAL_PEER, true);
        let NeighbourMsg::SurfaceStreamStart {
            codec,
            width,
            height,
            app_id,
            ..
        } = session.stream_start_msg()
        else {
            return;
        };
        let mounted = self.fb.host_mut().mount_peer_stream(&PeerStreamStart {
            source_peer: HOST_PEER,
            surface: SURFACE,
            local_peer: LOCAL_PEER,
            app_id,
            codec,
            display_size: Some((width, height)),
            hdr_static_metadata: None,
            input_sink: Arc::new(|_| {}),
            keyframe_sink: Arc::new(|_| {}),
            tab,
        });
        if !mounted {
            eprintln!("room-member-window: the stream found no tab");
            return;
        }
        self.held.push(Box::new(RoomMemberShare { session }));
    }

    /// The terminal [`Self::open_room_member_window`] opened sends its first
    /// pictures, and the tile decodes them.
    pub(crate) fn show_room_member_picture(&mut self) {
        let Some(share) = self
            .held
            .iter()
            .rev()
            .find_map(|held| held.downcast_ref::<RoomMemberShare>())
        else {
            eprintln!("room-member-window: no window opened to show");
            return;
        };
        let session = &share.session;
        let picture = terminal();
        // A few changes, so the decoder has its keyframe and the picture
        // after it.
        let mut units = Vec::new();
        for _ in 0..4 {
            session.deposit_captured_rgba(picture.clone());
            if let Some(frame) = session.encode_next_frame(Instant::now())
                && let NeighbourMsg::SurfaceFrame {
                    encoded_h264, seq, ..
                } = frame.into_neighbour_msg()
            {
                units.push((seq, encoded_h264));
            }
        }
        for (seq, unit) in units {
            let _ = self
                .fb
                .host_mut()
                .push_peer_stream_frame(HOST_PEER, SURFACE, seq, &unit, None);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let _ = self.fb.host_mut().collect_late_peer_pictures();
            let shown = self
                .fb
                .host_mut()
                .peer_stream_view_mut(HOST_PEER, SURFACE)
                .is_some_and(|view| view.latest_frame().is_some());
            if shown {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
