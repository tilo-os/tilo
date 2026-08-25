//! Capture the first-login identity ceremony offscreen, one PNG per screen.
//!
//! The ceremony is chrome the compositor draws before any desktop exists, so
//! it is not one of `ShellDriver`'s scenes — those are all staged on a
//! running desktop. It still has to be looked at: it is the first thing
//! anybody sees, and one of its three screens now carries a form.
//!
//! Same five passes as a live frame (sync → build_layout → compute_layout →
//! apply_layout → draw), through the widget's own `Widget` impl.
//!
//! ```text
//! cargo run -p shell --example onboarding_shot -- target/onboarding
//! QOS_SHOTS_THEME=light cargo run -p shell --example onboarding_shot -- target/onboarding-light
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::{transaction::Transactable, AutoCommit, ReadDoc, ScalarValue};
use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Affine, Point};
use vello::Scene;

use shell::onboarding::MintedIdentity;
use shell::onboarding_view::{
    OnboardingView, KEY_BEGIN, KEY_CREATE, KEY_DISPLAY_NAME, KEY_UNDERSTOOD,
};

const VIEWPORT: (u32, u32) = (1280, 900);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/onboarding"));
    std::fs::create_dir_all(&out)?;
    shell::shell::i18n::set_locale("en-US");

    // Three screens, in the order a person meets them. The middle one is the
    // interesting frame: it carries the consequence, the name field and the
    // gate, and it is the one that had to find room for a form.
    for (name, steps) in [
        ("1-intro", &[][..]),
        ("2-no-recovery", &[KEY_BEGIN][..]),
        ("3-no-recovery-acknowledged", &[KEY_BEGIN, KEY_UNDERSTOOD][..]),
        (
            "4-done",
            &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE][..],
        ),
    ] {
        render(&out.join(format!("{name}.png")), steps)?;
    }
    Ok(())
}

fn render(out: &Path, steps: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = VIEWPORT;
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };

    // A fixed fingerprint, so the final screen is the same pixels every run
    // and a diff means something changed rather than that a key was minted.
    let mut view = OnboardingView::with_minter(Box::new(|name: &str| {
        println!("  minting as {name:?}");
        Ok(MintedIdentity {
            fingerprint: "4f56a1c208b39e77c1d2e3f405162738".repeat(2),
            path: PathBuf::from("/home/you/.config/qos/identity/primary.json"),
        })
    }));

    let mut doc = AutoCommit::new();
    view.sync(&mut doc, &automerge::ROOT);
    for step in steps {
        let _ = doc.put(&automerge::ROOT, *step, ScalarValue::Boolean(true));
        view.sync(&mut doc, &automerge::ROOT);
    }
    // What a person who typed over the pre-filled account name would see.
    if let Ok(Some((_, obj))) = doc.get(&automerge::ROOT, KEY_DISPLAY_NAME) {
        let len = doc.text(&obj).map(|t| t.chars().count()).unwrap_or(0);
        let _ = doc.splice_text(&obj, 0, len as isize, "Ada Lovelace");
    }
    view.sync(&mut doc, &automerge::ROOT);

    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let widget: &mut dyn Widget = &mut view;

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    // `build_layout` recreates every child, so anything holding CRDT state —
    // the name field — has to read the document again before it draws. The
    // App does exactly this in `rebuild_structure`.
    widget.sync(&mut doc, &automerge::ROOT);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;
    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &PresenceCtx::empty(),
        &PermissionContext::permissive(),
    );

    let mut renderer = OffscreenRenderer::new(w, h);
    // The ceremony paints no ground of its own — the compositor is behind it
    // — so the capture supplies the one the compositor would.
    renderer.render_png(&scene, theme.bg_app, out);
    println!("wrote {}", out.display());
    Ok(())
}
