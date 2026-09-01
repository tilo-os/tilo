//! Offscreen renders of the **permission-denied** state, so the scrim that
//! signals "you cannot act on this" can be graded instead of assumed.
//!
//! Each row draws the same control twice — once through a permissive
//! [`PermissionContext`] and once through one that grants nothing — via the
//! real `draw_with_presence` path, which is where the scrim is applied. What
//! the PNG shows is therefore the production denied state, not a mock of it.
//!
//! Both a neutral control (primary text on the card) and an accent-filled one
//! are included on purpose: a scrim that darkens body and label together
//! preserves their ratio, so an accent chip can mute far less than a neutral
//! one. Seeing them side by side is what tells you whether the signal holds.
//!
//! Run:  cargo run -p ui --example denied_state_shots
//! Out:  $DENIED_SHOTS_OUT (default /tmp/denied_shots) — one PNG per theme in
//!       $DENIED_SHOTS_THEMES (default "Dark,Light,High Contrast").

use std::collections::HashMap;
use std::path::PathBuf;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use ui::offscreen::OffscreenRenderer;
use ui::permissions::{PermissionContext, PermissionScope, Verb};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, Checkbox, Toggle, Widget};

const WIDTH: u32 = 760;
const HEIGHT: u32 = 300;
const COL_ENABLED: f64 = 40.0;
const COL_DENIED: f64 = 420.0;

/// A scope nothing in `denying()` grants, so every control carrying it draws
/// its denied state.
fn locked() -> PermissionScope {
    PermissionScope::app("locked.example", Verb::Write)
}

/// A context that grants nothing at all.
fn denying() -> PermissionContext {
    PermissionContext::from_capabilities(ui::permissions::Actor::person(None, "nobody"), vec![])
}

fn stamp(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    state: &mut ui::State,
    widget: &mut dyn Widget,
    origin: Point,
    permissions: &PermissionContext,
) {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    widget.sync(&mut state.doc, &state.root);
    let node = widget.build_layout(&mut taffy, theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            Size { width: AvailableSpace::Definite(280.0), height: AvailableSpace::Definite(44.0) },
            |known, available, node_id, _, _| {
                node_map
                    .get(&node_id)
                    .map(|&p| unsafe { (*p).measure(known, available, tr) })
                    .unwrap_or(Size::ZERO)
            },
        )
        .unwrap();
    widget.apply_layout(&taffy, node, origin);
    widget.draw_with_presence(
        scene,
        Affine::IDENTITY,
        tr,
        theme,
        &PresenceCtx::default(),
        permissions,
    );
}

fn shoot(theme: &Theme, out: PathBuf) {
    let mut state = ui::State::new();
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let c = &theme.tokens.colors;

    let mut caption = |scene: &mut Scene, tr: &mut TextRenderer, x: f64, y: f64, s: &str| {
        tr.draw_text(scene, s, theme.tokens.typography.scale.sm, c.text_dim, Affine::translate((x, y)), None);
    };
    caption(&mut scene, &mut tr, COL_ENABLED, 16.0, "granted");
    caption(&mut scene, &mut tr, COL_DENIED, 16.0, "denied");

    let allow = PermissionContext::permissive();
    let deny = denying();

    for (i, ctx) in [(&allow), (&deny)].into_iter().enumerate() {
        let x = if i == 0 { COL_ENABLED } else { COL_DENIED };

        // Accent-filled control: white label on the accent.
        let mut primary = Button::new("d_primary", "Publish", None)
            .primary()
            .with_permission_scope(locked());
        stamp(&mut scene, &mut tr, theme, &mut state, &mut primary, Point::new(x, 44.0), ctx);

        // Neutral control: primary text on the raised card.
        let mut secondary =
            Button::new("d_secondary", "Save draft", None).with_permission_scope(locked());
        stamp(&mut scene, &mut tr, theme, &mut state, &mut secondary, Point::new(x, 108.0), ctx);

        let mut check = Checkbox::new("d_check", "Share with peers").with_permission_scope(locked());
        stamp(&mut scene, &mut tr, theme, &mut state, &mut check, Point::new(x, 172.0), ctx);

        let mut toggle = Toggle::new("d_toggle", "Sync").with_permission_scope(locked());
        stamp(&mut scene, &mut tr, theme, &mut state, &mut toggle, Point::new(x, 228.0), ctx);
    }

    let mut r = OffscreenRenderer::new(WIDTH, HEIGHT);
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {}", out.display());
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let dir = std::env::var("DENIED_SHOTS_OUT").unwrap_or_else(|_| "/tmp/denied_shots".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let dir = PathBuf::from(dir);
    let themes = std::env::var("DENIED_SHOTS_THEMES")
        .unwrap_or_else(|_| "Dark,Light,High Contrast".to_string());
    for name in themes.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let theme = Theme::named(name).unwrap_or_else(|| panic!("unknown theme {name:?}"));
        let slug: String = name
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' })
            .collect();
        shoot(&theme, dir.join(format!("denied-{slug}.png")));
    }
}
