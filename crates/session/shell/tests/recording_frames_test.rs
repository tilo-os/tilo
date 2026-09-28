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
    desktop_with(surface_id, APP, OWNER)
}

/// [`desktop_with_client`] for any app, in a room `owner` administers.
fn desktop_with(surface_id: u64, app_id: &str, owner: &str) -> FrameBuilder {
    let mut protocol = WorkspaceTree::bootstrap_demo();
    tab_binding::adopt_toplevel(&mut protocol, surface_id, Some(app_id.to_string()));
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
        workspace.set_owner(owner);
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

#[allow(
    clippy::too_many_lines,
    reason = "one recording, from the first frame to the stored blob, asserted as one scenario"
)]
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
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let broker = shell::shell::collab_broker::spawn_native_harness(
        dir.path().join("collab.sock"),
        fb.chrome().tree_handle(),
        Vec::new(),
    )
    .unwrap();
    fb.host_mut().attach_collab_broker(&broker);
    let recorder = fb
        .host()
        .session_services()
        .recorder()
        .cloned()
        .expect("recorder attached");
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
    let now = CaptureExecutor::room_surface_frames(&mut fb, &ROOM.to_string(), trace.duration, 1.0);
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

/// The tile of `surface_id` once laid out, in whole logical pixels.
fn laid_out(fb: &mut FrameBuilder, surface_id: u64) -> (u32, u32) {
    for _ in 0..4 {
        let _ = frame(fb);
    }
    let rect = fb
        .foreign_surface_rect(surface_id)
        .expect("the client's tile is laid out");
    (px(rect.width()), px(rect.height()))
}

#[test]
fn a_scaled_surface_is_kept_at_the_pixels_it_renders() {
    let surface_id = 0x5EED_F4A3_0002;
    store().forget(surface_id);
    let mut fb = desktop_with_client(surface_id);
    let (width, height) = laid_out(&mut fb, surface_id);
    // On a scale-2 output the client commits a buffer twice the tile.
    store().publish_rgba(
        surface_id,
        &solid(width * 2, height * 2, [0x30, 0x60, 0xD0]),
        width * 2,
        height * 2,
    );
    present(&mut fb, surface_id);

    let dir = tempfile::tempdir().unwrap();
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let broker = shell::shell::collab_broker::spawn_native_harness(
        dir.path().join("collab.sock"),
        fb.chrome().tree_handle(),
        Vec::new(),
    )
    .unwrap();
    fb.host_mut().attach_collab_broker(&broker);
    let recorder = fb
        .host()
        .session_services()
        .recorder()
        .cloned()
        .expect("recorder attached");
    let owner = OWNER.to_string();
    recorder
        .start(
            &[ROOM.to_string()],
            vec![APP.to_string()],
            Some(PixelPurpose::Check),
            &owner,
        )
        .unwrap();
    // What the compositor measures every frame: the surface renders at 2.
    // The display the tile is on is a scale-2 output; the shell measures
    // each laid-out tab against it every frame, as the compositor does.
    fb.host_mut()
        .set_recording_outputs(vec![shell::shell::recorder::RecordedOutput {
            rect: vello::kurbo::Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6),
            scale: 2.0,
            name: "output 1".into(),
        }]);

    let mut executor = CaptureExecutor::default();
    let mut scene = frame(&mut fb);
    let _ = executor.execute_ready(&mut fb, &mut scene);
    let checkout = recorder.stop(&owner).unwrap();
    let pending = recorder.take_session(&checkout.id).unwrap();
    let strip = referenced_frames(&pending);
    assert_eq!(strip.purpose, PixelPurpose::Check);
    assert_eq!(strip.frames.len(), 1);
    let stored = &strip.frames[0];
    assert_eq!(
        (stored.width, stored.height),
        (width * 2, height * 2),
        "a scaled surface keeps the pixels it rendered"
    );
    assert!((stored.scale - 2.0).abs() < 1e-9, "scale {}", stored.scale);
    assert_eq!(
        stored.unexplained,
        vec![PixelRect::new(0, 0, width * 2, height * 2)]
    );
    let pixels = stored.decode().unwrap();
    // Interior points: the tile's corners are rounded.
    for (x, y) in [
        (width / 2, height / 2),
        (width, height),
        (width * 3 / 2, height * 3 / 2),
    ] {
        let got = pixels.pixel(x, y).unwrap();
        for (channel, want) in got.iter().zip([0x30, 0x60, 0xD0]) {
            assert!(channel.abs_diff(want) <= 4, "({x},{y}) is {got:?}");
        }
    }
    let replayed = CaptureExecutor::room_surface_frames(&mut fb, &ROOM.to_string(), stored.at, 2.0);
    let replayed = replayed.into_iter().next().unwrap().unwrap();
    assert_eq!((replayed.width, replayed.height), (width * 2, height * 2));
    store().forget(surface_id);
    drop(broker);
}

/// A real app answering Inspect through the broker: a person's identity,
/// certified for `ROOM`.
mod inspected {
    use std::os::fd::IntoRawFd as _;
    use std::os::unix::net::UnixStream;

    use ed25519_dalek::{Signer as _, SigningKey};

    pub const APP: &str = "so.tilo.mask-fixture";
    const ROOT_KEY: [u8; 32] = [61; 32];
    const DEVICE_KEY: [u8; 32] = [62; 32];

    pub fn owner() -> String {
        let root = SigningKey::from_bytes(&ROOT_KEY);
        let collab_types::Principal::Person(principal) = collab_types::Principal::person_from_key(
            collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
        ) else {
            panic!("a person key names a person")
        };
        principal
    }

    pub fn device() -> [u8; 32] {
        SigningKey::from_bytes(&DEVICE_KEY)
            .verifying_key()
            .to_bytes()
    }

    fn identity() -> ui::collaboration::CertifiedEndpointIdentity {
        let root = SigningKey::from_bytes(&ROOT_KEY);
        let device = SigningKey::from_bytes(&DEVICE_KEY);
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "mask".to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
        let workspace = super::ROOM.to_string();
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: workspace.clone(),
            device_certificate: serde_json::to_vec(&certificate).unwrap(),
            proof: device
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &workspace,
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap();
        ui::collaboration::CertifiedEndpointIdentity::new(presentation, device)
    }

    /// A toolkit app with a public button and a personal email field.
    pub fn app(socket: &std::path::Path) -> ui::App {
        use ui::widget::{Button, Column, TextInput, TextInputKind};
        let stream = UnixStream::connect(socket).unwrap();
        let manager = ui::collaboration::CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            APP,
            super::ROOM,
            None,
            Some("mask".to_owned()),
            identity(),
        )
        .unwrap();
        let root = Column::new()
            .push(Box::new(Button::new("send", "Send", None)))
            .push(Box::new(
                TextInput::local()
                    .with_kind(TextInputKind::Email)
                    .with_initial_value("ada@example.org")
                    .with_accessible_name("Email"),
            ));
        let mut app =
            ui::App::with_collaboration_manager(ui::State::new(), Box::new(root), manager);
        app.init_layout();
        app
    }
}

/// Replaying the room as it shows now yields `Logic`, step for step.
fn assert_replays_as_logic(
    fb: &mut FrameBuilder,
    trace: &session_trace::SessionTrace,
    strip: &FrameStrip,
) {
    let mut replay = Showing {
        fb,
        room: ROOM.to_string(),
    };
    let report =
        session_trace::divergence::check(trace, strip, &mut replay, &Tolerance::default()).unwrap();
    assert_eq!(
        report.layer,
        tilo_reference::SessionLayer::Logic,
        "{:?}",
        report.steps
    );
    assert_eq!(report.compared, u32::try_from(strip.frames.len()).unwrap());
}

/// A replay that is the live desktop: what `room` shows now, through the
/// same capture path a recording uses.
struct Showing<'a> {
    fb: &'a mut FrameBuilder,
    room: String,
}

impl session_trace::frames::FrameProvider for Showing<'_> {
    fn frames_at(
        &mut self,
        at: session_trace::Offset,
    ) -> Result<Vec<session_trace::frames::SurfaceFrame>, session_trace::frames::FrameError> {
        CaptureExecutor::room_surface_frames(self.fb, &self.room, at, 1.0)
            .into_iter()
            .map(|frame| frame.map_err(session_trace::frames::FrameError::Render))
            .collect()
    }
}

/// The inspected app, connected and inspectable, having committed its own
/// drawn frame at its tile's size as `surface_id`'s buffer: the pixels the
/// compositor records are the app's, and its Inspect bounds describe them.
fn committed_app(
    socket: &std::path::Path,
    broker: &shell::shell::collab_broker::BrokerHandle,
    surface_id: u64,
    (width, height): (u32, u32),
) -> ui::App {
    let mut app = inspected::app(socket);
    let mut drawn = vello::Scene::new();
    assert!(app.draw_frame(&mut drawn, width, height, 1.0));
    let buffer = ui::offscreen::OffscreenRenderer::new(width, height)
        .render(&drawn, vello::peniko::Color::WHITE);
    store().publish_rgba(surface_id, &buffer, width, height);
    let actions = broker.app_action_broker().expect("typed broker state");
    let started = Instant::now();
    while actions.inspectable_apps().is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the app never became inspectable"
        );
        app.pump_collab_presence();
        std::thread::sleep(Duration::from_millis(5));
    }
    app
}

#[test]
fn a_personal_field_of_a_real_app_is_masked_in_every_kept_frame() {
    let surface_id = 0x5EED_F4A3_0003;
    store().forget(surface_id);
    let owner = inspected::owner();
    let mut fb = desktop_with(surface_id, inspected::APP, &owner);
    let (width, height) = laid_out(&mut fb, surface_id);

    let dir = tempfile::tempdir().unwrap();
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let socket = dir.path().join("collab.sock");
    let broker = shell::shell::collab_broker::spawn_native_harness(
        socket.clone(),
        fb.chrome().tree_handle(),
        vec![inspected::device()],
    )
    .unwrap();
    let mut app = committed_app(&socket, &broker, surface_id, (width, height));
    present(&mut fb, surface_id);
    fb.host_mut().attach_collab_broker(&broker);
    let recorder = fb
        .host()
        .session_services()
        .recorder()
        .cloned()
        .expect("recorder attached");
    recorder
        .start(
            &[ROOM.to_string()],
            vec![inspected::APP.to_string()],
            Some(PixelPurpose::Keep),
            &owner,
        )
        .unwrap();

    // The compositor's loop: the app answers, the recorder asks for
    // keyframes (sooner for each captured surface, through `inspect_now`),
    // and the capture tick keeps frames.
    let mut executor = CaptureExecutor::default();
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(2_500) {
        app.pump_collab_presence();
        recorder.pump(Instant::now(), &owner);
        let mut scene = frame(&mut fb);
        let _ = executor.execute_ready(&mut fb, &mut scene);
        std::thread::sleep(Duration::from_millis(10));
    }
    let checkout = recorder.stop(&owner).unwrap();
    let pending = recorder.take_session(&checkout.id).unwrap();
    let trace = pending.trace().unwrap();
    let email = trace
        .timeline
        .iter()
        .filter_map(|entry| match &entry.event {
            session_trace::Event::Inspect(keyframe) => Some(keyframe),
            _ => None,
        })
        .flat_map(|keyframe| keyframe.reply.nodes.iter())
        .find(|node| node.label.as_deref() == Some("Email"))
        .cloned()
        .expect("the recording kept a keyframe of the app");
    assert_eq!(email.sensitivity, ui::a11y::Sensitivity::Personal);
    let visible = email.visible.expect("the field is on screen");

    let strip = referenced_frames(&pending);
    assert!(!strip.frames.is_empty());
    for stored in &strip.frames {
        let bounds = PixelRect::new(0, 0, stored.width, stored.height);
        let field = PixelRect::covering(&visible, stored.scale, bounds);
        assert!(!field.is_empty(), "the field lies on the captured surface");
        assert!(
            stored.masked.iter().any(|rect| rect.contains(&field)),
            "{:?} is not masked in {:?}",
            field,
            stored.masked
        );
        let pixels = stored.decode().unwrap();
        for y in field.y..field.bottom() {
            for x in field.x..field.right() {
                assert_eq!(
                    pixels.pixel(x, y),
                    Some(session_trace::mask::MASK_RGBA),
                    "({x},{y}) of the personal field is not masked"
                );
            }
        }
        // Nothing but sensitive nodes is masked.
        let outside = (0..stored.height)
            .flat_map(|y| (0..stored.width).map(move |x| (x, y)))
            .find(|&(x, y)| !stored.masked.iter().any(|rect| rect.contains_point(x, y)))
            .expect("the frame is not all mask");
        assert_ne!(
            pixels.pixel(outside.0, outside.1),
            Some(session_trace::mask::MASK_RGBA)
        );
        // The toolkit paints every pixel of its window, so none of them is
        // unexplained: not the field, not the window's own background.
        assert!(
            stored.unexplained.is_empty(),
            "a toolkit window is described: {:?}",
            stored.unexplained
        );
    }

    // Replaying the same window, pixel for pixel, is a logic problem.
    assert_replays_as_logic(&mut fb, &trace, &strip);
    store().forget(surface_id);
    drop(app);
    drop(broker);
}

#[test]
fn a_surface_that_is_never_shown_is_rendered_from_its_committed_frame() {
    // A client configured to a recorded geometry but never laid out, as a
    // replay room's app is: no tab, no tile, only its committed frame.
    let surface_id = 0x5EED_F4A3_0004;
    store().forget(surface_id);
    store().publish_rgba(surface_id, &solid(80, 60, [0x20, 0xA0, 0x40]), 80, 60);
    let fb = FrameBuilder::new((W, H));
    let route = session_trace::Route::new(APP, ROOM.to_string(), "");
    let geometry = vello::kurbo::Rect::new(0.0, 0.0, 40.0, 30.0);
    let frames = CaptureExecutor::surface_frames(
        &fb,
        &[(surface_id, route.clone(), geometry)],
        session_trace::Offset(7),
        2.0,
    );
    let frame = frames.into_iter().next().unwrap().unwrap();
    assert_eq!((frame.width, frame.height), (80, 60));
    assert!((frame.scale - 2.0).abs() < 1e-9);
    assert_eq!(frame.at, session_trace::Offset(7));
    assert!(frame.same_surface(&route));
    let centre = frame.pixel(40, 30).unwrap();
    for (got, want) in centre.iter().zip([0x20, 0xA0, 0x40]) {
        assert!(got.abs_diff(want) <= 4, "centre is {centre:?}");
    }
    let unknown = CaptureExecutor::surface_frames(
        &fb,
        &[(0x5EED_F4A3_0005, route, geometry)],
        session_trace::Offset(7),
        1.0,
    );
    assert!(
        unknown[0].is_err(),
        "a surface with no frame yet has nothing to capture"
    );
    store().forget(surface_id);
}
