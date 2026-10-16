//! Production-renderer review harness for the first-login identity setup.
//!
//! It drives the compositor-owned [`OnboardingView`] through its layout and
//! draw path, with identity I/O replaced by the view's existing test seam. It
//! never creates an identity or changes the running session.
//!
//! ```text
//! TILO_ONBOARDING_SHOT_DIR=/tmp/tilo-onboarding-shots \
//!   cargo test -p shell --test it onboarding_visual::render_onboarding_matrix -- --ignored --nocapture
//! ```

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a visual review harness uses assertions and deterministic test state"
)]

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{AutoCommit, ScalarValue, transaction::Transactable};
use shell::onboarding::{MintedIdentity, Screen};
use shell::onboarding_view::{
    KEY_BACKUP_CONTINUE, KEY_BACKUP_SKIP_ACKNOWLEDGED, KEY_BEGIN, KEY_CREATE, KEY_UNDERSTOOD,
    OnboardingView,
};
use taffy::{NodeId, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Scrollable, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use ui::offscreen::OffscreenRenderer;

fn shot_dir() -> PathBuf {
    std::env::var_os("TILO_ONBOARDING_SHOT_DIR").map_or_else(
        || std::env::temp_dir().join("tilo-onboarding-shots"),
        PathBuf::from,
    )
}

/// The acknowledgement checkboxes are document-bound; the rest are buttons.
fn press(view: &OnboardingView, document: &mut AutoCommit, key: &'static str) {
    if matches!(key, KEY_UNDERSTOOD | KEY_BACKUP_SKIP_ACKNOWLEDGED) {
        document
            .put(&automerge::ROOT, key, ScalarValue::Boolean(true))
            .expect("set visual-review acknowledgement");
    } else {
        view.press(key);
    }
}

fn identity_view() -> OnboardingView {
    OnboardingView::with_minter_for_host_desktop(Box::new(|| {
        Ok(MintedIdentity {
            fingerprint: "ab".repeat(32),
            path: PathBuf::from("/identity/primary.json"),
        })
    }))
}

fn state_view(label: &str) -> OnboardingView {
    let mut view = identity_view();
    let mut document = AutoCommit::new();
    match label {
        "intro" => {}
        "protect" => {
            press(&view, &mut document, KEY_BEGIN);
            view.sync(&document, &automerge::ROOT);
        }
        "backup" => {
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                press(&view, &mut document, key);
                view.sync(&document, &automerge::ROOT);
            }
            assert_eq!(view.flow().screen(), Screen::Done);
        }
        "ready" => {
            for key in [
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
                KEY_BACKUP_CONTINUE,
            ] {
                press(&view, &mut document, key);
                view.sync(&document, &automerge::ROOT);
            }
            assert_eq!(view.flow().screen(), Screen::Done);
        }
        "tour" => return OnboardingView::new_tour(),
        _ => panic!("unknown OOBE visual state: {label}"),
    }
    view
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the offscreen renderer dimensions are screen-sized u32 values accepted by taffy's f32 layout API"
)]
fn render_view(
    view: &mut OnboardingView,
    theme: &Theme,
    renderer: &mut OffscreenRenderer,
    path: &std::path::Path,
    scroll_to_bottom: bool,
) {
    let mut text = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root = view.build_layout(&mut taffy, theme, &mut nodes);
    let measure_nodes = nodes.clone();
    taffy
        .compute_layout_with_measure(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(renderer.width as f32),
                height: taffy::AvailableSpace::Definite(renderer.height as f32),
            },
            |known, available, node, _, _| {
                measure_nodes
                    .get(&node)
                    .map_or(taffy::Size::ZERO, |&widget| unsafe {
                        (*widget).measure(known, available, &mut text)
                    })
            },
        )
        .expect("OOBE visual layout");
    view.apply_layout(&taffy, root, Point::ZERO);
    if scroll_to_bottom {
        let scroll = nodes
            .values()
            .copied()
            .find(|&pointer| unsafe { (*pointer).name() == "Scrollable" })
            .expect("OOBE page has a vertical scroll viewport");
        let scroll = unsafe {
            (*scroll)
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<Scrollable>())
                .expect("OOBE scroll viewport exposes its offset for review")
        };
        let max_offset = scroll.max_offset_y();
        assert!(
            max_offset.is_finite(),
            "OOBE bottom review needs a finite scroll extent"
        );
        scroll.set_offset_y(max_offset);
        assert_eq!(
            scroll.offset_y(),
            max_offset,
            "OOBE bottom review must land on the clamped maximum offset"
        );
        // Scroll offsets move the child origin, so apply the unchanged taffy
        // layout once more before recording the bottom-reachability frame.
        view.apply_layout(&taffy, root, Point::ZERO);
    }
    view.settle_motion_for_snapshot();

    let mut scene = Scene::new();
    view.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        &PresenceCtx::default(),
    );
    renderer.render_png(&scene, Color::BLACK, path);
}

#[test]
#[ignore = "review tool: needs an offscreen wgpu adapter and writes PNGs"]
fn render_onboarding_matrix() {
    let root = shot_dir();
    std::fs::create_dir_all(&root).expect("create OOBE screenshot directory");

    for (theme_label, theme) in [
        ("normal", Theme::dark()),
        ("large-text", Theme::dark().with_text_scale(1.3)),
        ("high-contrast", Theme::high_contrast().with_ui_scale(1.25)),
    ] {
        for (width, height) in [(1280, 720), (1280, 800), (960, 540)] {
            for state in ["intro", "protect", "backup", "ready", "tour"] {
                let mut view = state_view(state);
                let mut renderer = OffscreenRenderer::new(width, height);
                render_view(
                    &mut view,
                    &theme,
                    &mut renderer,
                    &root.join(format!("{width}x{height}-{theme_label}-{state}.png")),
                    false,
                );
                // These are the two vertically dense pages. Their bottom
                // frames prove that the fixed footer cannot make the last
                // content or action unreachable at compact/a11y density.
                if matches!(state, "backup" | "tour") {
                    let mut bottom_view = state_view(state);
                    let mut bottom_renderer = OffscreenRenderer::new(width, height);
                    render_view(
                        &mut bottom_view,
                        &theme,
                        &mut bottom_renderer,
                        &root.join(format!("{width}x{height}-{theme_label}-{state}-bottom.png")),
                        true,
                    );
                }
            }
        }
    }
    println!("OOBE review PNGs: {}", root.display());
}
