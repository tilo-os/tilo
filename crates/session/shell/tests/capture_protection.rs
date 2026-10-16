//! Capture protection on the production capture paths.
//!
//! A window whose app asked for protection must leave the display only as
//! the themed placeholder — in a screenshot, an App capture and a screen-share
//! frame — while the local display keeps showing it. The lock screen is
//! withheld from every capture whether or not anything asked, and so are the
//! shell's own secret prompts and, while a password field has focus, the
//! on-screen keyboard.
//!
//! Every capture here goes through the executor entry points the compositor
//! calls (`render_screenshot`, `render_portal_screencast_frame`) on the scene
//! `FrameBuilder::paint` produced; the client frame reaches the tile through
//! the same `ForeignFrameStore` the commit path fills.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; the sample counters are bounded by the screen"
)]

use shell::compositor::capture::CaptureExecutor;
use shell::compositor::foreign::store;
use shell::compositor::frame::FrameBuilder;
use shell::compositor::lock_screen::{self, LockOutput, LockView};
use shell::compositor::tab_binding;
use shell::shell::capture_protection;
use ui::num::u32_of_f64;
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
                u32_of_f64(rect.x0 + rect.width() * fx),
                u32_of_f64(rect.y0 + rect.height() * fy),
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
    let deadline = tilo_test_env::hang_deadline();
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
    let mut fb = FrameBuilder::new((W, H));
    {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        tab_binding::adopt_toplevel(&mut rendered, surface_id, Some(app_id.to_string()));
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
        keyboard: None,
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

/// Whether a shell prompt at `rect` is withheld: every sample of it in the
/// capture is the placeholder, away from its rounded corners and the mark at
/// its centre, while the local display shows the prompt's content there.
fn assert_prompt_withheld(
    local: &[u8],
    capture: &[u8],
    width: u32,
    rect: Rect,
    fill: (u8, u8, u8),
    what: &str,
) {
    let screen = Rect::new(0.0, 0.0, f64::from(W), f64::from(H));
    let area = rect.intersect(screen).inset(-24.0);
    assert!(
        area.width() > 48.0 && area.height() > 48.0,
        "{what}: the prompt must occupy real area; got {rect:?}"
    );
    let mark = Rect::from_center_size(area.center(), (72.0, 72.0));
    let mut samples = 0_usize;
    let mut shown = 0_usize;
    let mut withheld = 0_usize;
    for row in 0..u32_of_f64(area.height() / 3.0) {
        let y = area.y0 + f64::from(row) * 3.0;
        for column in 0..u32_of_f64(area.width() / 3.0) {
            let x = area.x0 + f64::from(column) * 3.0;
            if mark.contains(vello::kurbo::Point::new(x, y)) {
                continue;
            }
            samples += 1;
            let (px, py) = (u32_of_f64(x), u32_of_f64(y));
            if !near(OffscreenRenderer::sample(local, W, px, py), fill) {
                shown += 1;
            }
            if near(OffscreenRenderer::sample(capture, width, px, py), fill) {
                withheld += 1;
            }
        }
    }
    assert!(
        shown * 100 >= samples,
        "{what}: fixture: the prompt's content must be on the local display ({shown} of {samples} samples)"
    );
    assert_eq!(
        withheld, samples,
        "{what}: a capture of the prompt must be only the placeholder"
    );
}

/// Paint, then check every secret prompt the shell reports is withheld from
/// a screenshot and a screen-share frame while the local display shows it.
pub(crate) fn assert_secret_prompts_withheld(fb: &mut FrameBuilder, what: &str) {
    let scene = frame(fb);
    let prompts = fb.host().secret_prompt_rects();
    assert!(!prompts.is_empty(), "{what}: the prompt is a secret prompt");
    let fill = placeholder_rgb(fb);
    let mut renderer = OffscreenRenderer::new(W, H);
    let local = renderer.render(&scene, Color::BLACK);
    let (shot, shot_width, _) =
        CaptureExecutor::render_screenshot(fb, &scene, &whole_screen()).expect("screenshot");
    let (cast_width, _, cast) = CaptureExecutor::default()
        .render_portal_screencast_frame(fb, &scene)
        .expect("screen-share frame");
    for rect in prompts {
        assert_prompt_withheld(&local, &shot, shot_width, rect, fill, what);
        assert_prompt_withheld(&local, &cast, cast_width, rect, fill, what);
    }
}

#[test]
fn the_shells_secret_prompts_are_never_captured() {
    use shell::shell::portal_dialog::{
        AuthenticationRequest, NetworkSecretsRequest, PortalDialogQueue, SecretFieldRule,
        SecretFieldSpec,
    };

    // Phase `administrator_authentication`.
    {
        let mut fb = FrameBuilder::new((W, H));
        let queue = PortalDialogQueue::default();
        fb.host_mut().set_portal_dialog_queue(queue.clone());
        let _reply = queue
            .enqueue_authentication(AuthenticationRequest {
                handle: "/capture-protection/authentication".into(),
                message: "Authentication is required to change the system time".into(),
                action_id: "org.example.capture-protection.set-time".into(),
                identities: vec!["alice".into()],
                identity: 0,
                prompt: "Password:".into(),
                echo: false,
                notice: None,
            })
            .expect("the sheet is queued");
        fb.host_mut().pump_portal_dialogs();
        assert_secret_prompts_withheld(&mut fb, "administrator authentication");
    }

    // Phase `network_secrets`.
    {
        let mut fb = FrameBuilder::new((W, H));
        let queue = PortalDialogQueue::default();
        fb.host_mut().set_portal_dialog_queue(queue.clone());
        let _reply = queue
            .enqueue_network_secrets(NetworkSecretsRequest {
                handle: "/capture-protection/network".into(),
                title: "Wi-Fi network \"Cafe\"".into(),
                detail: "A passphrase is required to connect.".into(),
                fields: vec![SecretFieldSpec {
                    key: "psk".into(),
                    label: "Passphrase".into(),
                    masked: true,
                    required: true,
                    initial: String::new(),
                    rule: SecretFieldRule::WpaPassphrase,
                }],
                remember: Some(true),
            })
            .expect("the sheet is queued");
        fb.host_mut().pump_portal_dialogs();
        assert_secret_prompts_withheld(&mut fb, "network secrets");
    }

    // Phase `wifi_passphrase`.
    {
        let mut fb = FrameBuilder::new((W, H));
        fb.with_chrome(|chrome| {
            chrome.overlays.quick_settings = true;
            chrome.quick_settings.wifi_password = Some(
                shell::shell::quick_settings::WifiPasswordPrompt::new("Cafe"),
            );
            chrome.bump();
        });
        fb.host_mut().force_quick_settings_settled();
        assert_secret_prompts_withheld(&mut fb, "the Wi-Fi passphrase");
        drop(fb);
    }

    // Phase `identity_restore_and_backup`: the steps that take a backup's
    // passphrase (and show a nearby device's pairing code) or set a new one.
    // The steps around them hold no secret and are captured as they are.
    {
        use shell::onboarding_view::OnboardingVisualState;
        let identity = tempfile::tempdir().expect("identity dir");
        let mut fb = FrameBuilder::new((W, H));
        fb.enable_host_desktop_onboarding(identity.path().to_path_buf());
        for (state, secret) in [
            (OnboardingVisualState::Intro, false),
            (OnboardingVisualState::Existing, false),
            (OnboardingVisualState::NearbyCode, true),
            (OnboardingVisualState::Restore, true),
            (OnboardingVisualState::Protect, false),
            (OnboardingVisualState::Backup, true),
            (OnboardingVisualState::BackupVerified, true),
            (OnboardingVisualState::Ready, false),
        ] {
            fb.host_mut()
                .set_onboarding_visual_state_for_snapshot(state);
            fb.relayout();
            if secret {
                assert_secret_prompts_withheld(&mut fb, &format!("setup step {state:?}"));
            } else {
                let _ = frame(&mut fb);
                assert!(
                    fb.host().secret_prompt_rects().is_empty(),
                    "setup step {state:?} shows no secret"
                );
            }
        }
        drop(fb);
    }
}

/// A shortcut step that asks for a secret is withheld; one that asks for
/// plain text is captured as it is.
#[test]
fn a_shortcut_steps_secret_answer_is_never_captured() {
    use shell::shell::portal_dialog::{PortalDialogQueue, ShortcutStepField, ShortcutStepRequest};

    for masked in [true, false] {
        let mut fb = FrameBuilder::new((W, H));
        let queue = PortalDialogQueue::default();
        fb.host_mut().set_portal_dialog_queue(queue.clone());
        queue
            .enqueue_shortcut_step(
                ShortcutStepRequest {
                    handle: "shortcut-step:capture-protection".into(),
                    shortcut: "Publish".into(),
                    title: "Publishing token".into(),
                    detail: String::new(),
                    confirm_label: "Continue".into(),
                    choices: Vec::new(),
                    field: Some(ShortcutStepField { masked }),
                },
                |_| {},
            )
            .expect("the sheet is queued");
        fb.host_mut().pump_portal_dialogs();
        if masked {
            assert_secret_prompts_withheld(&mut fb, "a shortcut step's secret answer");
        } else {
            let _ = frame(&mut fb);
            assert!(
                fb.host().secret_prompt_rects().is_empty(),
                "a plain shortcut answer is no secret"
            );
        }
    }
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the harness and frame builder drive every phase of the one keyboard session"
)]
fn the_on_screen_keyboard_is_withheld_while_a_password_field_has_focus() {
    use super::emoji_picker_commit::Harness;
    use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
        ContentHint, ContentPurpose,
    };
    const KEYBOARD: u64 = 0xCA97_0051;
    store().forget(KEYBOARD);

    let mut h = Harness::new();
    let _keyboard_session = h.own_seat_with_on_screen_keyboard();
    let field = h.field(Some((
        ContentHint::HiddenText | ContentHint::SensitiveData,
        ContentPurpose::Password,
    )));
    assert!(h.state.text_input_active(), "the password field takes text");

    // The keyboard's toplevel, promoted by the compositor, with its frame.
    h.state.osk_surface = Some(KEYBOARD);
    let mut fb = FrameBuilder::new((W, H));
    store().publish_rgba(KEYBOARD, &solid(960, 360, CLIENT), 960, 360);
    fb.set_osk_surface(Some(KEYBOARD), None, None, true);
    // Paint until the keyboard's frame is presented and any opening
    // cross-fade is over, so samples measure the keyboard itself.
    let settled = std::time::Instant::now()
        .checked_add(std::time::Duration::from_millis(400))
        .unwrap_or_else(std::time::Instant::now);
    while std::time::Instant::now() < settled {
        let _ = frame(&mut fb);
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    let (_, keyboard) = fb
        .osk_surface_rect()
        .expect("the keyboard is shown for the focused field");
    let fill = placeholder_rgb(&fb);

    let capture = |fb: &mut FrameBuilder| {
        let scene = frame(fb);
        let mut renderer = OffscreenRenderer::new(W, H);
        let local = renderer.render(&scene, Color::BLACK);
        let (shot, width, _) =
            CaptureExecutor::render_screenshot(fb, &scene, &whole_screen()).expect("screenshot");
        let (cast_width, _, cast) = CaptureExecutor::default()
            .render_portal_screencast_frame(fb, &scene)
            .expect("screen-share frame");
        (
            interior_samples(&local, W, keyboard),
            interior_samples(&shot, width, keyboard),
            interior_samples(&cast, cast_width, keyboard),
        )
    };

    // Phase `password_field_withholds_the_keyboard`.
    h.state.sync_capture_protection();
    let (local, shot, cast) = capture(&mut fb);
    assert!(
        count_near(&local, CLIENT) >= 12,
        "the local display shows the keyboard; samples {local:?}"
    );
    assert_eq!(count_near(&shot, CLIENT), 0, "no key reaches a screenshot");
    assert!(count_near(&shot, fill) >= 14, "samples {shot:?}");
    assert_eq!(
        count_near(&cast, CLIENT),
        0,
        "no key reaches a screen share"
    );
    assert!(count_near(&cast, fill) >= 14, "samples {cast:?}");

    // Phase `ordinary_field_shows_the_keyboard`: the same field now declares
    // ordinary text.
    field
        .text_input()
        .set_content_type(ContentHint::None, ContentPurpose::Normal);
    field.text_input().commit();
    h.pump();
    h.state.sync_capture_protection();
    let (_, shot, cast) = capture(&mut fb);
    assert!(
        count_near(&shot, CLIENT) >= 12,
        "an ordinary field's keyboard is captured as it is; samples {shot:?}"
    );
    assert!(count_near(&cast, CLIENT) >= 12);

    store().forget(KEYBOARD);
}

#[test]
fn disconnect_retains_the_displayed_secret_until_its_surface_is_destroyed() {
    const SURFACE: u64 = 0xCA97_0010;
    const CONNECTION: u64 = 0xCA97_0C10;
    const APP: &str = "org.example.capture-protection.disconnected";
    store().forget(SURFACE);
    store().release_capture_protection(CONNECTION);
    let mut fb = adopt(SURFACE, APP);
    show_client(&mut fb, SURFACE);
    let rect = tile_rect(&mut fb, SURFACE);
    store().request_app_capture_protection(CONNECTION, APP, true);
    store().retain_surface_capture_protection(SURFACE, APP);
    store().release_capture_protection(CONNECTION);
    let scene = frame(&mut fb);
    let (shot, width, _) = CaptureExecutor::render_screenshot(&mut fb, &scene, &whole_screen())
        .expect("disconnected screenshot");
    assert_eq!(count_near(&interior_samples(&shot, width, rect), CLIENT), 0);
    drop(fb);
    store().forget(SURFACE);
    assert!(
        !store().is_capture_protected(SURFACE, APP),
        "destroying the surface clears its protection latch"
    );
}

/// A popup is part of the window that opened it. While that window's app is
/// protected, the compositor's per-wakeup pass withholds the popup from the
/// first wakeup after it maps; once the app's protection is released, the
/// next wakeup lets it through again. Both changes reach the pass only
/// through its change signals (the popup list and the store's protection
/// epoch), so a skipped re-derivation would fail here.
#[test]
fn a_protected_windows_popup_is_withheld_from_the_wakeup_it_maps_until_its_app_is_released() {
    use super::closing_window_frames::{APP_ID, Harness, shown_window};
    use shell::compositor::state::surface_id;
    const CONNECTION: u64 = 0xCA97_0188;
    // One wakeup, in the loop's order: fold tree writes in, then protect.
    fn wakeup(h: &mut Harness) {
        let state = &mut h.state;
        state.client_input.refresh_from_tree(&state.workspace_tree);
        state.sync_capture_protection();
    }
    store().release_capture_protection(CONNECTION);

    let mut h = Harness::new();
    let qh = h.queue.handle();
    let (compositor, wm, _window, xdg, _buffer) = shown_window(&mut h);
    store().request_app_capture_protection(CONNECTION, APP_ID, true);
    wakeup(&mut h);

    let positioner = wm.create_positioner(&qh, ());
    positioner.set_size(32, 32);
    positioner.set_anchor_rect(0, 0, 8, 8);
    let menu = compositor.create_surface(&qh, ());
    let menu_xdg = wm.get_xdg_surface(&menu, &qh, ());
    let _popup = menu_xdg.get_popup(Some(&xdg), &positioner, &qh, ());
    menu.commit();
    h.pump();
    let popups = h
        .state
        .xdg_popups()
        .map(|popup| surface_id(popup.wl_surface()))
        .collect::<Vec<_>>();
    let [popup] = popups.as_slice() else {
        panic!("one popup is mapped, got {popups:?}");
    };

    wakeup(&mut h);
    assert!(
        store().is_capture_protected(*popup, ""),
        "the menu of a protected window is withheld from captures"
    );
    for _ in 0..3 {
        wakeup(&mut h);
        assert!(
            store().is_capture_protected(*popup, ""),
            "wakeups that change nothing keep it withheld"
        );
    }

    store().release_capture_protection(CONNECTION);
    wakeup(&mut h);
    assert!(
        !store().is_capture_protected(*popup, ""),
        "once the app is no longer protected, neither is its menu"
    );
    store().forget(*popup);
}
