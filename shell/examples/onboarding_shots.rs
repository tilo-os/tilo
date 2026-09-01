//! Render every screen of the first-login identity ceremony to PNG.
//!
//! ```text
//! cargo run -p shell --example onboarding_shots
//! ```
//!
//! Output is identical to what the live binary paints — the same widget tree
//! through the same layout and draw passes — so this is how the ceremony is
//! reviewed rather than by launching a session.
//!
//! The minter is stubbed with a fixed fingerprint so the "done" screen is
//! byte-stable across runs. Nothing else is faked: the widget, its state
//! machine and its copy are the production ones.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::{ScalarValue, transaction::Transactable};
use shell::onboarding::MintedIdentity;
use shell::onboarding_view::{KEY_BEGIN, KEY_CONNECT, KEY_CREATE, KEY_UNDERSTOOD, OnboardingView};
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

#[allow(
    clippy::cast_precision_loss,
    reason = "snapshot dimensions are small renderer sizes and exactly representable"
)]
fn shoot(theme: &Theme, out: &Path, steps: &[&str], width: u32, height: u32) -> Result<(), String> {
    let mut state = State::new();
    let mut view = OnboardingView::with_minter(Box::new(|| {
        Ok(MintedIdentity {
            fingerprint: "3f7a1c9e40b28d6512ef0a73c8b4d915e26f88a0c3147bd5e9f20a6c7b81d34e"
                .to_string(),
            path: PathBuf::from("/home/quinten/.config/qos/identity/primary.json"),
        })
    }));

    // Drive the real widget through the real keys, one sync per press, the
    // way a click would.
    for key in steps {
        let _ = state.doc.put(&state.root, *key, ScalarValue::Boolean(true));
        view.sync(&mut state.doc, &state.root);
    }
    view.sync(&mut state.doc, &state.root);

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
    // so this is the steady state the user actually looks at rather than the
    // single frame in which the tree was rebuilt.
    view.sync(&mut state.doc, &state.root);
    view.settle_motion_for_snapshot();

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    view.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(width, height);
    r.render_png(&scene, theme.bg_app, out);
    println!("wrote {}", out.display());
    Ok(())
}

fn main() -> Result<(), String> {
    ui::i18n::I18nManager::init("en-US");
    let dir = std::env::var("ONBOARDING_SHOTS_OUT")
        .unwrap_or_else(|_| "target/onboarding-shots".to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("create_dir_all({dir}): {e}"))?;
    let dir = PathBuf::from(dir);

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
            &dir.join(format!("2-connect-{suffix}.png")),
            &[KEY_CONNECT],
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
            &dir.join(format!("5-done-{suffix}.png")),
            &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE],
            WIDTH,
            HEIGHT,
        )?;
    }

    shoot(
        &Theme::dark(),
        &dir.join("6-intro-narrow-dark.png"),
        &[],
        420,
        640,
    )?;
    shoot(
        &Theme::dark().with_reduce_motion(true),
        &dir.join("7-no-recovery-reduced-motion.png"),
        &[KEY_BEGIN],
        800,
        600,
    )?;
    shoot(
        &Theme::dark().with_text_scale(1.3),
        &dir.join("8-done-large-text.png"),
        &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE],
        1000,
        720,
    )?;
    shoot(
        &Theme::high_contrast()
            .with_ui_scale(1.25)
            .with_reduce_motion(true),
        &dir.join("9-connect-scaled-high-contrast.png"),
        &[KEY_CONNECT],
        800,
        640,
    )?;
    Ok(())
}
