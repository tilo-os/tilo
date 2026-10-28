//! Offscreen renders of [`QuickTile`] in its states: off, on, with a
//! section closed and open, under the pointer on either half, focused on
//! either half, not enabled, and with a long name and state line, in dark
//! and light, wide and compact (420 px, compact density).
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- quick_tile_shots [out-dir]`
//! Out:  `out-dir` (default `target/quick_tile_shots`): quick-tile-{dark,light}-{wide,compact}.png

use std::path::{Path, PathBuf};

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use tilo_ui::icons::Icon;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::tokens::Density;
use tilo_ui::widget::{QuickTile, QuickTilePart, Widget};

/// What each tile shows and which state it is put in.
enum Pose {
    Rest,
    HoverSwitch,
    HoverDetails,
    FocusSwitch,
    FocusDetails,
}

fn tiles() -> Vec<(QuickTile, Pose)> {
    let wifi = || {
        QuickTile::new("wifi", Icon::Wifi, "Wi-Fi")
            .detail("Home")
            .on(true)
            .with_details("Network details")
    };
    let mut open = wifi();
    open.set_details_open(1.0);
    vec![
        (
            QuickTile::new("night", Icon::Moon, "Night light").detail("Off"),
            Pose::Rest,
        ),
        (
            QuickTile::new("dnd", Icon::BellOff, "Do not disturb")
                .detail("On")
                .on(true),
            Pose::Rest,
        ),
        (wifi(), Pose::Rest),
        (open, Pose::Rest),
        (
            QuickTile::new("bt", Icon::Bluetooth, "Bluetooth")
                .detail("Not connected")
                .with_details("Bluetooth details"),
            Pose::HoverSwitch,
        ),
        (wifi(), Pose::HoverDetails),
        (
            QuickTile::new("vpn", Icon::Lock, "VPN")
                .detail("Off")
                .with_details("VPN details"),
            Pose::FocusSwitch,
        ),
        (wifi(), Pose::FocusDetails),
        (
            QuickTile::new("air", Icon::Plane, "Airplane mode")
                .detail("Turning on")
                .enabled(false),
            Pose::Rest,
        ),
        (
            QuickTile::new("long", Icon::Wifi, "Wi-Fi")
                .detail("The network at the far end of the long corridor upstairs")
                .on(true)
                .with_details("Network details"),
            Pose::Rest,
        ),
    ]
}

#[allow(
    clippy::cast_precision_loss,
    reason = "tile indices and pixel sizes are small"
)]
fn shoot(theme: &Theme, width: u32, out: &Path) {
    let columns: usize = if width >= 600 { 2 } else { 1 };
    let pad = f64::from(theme.tokens.spacing.s4);
    let gap = f64::from(theme.tokens.spacing.s3);
    let height = QuickTile::height(theme);
    let tile_w = (f64::from(width) - 2.0 * pad - gap * (columns as f64 - 1.0)) / columns as f64;
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    let mut tiles = tiles();
    let rows = tiles.len().div_ceil(columns);
    for (index, (tile, pose)) in tiles.iter_mut().enumerate() {
        let x = pad + index.checked_rem(columns).unwrap_or(0) as f64 * (tile_w + gap);
        let y = pad + index.checked_div(columns).unwrap_or(0) as f64 * (height + gap);
        tile.apply_layout_external(Rect::new(x, y, x + tile_w, y + height));
        // A first draw takes the theme's sizes, which split the tile.
        tile.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut tr,
            theme,
            &presence,
        );
        let (switch, details) = tile.parts();
        match pose {
            Pose::Rest => {}
            Pose::HoverSwitch => tile.set_pointer(Some(switch.center())),
            Pose::HoverDetails => tile.set_pointer(details.map(|d| d.center())),
            Pose::FocusSwitch => tile.set_focused_part(Some(QuickTilePart::Switch)),
            Pose::FocusDetails => tile.set_focused_part(Some(QuickTilePart::Details)),
        }
        tile.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small positive pixel count"
    )]
    let total = (2.0 * pad + rows as f64 * (height + gap)).ceil() as u32;
    let mut renderer = OffscreenRenderer::new(width, total);
    renderer.render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {}", out.display());
}

pub fn run(args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let dir = PathBuf::from(
        args.first()
            .cloned()
            .unwrap_or_else(|| "target/quick_tile_shots".to_string()),
    );
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("quick_tile_shots: failed to create {}: {e}", dir.display());
        return;
    }
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        shoot(
            &theme,
            960,
            &dir.join(format!("quick-tile-{name}-wide.png")),
        );
        shoot(
            &theme.with_density(Density::Compact),
            420,
            &dir.join(format!("quick-tile-{name}-compact.png")),
        );
    }
}
