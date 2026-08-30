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
use std::path::PathBuf;

use automerge::{ScalarValue, transaction::Transactable};
use shell::onboarding::MintedIdentity;
use shell::onboarding_view::{KEY_BEGIN, KEY_CREATE, KEY_UNDERSTOOD, OnboardingView};
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

fn shoot(theme: &Theme, out: PathBuf, steps: &[&str]) {
    let mut state = State::new();
    let mut view = OnboardingView::with_minter(Box::new(|| {
        Ok(MintedIdentity {
            fingerprint: "3f7a1c9e40b28d6512ef0a73c8b4d915e26f88a0c3147bd5e9f20a6c7b81d34e"
                .to_string(),
            path: PathBuf::from("/home/user/.config/qos/identity/primary.json"),
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
                width: AvailableSpace::Definite(WIDTH as f32),
                height: AvailableSpace::Definite(HEIGHT as f32),
            },
            |known, available, node_id, _, _| {
                node_map.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, &mut tr)
                })
            },
        )
        .unwrap();
    view.apply_layout(&taffy, node, Point::ZERO);

    // `build_layout` rebuilds the child widgets, so they have not pulled
    // their own state out of the CRDT yet — a freshly built Checkbox does not
    // know it is ticked until it syncs. The live loop syncs once per frame,
    // so this is the steady state the user actually looks at rather than the
    // single frame in which the tree was rebuilt.
    view.sync(&mut state.doc, &state.root);

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    view.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, HEIGHT);
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {}", out.display());
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let dir = std::env::var("ONBOARDING_SHOTS_OUT")
        .unwrap_or_else(|_| "target/onboarding-shots".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let dir = PathBuf::from(dir);

    for (theme, suffix) in [(Theme::dark(), "dark"), (Theme::light(), "light")] {
        shoot(&theme, dir.join(format!("1-intro-{suffix}.png")), &[]);
        shoot(
            &theme,
            dir.join(format!("2-no-recovery-{suffix}.png")),
            &[KEY_BEGIN],
        );
        // The gate, ticked — the primary action becomes available only here.
        shoot(
            &theme,
            dir.join(format!("3-acknowledged-{suffix}.png")),
            &[KEY_BEGIN, KEY_UNDERSTOOD],
        );
        shoot(
            &theme,
            dir.join(format!("4-done-{suffix}.png")),
            &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE],
        );
    }
}
