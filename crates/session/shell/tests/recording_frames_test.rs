//! The pixel layer of a feedback recording, headless and through the
//! production chain: a foreign client's committed buffer is composited into
//! its tab, the recorder records the room with pixels kept, the capture
//! executor (the same tick the compositor runs every frame) renders that
//! surface alone and hands it to the recorder, and stopping seals a
//! `session` record whose `frames` blob holds exactly that surface's
//! pixels, captured again once the client drew something new.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion or the fixture, not a hazard"
)]

use std::time::{Duration, Instant};

use session_trace::divergence::Tolerance;
use session_trace::frames::{FrameStrip, PixelPurpose, PixelRect};
use shell::compositor::capture::CaptureExecutor;
use shell::compositor::foreign::{self, store};
use shell::compositor::frame::FrameBuilder;
use shell::compositor::tab_binding;
use shell::shell::state::{RoomId, WorkspaceTree};

const W: u32 = 1280;
const H: u32 = 800;
const APP: &str = "so.tilo.notes";
const ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x5e55_1011_f4a3_4c0d_9a11_0000_0000_0002);
const OWNER: &str = "owner-of-the-recorded-room";

fn frame(fb: &mut FrameBuilder) -> vello::Scene {
    let mut doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;
    fb.pump_apps(&mut doc, &root);
    fb.paint_settled()
}

/// Paint until the adopted client is presented and its hand-over from
/// the opening card is over.
fn present(fb: &mut FrameBuilder, surface_id: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !store().is_presented(surface_id) && Instant::now() < deadline {
        let _ = fb.any_tile_opening();
        let _ = frame(fb);
        std::thread::sleep(Duration::from_millis(8));
    }
    assert!(
        store().is_presented(surface_id),
        "the client never presented"
    );
    let handover = Instant::now() + Duration::from_millis(250);
    while Instant::now() < handover {
        let _ = frame(fb);
        std::thread::sleep(Duration::from_millis(8));
    }
}

fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    [rgb[0], rgb[1], rgb[2], 0xff].repeat((width * height) as usize)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a laid-out tile edge is a small positive pixel count"
)]
fn px(value: f64) -> u32 {
    value.round() as u32
}

/// A frame builder whose rendered tree holds the client's tab, focused,
/// in a workspace that is room `ROOM`, administered by `OWNER`.
fn desktop_with_client(surface_id: u64) -> FrameBuilder {
    let mut protocol = WorkspaceTree::bootstrap_demo();
    tab_binding::adopt_toplevel(&mut protocol, surface_id, Some(APP.to_string()));
    let mut fb = FrameBuilder::new((W, H));
    {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        assert!(foreign::reconcile_foreign_tabs(&protocol, &mut rendered));
        let (workspace_index, strip_id, group_id) = rendered
            .desktop
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(index, workspace)| {
                workspace.strips.iter().flat_map(move |strip| {
                    strip
                        .window_groups
                        .iter()
                        .map(move |g| (index, strip.id, g))
                })
            })
            .find(|(_, _, group)| {
                group.tabs.iter().any(|tab| {
                    matches!(
                        &tab.tile,
                        shell::shell::state::TileContent::Foreign { surface_id: s, .. }
                            if *s == surface_id
                    )
                })
            })
            .map(|(index, strip, group)| (index, strip, group.id))
            .expect("the client's group is in the rendered tree");
        rendered.desktop.set_active_group(strip_id, group_id);
        let workspace = &mut rendered.desktop.workspaces[workspace_index];
        workspace.room = RoomId::parse(&ROOM.to_string()).unwrap();
        workspace.set_owner(OWNER);
    }
    fb.with_chrome(shell::shell::chrome_state::ChromeState::bump);
    fb
}

/// The frame strip a pending session's record references as its `frames`
/// blob, checked against the reference.
fn referenced_frames(pending: &shell::shell::recorder::PendingSession) -> FrameStrip {
    let blob = pending.frame_blob().expect("the session kept frames");
    let references = ui::record::blob_references(pending.document());
    let reference = references
        .iter()
        .find(|reference| reference.relation == session_trace::FRAMES_BLOB_RELATION)
        .expect("the record references its frames");
    assert_eq!(reference.hash, blake3::hash(blob).to_hex().to_string());
    assert_eq!(reference.len, blob.len() as u64);
    assert_eq!(reference.mime, session_trace::frames::FRAMES_MIME);

    FrameStrip::decode(blob).unwrap()
}

/// Each kept frame is the client's tile alone, in the colour it drew, and
/// flagged as unexplained because no Inspect keyframe describes it.
fn assert_frames_show_the_client(strip: &FrameStrip, (width, height): (u32, u32)) {
    for (stored, rgb) in strip
        .frames
        .iter()
        .zip([[0x19, 0xC8, 0x5A], [0xD0, 0x30, 0x40]])
    {
        assert_eq!(stored.route.app_id, APP);
        assert_eq!(stored.route.room, ROOM.to_string());
        assert_eq!(
            (stored.width, stored.height),
            (width, height),
            "the tile alone, no chrome"
        );
        // No Inspect keyframe describes this client: its pixels are
        // flagged for review, and nothing is guessed at or masked.
        assert!(stored.masked.is_empty());
        assert_eq!(
            stored.unexplained,
            vec![PixelRect::new(0, 0, width, height)]
        );
        let pixels = stored.decode().unwrap();
        let centre = pixels.pixel(width / 2, height / 2).unwrap();
        for (got, want) in centre.iter().zip(rgb) {
            assert!(
                got.abs_diff(want) <= 4,
                "captured {centre:?}, the client drew {rgb:?}"
            );
        }
    }
}

#[test]
fn recording_with_pixels_keeps_the_recorded_surface_as_a_frames_blob() {
    let surface_id = 0x5EED_F4A3_0001;
    store().forget(surface_id);
    let mut fb = desktop_with_client(surface_id);
    for _ in 0..4 {
        let _ = frame(&mut fb);
    }
    let rect = fb
        .foreign_surface_rect(surface_id)
        .expect("the client's tile is laid out");
    let (width, height) = (px(rect.width()), px(rect.height()));
    store().publish_rgba(
        surface_id,
        &solid(width, height, [0x19, 0xC8, 0x5A]),
        width,
        height,
    );
    present(&mut fb, surface_id);

    let dir = tempfile::tempdir().unwrap();
    let broker = shell::shell::collab_broker::spawn_native_harness(
        dir.path().join("collab.sock"),
        fb.chrome().tree_handle(),
        Vec::new(),
    )
    .unwrap();
    fb.host_mut().attach_collab_broker(&broker);
    let recorder = fb.chrome().recorder().cloned().expect("recorder attached");
    let owner = OWNER.to_string();
    recorder
        .start(
            &[ROOM.to_string()],
            vec![APP.to_string()],
            Some(PixelPurpose::Keep),
            &owner,
        )
        .unwrap();
    assert_eq!(
        recorder.pixel_request().map(|request| request.purpose),
        Some(PixelPurpose::Keep)
    );

    let mut executor = CaptureExecutor::default();
    let mut scene = frame(&mut fb);
    let _ = executor.execute_ready(&mut fb, &mut scene);
    assert_eq!(
        recorder.status_json()["recording"]["frames"],
        serde_json::json!(1),
        "the recorded surface is captured as soon as it is followed"
    );
    // Nothing changed and the base interval has not passed: no new frame.
    let mut scene = frame(&mut fb);
    let _ = executor.execute_ready(&mut fb, &mut scene);
    assert_eq!(
        recorder.status_json()["recording"]["frames"],
        serde_json::json!(1)
    );

    // The client draws: damage captures it again.
    store().publish_rgba(
        surface_id,
        &solid(width, height, [0xD0, 0x30, 0x40]),
        width,
        height,
    );
    std::thread::sleep(Duration::from_millis(250));
    present(&mut fb, surface_id);
    let mut scene = frame(&mut fb);
    let _ = executor.execute_ready(&mut fb, &mut scene);
    assert_eq!(
        recorder.status_json()["recording"]["frames"],
        serde_json::json!(2)
    );

    let checkout = recorder.stop(&owner).unwrap();
    let pending = recorder
        .take_session(&checkout.id)
        .expect("session awaits review");
    let strip = referenced_frames(&pending);
    assert_eq!(&strip, pending.frames().unwrap());
    assert_eq!(strip.purpose, PixelPurpose::Keep);
    assert_eq!(strip.frames.len(), 2);
    assert!(strip.frames[0].at < strip.frames[1].at);
    let trace = pending.trace().unwrap();
    assert!(
        strip.frames[1].at <= trace.duration,
        "frames are on the trace's clock"
    );
    assert_frames_show_the_client(&strip, (width, height));

    // Replay reads frames through the same render path: the surface as it
    // looks now is exactly the last frame the recording kept.
    let now = CaptureExecutor::room_surface_frames(&mut fb, &ROOM.to_string(), trace.duration);
    assert_eq!(now.len(), 1);
    let now = now.into_iter().next().unwrap().unwrap();
    let last = strip.frames[1].decode().unwrap();
    assert!(now.same_surface(&last.route));
    assert!(
        !session_trace::divergence::difference(&last, &now, &Tolerance::default()).contains(&true),
        "an unchanged surface replays pixel for pixel"
    );
    store().forget(surface_id);
    drop(broker);
}
