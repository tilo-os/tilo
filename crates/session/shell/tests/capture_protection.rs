//! Capture protection on the production capture paths.
//!
//! A window whose app asked for protection must leave the display only as
//! the themed placeholder — in a screenshot, an App capture and a screen-share
//! frame — while the local display keeps showing it. The lock screen is
//! withheld from every capture whether or not anything asked.
//!
//! Every capture here goes through the executor entry points the compositor
//! calls (`render_screenshot`, `render_portal_screencast_frame`) on the scene
//! `FrameBuilder::paint` produced; the client frame reaches the tile through
//! the same `ForeignFrameStore` the commit path fills.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::compositor::capture::CaptureExecutor;
use shell::compositor::foreign::{self, store};
use shell::compositor::frame::FrameBuilder;
use shell::compositor::lock_screen::{self, LockOutput, LockView};
use shell::compositor::tab_binding;
use shell::shell::capture_protection;
use shell::shell::state::WorkspaceTree;
use ui::offscreen::OffscreenRenderer;
use ui::widget::{
    AudioCaptureMode, CaptureDestination, CaptureExtent, CaptureIntent, CaptureMode, PipLayout,
    SourceMode,
};
use vello::kurbo::Rect;
use vello::peniko::Color;

const W: u32 = 1600;
const H: u32 = 900;
/// A colour no theme paints, so a match can only be the client's pixels.
const CLIENT: (u8, u8, u8) = (0xE0, 0x18, 0xA0);
const TOLERANCE: u8 = 4;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "float-to-int `as` saturates in Rust (no UB/wrap/panic); nothing here can misbehave"
)]
fn as_u32(v: f64) -> u32 {
    v as u32
}

fn near(pixel: (u8, u8, u8, u8), rgb: (u8, u8, u8)) -> bool {
    pixel.0.abs_diff(rgb.0) <= TOLERANCE
        && pixel.1.abs_diff(rgb.1) <= TOLERANCE
        && pixel.2.abs_diff(rgb.2) <= TOLERANCE
}

/// The placeholder's ground as it is rendered.
fn placeholder_rgb(fb: &FrameBuilder) -> (u8, u8, u8) {
    let [r, g, b, _] = capture_protection::placeholder_fill_rgba8(fb.theme());
    (r, g, b)
}

/// Samples on a 4×4 grid inside `rect`, skipping the centre where the
/// placeholder's mark sits and where collaboration ink may be drawn.
fn interior_samples(rgba: &[u8], width: u32, rect: Rect) -> Vec<(u8, u8, u8, u8)> {
    let mut samples = Vec::new();
    for fx in [0.15, 0.3, 0.7, 0.85] {
        for fy in [0.15, 0.3, 0.7, 0.85] {
            samples.push(OffscreenRenderer::sample(
                rgba,
                width,
                as_u32(rect.x0 + rect.width() * fx),
                as_u32(rect.y0 + rect.height() * fy),
            ));
        }
    }
    samples
}

fn count_near(samples: &[(u8, u8, u8, u8)], rgb: (u8, u8, u8)) -> usize {
    samples.iter().filter(|pixel| near(**pixel, rgb)).count()
}

/// Every eleventh pixel of a whole image — enough to find any client area.
fn sparse_pixels(rgba: &[u8]) -> Vec<(u8, u8, u8, u8)> {
    rgba.chunks_exact(4)
        .step_by(11)
        .map(|p| (p[0], p[1], p[2], p[3]))
        .collect()
}

fn intent(extent: CaptureExtent, regions: Vec<Rect>) -> CaptureIntent {
    CaptureIntent {
        mode: CaptureMode::Screenshot,
        source: if extent == CaptureExtent::Visible {
            SourceMode::Region
        } else {
            SourceMode::Window
        },
        destination: CaptureDestination::File,
        regions,
        include_pointer: false,
        audio: AudioCaptureMode::None,
        extent,
        pip_layout: PipLayout::Auto,
        targets: Vec::new(),
        recording_command: None,
        replay_command: None,
        screenshot_command: None,
    }
}

fn whole_screen() -> CaptureIntent {
    intent(
        CaptureExtent::Visible,
        vec![Rect::new(0.0, 0.0, f64::from(W), f64::from(H))],
    )
}

fn frame(fb: &mut FrameBuilder) -> vello::Scene {
    let mut doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;
    fb.pump_apps(&mut doc, &root);
    fb.paint_settled()
}

/// Paint until the adopted client has latched as presented and its opening
/// cross-fade is over, so samples measure the client and not the card.
fn show_client(fb: &mut FrameBuilder, surface_id: u64) {
    let _ = frame(fb);
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(3))
        .unwrap_or_else(std::time::Instant::now);
    let mut published = false;
    while std::time::Instant::now() < deadline {
        if !published && let Some((bw, bh)) = store().viewport(surface_id) {
            store().publish_rgba(surface_id, &solid(bw, bh, CLIENT), bw, bh);
            published = true;
        }
        let _ = fb.any_tile_opening();
        let _ = frame(fb);
        if published && store().is_presented(surface_id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    assert!(
        store().is_presented(surface_id),
        "the committed client frame never became presented"
    );
    let handover_end = std::time::Instant::now()
        .checked_add(std::time::Duration::from_millis(250))
        .unwrap_or_else(std::time::Instant::now);
    while std::time::Instant::now() < handover_end {
        let _ = frame(fb);
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

fn solid(width: u32, height: u32, rgb: (u8, u8, u8)) -> Vec<u8> {
    [rgb.0, rgb.1, rgb.2, 0xFF].repeat(usize::try_from(width.saturating_mul(height)).unwrap())
}

/// A `FrameBuilder` whose rendered tree carries the client's tab, focused,
/// reached the way the native loop reaches it.
fn adopt(surface_id: u64, app_id: &str) -> FrameBuilder {
    let mut protocol_tree = WorkspaceTree::bootstrap_demo();
    tab_binding::adopt_toplevel(&mut protocol_tree, surface_id, Some(app_id.to_string()));
    let mut fb = FrameBuilder::new((W, H));
    {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        assert!(foreign::reconcile_foreign_tabs(
            &protocol_tree,
            &mut rendered
        ));
        let (strip_id, group_id) = rendered
            .desktop
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|st| st.window_groups.iter().map(move |g| (st.id, g)))
            .find(|(_, g)| {
                g.tabs.iter().any(|t| {
                    matches!(
                        &t.tile,
                        shell::shell::state::TileContent::Foreign { surface_id: s, .. }
                            if *s == surface_id
                    )
                })
            })
            .map(|(strip_id, g)| (strip_id, g.id))
            .expect("the mirrored tree holds the adopted client's group");
        rendered.desktop.set_active_group(strip_id, group_id);
    }
    fb.with_chrome(shell::shell::chrome_state::ChromeState::bump);
    fb
}

fn tile_rect(fb: &mut FrameBuilder, surface_id: u64) -> Rect {
    let rect = fb
        .foreign_surface_rect(surface_id)
        .unwrap_or_else(|| panic!("no visible tile hosts surface {surface_id}"));
    assert!(
        rect.width() > 100.0 && rect.height() > 100.0,
        "the client tile must occupy real area; got {rect:?}"
    );
    rect
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario keeps each capture path beside the same protected client"
)]
fn a_protected_window_is_captured_as_the_placeholder_and_an_unprotected_one_as_its_pixels() {
    const SURFACE: u64 = 0xCA97_0001;
    const APP: &str = "org.example.capture-protection.vault";
    const CONNECTION: u64 = 0xCA97_0C01;
    store().forget(SURFACE);
    store().release_capture_protection(CONNECTION);

    let mut fb = adopt(SURFACE, APP);
    show_client(&mut fb, SURFACE);
    let rect = tile_rect(&mut fb, SURFACE);
    let fill = placeholder_rgb(&fb);
    assert!(
        !near((fill.0, fill.1, fill.2, 0xFF), CLIENT),
        "fixture: the placeholder must be distinguishable from the client"
    );

    // Unprotected: the screenshot holds the client's pixels.
    let scene = frame(&mut fb);
    let (shot, width, _) =
        CaptureExecutor::render_screenshot(&mut fb, &scene, &whole_screen()).expect("screenshot");
    let samples = interior_samples(&shot, width, rect);
    assert!(
        count_near(&samples, CLIENT) >= 12,
        "an unprotected window is captured normally; samples {samples:?} in {rect:?}"
    );

    // The app asks for protection over its authenticated connection.
    store().request_app_capture_protection(CONNECTION, APP, true);
    let scene = frame(&mut fb);

    // Phase `screenshot_is_the_placeholder`.
    let (shot, width, _) =
        CaptureExecutor::render_screenshot(&mut fb, &scene, &whole_screen()).expect("screenshot");
    let samples = interior_samples(&shot, width, rect);
    assert_eq!(
        count_near(&sparse_pixels(&shot), CLIENT),
        0,
        "no client pixel may reach a screenshot of a protected window"
    );
    assert!(
        count_near(&samples, fill) >= 14,
        "the protected tile must be the themed placeholder {fill:?}; samples {samples:?}"
    );

    // Phase `local_display_is_unaffected`.
    let mut renderer = OffscreenRenderer::new(W, H);
    let local = renderer.render(&scene, Color::BLACK);
    let samples = interior_samples(&local, W, rect);
    assert!(
        count_near(&samples, CLIENT) >= 12,
        "the local display still shows the protected window; samples {samples:?}"
    );

    // Phase `screencast_frame_is_the_placeholder`.
    let (cast_width, _, cast) = CaptureExecutor::default()
        .render_portal_screencast_frame(&mut fb, &scene)
        .expect("screen-share frame");
    assert_eq!(count_near(&sparse_pixels(&cast), CLIENT), 0);
    assert!(count_near(&interior_samples(&cast, cast_width, rect), fill) >= 14);

    // Phase `app_capture_is_the_placeholder_even_off_screen`: an App capture
    // repaints the window outside the output clip, so it must not rely on
    // what the presented scene held.
    let target = fb
        .capture_targets()
        .into_iter()
        .find(|target| target.virtual_rect.contains(rect.center()))
        .expect("the protected window is a capture target");
    let mut app_capture = intent(CaptureExtent::FullApp, vec![target.virtual_rect]);
    app_capture.targets = vec![target];
    let (shot, _, _) =
        CaptureExecutor::render_screenshot(&mut fb, &scene, &app_capture).expect("app capture");
    let pixels = sparse_pixels(&shot);
    assert_eq!(count_near(&pixels, CLIENT), 0);
    assert!(
        count_near(&pixels, fill) * 2 > pixels.len(),
        "an App capture of a protected window is mostly placeholder"
    );

    // Phase `protection_ends_with_its_connection`.
    store().release_capture_protection(CONNECTION);
    let scene = frame(&mut fb);
    let (shot, width, _) =
        CaptureExecutor::render_screenshot(&mut fb, &scene, &whole_screen()).expect("screenshot");
    drop(fb);
    assert!(count_near(&interior_samples(&shot, width, rect), CLIENT) >= 12);

    store().forget(SURFACE);
}

#[test]
fn a_protected_app_does_not_withhold_other_apps() {
    const SURFACE: u64 = 0xCA97_0002;
    const CONNECTION: u64 = 0xCA97_0C02;
    store().forget(SURFACE);
    let mut fb = adopt(SURFACE, "org.example.capture-protection.editor");
    show_client(&mut fb, SURFACE);
    let rect = tile_rect(&mut fb, SURFACE);

    store().request_app_capture_protection(
        CONNECTION,
        "org.example.capture-protection.some-other-vault",
        true,
    );
    let scene = frame(&mut fb);
    let (shot, width, _) =
        CaptureExecutor::render_screenshot(&mut fb, &scene, &whole_screen()).expect("screenshot");
    drop(fb);
    store().release_capture_protection(CONNECTION);
    assert!(
        count_near(&interior_samples(&shot, width, rect), CLIENT) >= 12,
        "another app's protection must not blank this window"
    );
    store().forget(SURFACE);
}

#[test]
fn the_lock_screen_is_never_captured() {
    let mut fb = FrameBuilder::new((W, H));
    let theme = fb.theme().clone();
    let fill = placeholder_rgb(&fb);
    let outputs = [LockOutput {
        origin: (0.0, 0.0),
        size: (f64::from(W), f64::from(H)),
        scale: 1.0,
    }];
    let view = LockView {
        password_len: 6,
        error: Some("Incorrect password"),
        clock: "13:37",
        date: "Friday, 11 July",
        notification_count: 3,
        verifying: false,
        now_playing: None,
    };
    let lock = lock_screen::draw_lock_scene_at_time(
        &outputs,
        &theme,
        fb.text_renderer_mut(),
        &view,
        None,
        0.0,
    );
    let everywhere = Rect::new(0.0, 0.0, f64::from(W), f64::from(H));

    // The lock screen itself is not the placeholder, so a match below means
    // it was withheld rather than coincidentally the same colour.
    let mut renderer = OffscreenRenderer::new(W, H);
    let local = renderer.render(&lock, Color::BLACK);
    assert!(
        count_near(&interior_samples(&local, W, everywhere), fill) < 16,
        "fixture: the lock screen must differ from the placeholder somewhere"
    );

    // No request is involved: a locked session protects everything.
    fb.set_session_locked(true);
    let (shot, width, _) =
        CaptureExecutor::render_screenshot(&mut fb, &lock, &whole_screen()).expect("screenshot");
    let samples = interior_samples(&shot, width, everywhere);
    assert_eq!(
        count_near(&samples, fill),
        samples.len(),
        "a screenshot of the locked session is only the placeholder; samples {samples:?}"
    );
    let (cast_width, _, cast) = CaptureExecutor::default()
        .render_portal_screencast_frame(&mut fb, &lock)
        .expect("screen-share frame");
    let samples = interior_samples(&cast, cast_width, everywhere);
    assert_eq!(count_near(&samples, fill), samples.len());

    // An App capture has no desktop to repaint while locked.
    let mut app_capture = intent(CaptureExtent::FullApp, vec![everywhere]);
    app_capture.targets = fb.capture_targets();
    let captured = CaptureExecutor::render_screenshot(&mut fb, &lock, &app_capture);
    drop(fb);
    assert!(captured.is_err());
}
