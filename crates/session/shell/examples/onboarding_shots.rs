//! Render every screen of the first-login identity ceremony to PNG.
//!
//! ```text
//! cargo run -p shell --example onboarding_shots
//! ```
//!
//! Output uses the same widget tree, layout, and draw passes as the live
//! binary, allowing the ceremony to be reviewed without launching a session.
//!
//! The minter uses a fixed fingerprint so the "done" screen is byte-stable
//! across runs. The widget, state machine, and copy use the same
//! implementations as the live path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::{ScalarValue, transaction::Transactable};
use shell::onboarding::MintedIdentity;
use shell::onboarding_view::{
    KEY_BACKUP_CONTINUE, KEY_BACKUP_SKIP_ACKNOWLEDGED, KEY_BEGIN, KEY_CHOOSE_NEARBY,
    KEY_CHOOSE_RESTORE, KEY_CREATE, KEY_EXISTING, KEY_IMPORT, KEY_TOUR_NEXT, KEY_TOUR_START,
    KEY_UNDERSTOOD, OnboardingView, OnboardingVisualState,
};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::State;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

const WIDTH: u32 = 1000;
const HEIGHT: u32 = 720;

fn shoot(
    theme: &Theme,
    out: &Path,
    steps: &[&'static str],
    width: u32,
    height: u32,
) -> Result<(), String> {
    shoot_in_context(theme, out, steps, width, height, false, false)
}

fn shoot_host(
    theme: &Theme,
    out: &Path,
    steps: &[&'static str],
    width: u32,
    height: u32,
) -> Result<(), String> {
    shoot_in_context(theme, out, steps, width, height, true, false)
}

fn shoot_host_mint_error(
    theme: &Theme,
    out: &Path,
    steps: &[&'static str],
    width: u32,
    height: u32,
) -> Result<(), String> {
    shoot_in_context(theme, out, steps, width, height, true, true)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "snapshot dimensions are small renderer sizes and exactly representable"
)]
fn shoot_in_context(
    theme: &Theme,
    out: &Path,
    steps: &[&'static str],
    width: u32,
    height: u32,
    host_desktop: bool,
    mint_error: bool,
) -> Result<(), String> {
    let mut state = State::new();
    let minter = Box::new(move || {
        if mint_error {
            Err("The identity store is unavailable. Check free space and permissions, then try again."
                .to_string())
        } else {
            Ok(MintedIdentity {
                fingerprint: "3f7a1c9e40b28d6512ef0a73c8b4d915e26f88a0c3147bd5e9f20a6c7b81d34e"
                    .to_string(),
                path: PathBuf::from("/home/user/.config/tilo/identity/primary.json"),
            })
        }
    });
    let mut view = if host_desktop {
        OnboardingView::with_minter_for_host_desktop(minter)
    } else {
        OnboardingView::with_minter(minter)
    };

    // Drive the widget through one sync per step, as a click would. The
    // acknowledgement checkboxes are document-bound; the rest are buttons.
    for &key in steps {
        if matches!(key, KEY_UNDERSTOOD | KEY_BACKUP_SKIP_ACKNOWLEDGED) {
            let _ = state.doc.put(&state.root, key, ScalarValue::Boolean(true));
        } else {
            view.press(key);
        }
        view.sync(&state.doc, &state.root);
    }
    view.sync(&state.doc, &state.root);

    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = view.build_layout(&mut taffy, theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(width as f32),
                height: AvailableSpace::Definite(height as f32),
            },
            |known, available, node_id, _, _| {
                node_map.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, &mut tr)
                })
            },
        )
        .map_err(|e| format!("compute_layout_with_measure: {e}"))?;
    view.apply_layout(&taffy, node, Point::ZERO);

    // `build_layout` rebuilds the child widgets, so they have not pulled
    // their own state out of the CRDT yet — a freshly built Checkbox does not
    // know it is ticked until it syncs. The live loop syncs once per frame,
    // so this captures the settled state after the tree was rebuilt.
    view.sync(&state.doc, &state.root);
    view.settle_motion_for_snapshot();

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    view.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(width, height);
    r.render_png(&scene, theme.tokens.colors.surfaces.base, out);
    println!("wrote {}", out.display());
    Ok(())
}

/// Render one deterministic visual state (the media codecs step's results
/// cannot be reached by key presses alone without a real helper).
#[allow(
    clippy::cast_precision_loss,
    reason = "snapshot dimensions are small renderer sizes and exactly representable"
)]
fn shoot_visual(
    theme: &Theme,
    out: &Path,
    visual: OnboardingVisualState,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let state = State::new();
    let mut view = OnboardingView::with_minter(Box::new(|| {
        Err("identity creation is not part of this snapshot".to_string())
    }));
    view.set_visual_state_for_snapshot(visual);
    view.sync(&state.doc, &state.root);

    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = view.build_layout(&mut taffy, theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(width as f32),
                height: AvailableSpace::Definite(height as f32),
            },
            |known, available, node_id, _, _| {
                node_map.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, &mut tr)
                })
            },
        )
        .map_err(|e| format!("compute_layout_with_measure: {e}"))?;
    view.apply_layout(&taffy, node, Point::ZERO);
    view.sync(&state.doc, &state.root);
    view.settle_motion_for_snapshot();

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    view.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(width, height);
    r.render_png(&scene, theme.tokens.colors.surfaces.base, out);
    println!("wrote {}", out.display());
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the declarative snapshot matrix is clearest as one ordered review manifest"
)]
fn main() -> Result<(), String> {
    ui::i18n::I18nManager::init("en-US");
    let dir = std::env::var("ONBOARDING_SHOTS_OUT")
        .unwrap_or_else(|_| "target/onboarding-shots".to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("create_dir_all({dir}): {e}"))?;
    let dir = PathBuf::from(dir);

    for (visual, name) in [
        (OnboardingVisualState::Recovery, "choose"),
        (OnboardingVisualState::RecoveryNearby, "nearby"),
        (OnboardingVisualState::RecoveryRestore, "file"),
        (OnboardingVisualState::RecoveryKeyTyping, "key-typing"),
        (OnboardingVisualState::RecoveryKeyTypo, "key-typo"),
        (OnboardingVisualState::RecoveryKeyReady, "key-ready"),
        (OnboardingVisualState::RecoveryKeyDifferent, "key-different"),
        (OnboardingVisualState::Backup, "setup-backup"),
        (OnboardingVisualState::BackupRecoveryKey, "setup-write-down"),
        (
            OnboardingVisualState::BackupRecoveryKeyWritten,
            "setup-written",
        ),
        (OnboardingVisualState::Protect, "setup-protect"),
    ] {
        shoot_visual(
            &Theme::dark(),
            &dir.join(format!("recovery-{name}-1280x800.png")),
            visual,
            1280,
            800,
        )?;
    }
    if std::env::var_os("ONBOARDING_SHOTS_RECOVERY_ONLY").is_some() {
        return Ok(());
    }

    // The Fedora/rpm-ostree host-session path is reviewed at the three common
    // laptop sizes that motivated this harness. Include the acknowledgement
    // state separately because it changes both the checkbox and primary
    // action, and must not shift either one.
    for (width, height) in [
        (1600, 900),
        (1366, 768),
        (1280, 800),
        (1280, 720),
        (800, 600),
    ] {
        let size = format!("{width}x{height}");
        shoot_host(
            &Theme::dark(),
            &dir.join(format!("host-1-choose-{size}.png")),
            &[],
            width,
            height,
        )?;
        shoot_host(
            &Theme::dark(),
            &dir.join(format!("host-2-connect-{size}.png")),
            &[KEY_EXISTING, KEY_CHOOSE_RESTORE],
            width,
            height,
        )?;
        shoot_host(
            &Theme::dark(),
            &dir.join(format!("host-2-connect-error-{size}.png")),
            &[KEY_EXISTING, KEY_CHOOSE_RESTORE, KEY_IMPORT],
            width,
            height,
        )?;
        shoot_host(
            &Theme::dark(),
            &dir.join(format!("host-2-protect-{size}.png")),
            &[KEY_BEGIN],
            width,
            height,
        )?;
        shoot_host(
            &Theme::dark(),
            &dir.join(format!("host-2-protect-acknowledged-{size}.png")),
            &[KEY_BEGIN, KEY_UNDERSTOOD],
            width,
            height,
        )?;
        shoot_host_mint_error(
            &Theme::dark(),
            &dir.join(format!("host-2-create-error-{size}.png")),
            &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE],
            width,
            height,
        )?;
        shoot_host(
            &Theme::dark(),
            &dir.join(format!("host-3-backup-{size}.png")),
            &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE],
            width,
            height,
        )?;
        shoot_host(
            &Theme::dark(),
            &dir.join(format!("host-3-backup-acknowledged-{size}.png")),
            &[
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
            ],
            width,
            height,
        )?;
        shoot_host(
            &Theme::dark(),
            &dir.join(format!("host-4-ready-{size}.png")),
            &[
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
                KEY_BACKUP_CONTINUE,
            ],
            width,
            height,
        )?;
    }
    shoot_host(
        &Theme::dark().with_text_scale(1.3),
        &dir.join("host-compact-large-text-1024x640.png"),
        &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE],
        1024,
        640,
    )?;
    shoot_host(
        &Theme::high_contrast()
            .with_ui_scale(1.25)
            .with_reduce_motion(true),
        &dir.join("host-hidpi-high-contrast-1280x720.png"),
        &[KEY_BEGIN],
        1280,
        720,
    )?;

    let accessibility_states: &[(&str, &[&'static str])] = &[
        ("choose", &[]),
        ("connect", &[KEY_EXISTING, KEY_CHOOSE_RESTORE]),
        ("protect", &[KEY_BEGIN]),
        ("protect-acknowledged", &[KEY_BEGIN, KEY_UNDERSTOOD]),
        ("backup", &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE]),
        (
            "backup-acknowledged",
            &[
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
            ],
        ),
        (
            "ready",
            &[
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
                KEY_BACKUP_CONTINUE,
            ],
        ),
    ];
    for (name, steps) in accessibility_states {
        shoot_host(
            &Theme::dark().with_text_scale(1.3),
            &dir.join(format!("host-{name}-large-text-1280x720.png")),
            steps,
            1280,
            720,
        )?;
        shoot_host(
            &Theme::high_contrast()
                .with_ui_scale(1.25)
                .with_reduce_motion(true),
            &dir.join(format!("host-{name}-high-contrast-1280x720.png")),
            steps,
            1280,
            720,
        )?;
    }

    for (visual, name) in [
        (OnboardingVisualState::Codecs, "offer"),
        (OnboardingVisualState::CodecsInstalling, "installing"),
        (OnboardingVisualState::CodecsWaitingForNetwork, "offline"),
        (OnboardingVisualState::CodecsInstalled, "installed"),
        (OnboardingVisualState::CodecsFailed, "failed"),
    ] {
        shoot_visual(
            &Theme::dark(),
            &dir.join(format!("codecs-{name}-1280x800.png")),
            visual,
            1280,
            800,
        )?;
    }
    shoot_visual(
        &Theme::light().with_text_scale(1.3),
        &dir.join("codecs-offer-large-text-1024x640.png"),
        OnboardingVisualState::Codecs,
        1024,
        640,
    )?;

    if std::env::var_os("ONBOARDING_SHOTS_HOST_ONLY").is_some() {
        return Ok(());
    }

    for (theme, suffix) in [
        (Theme::dark(), "dark"),
        (Theme::light(), "light"),
        (Theme::high_contrast(), "high-contrast"),
    ] {
        shoot(
            &theme,
            &dir.join(format!("1-intro-{suffix}.png")),
            &[],
            WIDTH,
            HEIGHT,
        )?;
        shoot(
            &theme,
            &dir.join(format!("2-existing-{suffix}.png")),
            &[KEY_EXISTING],
            WIDTH,
            HEIGHT,
        )?;
        shoot(
            &theme,
            &dir.join(format!("2-nearby-{suffix}.png")),
            &[KEY_EXISTING, KEY_CHOOSE_NEARBY],
            WIDTH,
            HEIGHT,
        )?;
        shoot(
            &theme,
            &dir.join(format!("2-connect-{suffix}.png")),
            &[KEY_EXISTING, KEY_CHOOSE_RESTORE],
            WIDTH,
            HEIGHT,
        )?;
        shoot(
            &theme,
            &dir.join(format!("3-no-recovery-{suffix}.png")),
            &[KEY_BEGIN],
            WIDTH,
            HEIGHT,
        )?;
        // The gate, ticked — the primary action becomes available only here.
        shoot(
            &theme,
            &dir.join(format!("4-acknowledged-{suffix}.png")),
            &[KEY_BEGIN, KEY_UNDERSTOOD],
            WIDTH,
            HEIGHT,
        )?;
        shoot(
            &theme,
            &dir.join(format!("5-backup-{suffix}.png")),
            &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE],
            WIDTH,
            HEIGHT,
        )?;
        shoot(
            &theme,
            &dir.join(format!("6-tour-move-{suffix}.png")),
            &[
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
                KEY_BACKUP_CONTINUE,
                KEY_TOUR_START,
            ],
            WIDTH,
            HEIGHT,
        )?;
        shoot(
            &theme,
            &dir.join(format!("7-tour-arrange-{suffix}.png")),
            &[
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
                KEY_BACKUP_CONTINUE,
                KEY_TOUR_START,
                KEY_TOUR_NEXT,
            ],
            WIDTH,
            HEIGHT,
        )?;
        shoot(
            &theme,
            &dir.join(format!("8-tour-share-{suffix}.png")),
            &[
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
                KEY_BACKUP_CONTINUE,
                KEY_TOUR_START,
                KEY_TOUR_NEXT,
                KEY_TOUR_NEXT,
            ],
            WIDTH,
            HEIGHT,
        )?;
    }

    shoot(
        &Theme::dark(),
        &dir.join("9-intro-narrow-dark.png"),
        &[],
        420,
        640,
    )?;
    shoot(
        &Theme::dark().with_reduce_motion(true),
        &dir.join("10-no-recovery-reduced-motion.png"),
        &[KEY_BEGIN],
        800,
        600,
    )?;
    shoot(
        &Theme::dark().with_text_scale(1.3),
        &dir.join("11-done-large-text.png"),
        &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE],
        1000,
        720,
    )?;
    shoot(
        &Theme::high_contrast()
            .with_ui_scale(1.25)
            .with_reduce_motion(true),
        &dir.join("12-connect-scaled-high-contrast.png"),
        &[KEY_EXISTING, KEY_CHOOSE_RESTORE],
        800,
        640,
    )?;
    shoot(
        &Theme::dark(),
        &dir.join("13-tour-move-narrow-dark.png"),
        &[
            KEY_BEGIN,
            KEY_UNDERSTOOD,
            KEY_CREATE,
            KEY_BACKUP_SKIP_ACKNOWLEDGED,
            KEY_BACKUP_CONTINUE,
            KEY_TOUR_START,
        ],
        420,
        640,
    )?;
    shoot(
        &Theme::light().with_text_scale(1.3),
        &dir.join("14-tour-arrange-large-text.png"),
        &[
            KEY_BEGIN,
            KEY_UNDERSTOOD,
            KEY_CREATE,
            KEY_BACKUP_SKIP_ACKNOWLEDGED,
            KEY_BACKUP_CONTINUE,
            KEY_TOUR_START,
            KEY_TOUR_NEXT,
        ],
        1000,
        720,
    )?;
    Ok(())
}
