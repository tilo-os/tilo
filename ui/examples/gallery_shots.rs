//! Offscreen renders of the component gallery, so the showcase can be
//! self-inspected without opening a window. Drives the real five-pass
//! pipeline (sync → build_layout → compute_layout → apply_layout → draw) on
//! the same widget tree the interactive `gallery` example builds.
//!
//! Run:  cargo run -p ui --example ui-shots -- gallery_shots
//! Out:  $GALLERY_SHOTS_OUT (default /tmp/gallery_shots) — gallery-dark.png,
//!       gallery-light.png
//!
//! `GALLERY_SHOTS_THEMES` takes a comma-separated list of theme names (see
//! `Theme::named`) to render instead of the light/dark default — a colour
//! defect that only exists in one polarity is invisible to a two-theme check,
//! so a sweep across every preset is what actually catches one.
//!
//! `GALLERY_SHOTS_HOVER=x,y` sends a real `CursorMoved` to that logical point
//! before drawing, so a shot can show a genuinely hovered control rather than
//! a resting one. It goes through the same `handle_event` path a live window
//! uses — nothing about the hover state is faked for the capture.
//!
//! Fractional HiDPI: set `GALLERY_SHOTS_SCALE=1.5` (or 1.25 / 1.75 / 2.0)
//! to render at that display scale. This reproduces the exact live-window
//! path — a physical-pixel surface sized `logical × scale`, taffy solved
//! at the logical size, and the whole scene composited through
//! `Affine::scale(scale)` — so the PNG is crisp (vector-rasterized) at a
//! fractional factor rather than a blurry upscale of a 1× frame.

use std::collections::HashMap;
use std::path::PathBuf;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use ui::State;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use super::gallery_scene;

const WIDTH: u32 = 1200;
const HEIGHT: u32 = 2300;

fn shoot(theme: &Theme, out: PathBuf, scale: f64) {
    let mut state = State::new();
    let mut root = gallery_scene::build_showcase(&mut state, theme);

    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    // sync pulls the seeded control state out of the CRDT.
    root.sync(&mut state.doc, &state.root);

    // Layout is solved in LOGICAL space (the same WIDTH/HEIGHT regardless
    // of scale) — HiDPI must not change the logical layout, only how many
    // physical pixels each logical unit covers.
    let node = root.build_layout(&mut taffy, theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(WIDTH as f32),
                height: AvailableSpace::Definite(HEIGHT as f32),
            },
            |known, available, node_id, _, _| {
                node_map
                    .get(&node_id)
                    .map(|&p| unsafe { (*p).measure(known, available, &mut tr) })
                    .unwrap_or(Size::ZERO)
            },
        )
        .unwrap();
    root.apply_layout(&taffy, node, Point::ZERO);

    // Optional hover, delivered through the production event path so what the
    // PNG shows is the widget's own hover state, not a capture-only override.
    if let Some((hx, hy)) = hover_point() {
        let mut handled = false;
        let mut focus_target = None;
        root.handle_event(
            &winit::event::WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(hx, hy),
            },
            1.0,
            &winit::event::Modifiers::default(),
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
    }

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    // Composite through a pure scale by the (possibly fractional) factor —
    // exactly what `App` does live with `Affine::scale(window.scale_factor())`.
    root.draw(&mut scene, Affine::scale(scale), &mut tr, theme, &presence);

    // The offscreen target is sized in PHYSICAL pixels: logical × scale.
    let phys_w = ((WIDTH as f64) * scale).round() as u32;
    let phys_h = ((HEIGHT as f64) * scale).round() as u32;
    let mut r = OffscreenRenderer::new(phys_w, phys_h);
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {} ({phys_w}x{phys_h} px @ {scale}x)", out.display());
}

pub fn run(_args: &[String]) {
    ui::i18n::I18nManager::init("en-US");
    let dir =
        std::env::var("GALLERY_SHOTS_OUT").unwrap_or_else(|_| "/tmp/gallery_shots".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let dir = PathBuf::from(dir);

    // Fractional HiDPI factor from the environment (default 1.0). 1.5 / 1.25
    // / 1.75 exercise the crisp fractional-scale render path.
    let scale = std::env::var("GALLERY_SHOTS_SCALE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0);

    match std::env::var("GALLERY_SHOTS_THEMES") {
        Ok(list) if !list.trim().is_empty() => {
            for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let theme = Theme::named(name)
                    .unwrap_or_else(|| panic!("unknown theme {name:?} — see Theme::ALL"));
                let slug: String = name
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() {
                            c.to_ascii_lowercase()
                        } else {
                            '-'
                        }
                    })
                    .collect();
                shoot(&theme, dir.join(format!("gallery-{slug}.png")), scale);
            }
        }
        _ => {
            shoot(&Theme::dark(), dir.join("gallery-dark.png"), scale);
            shoot(&Theme::light(), dir.join("gallery-light.png"), scale);
        }
    }
}

/// `GALLERY_SHOTS_HOVER=x,y` — the logical point to park the cursor on.
fn hover_point() -> Option<(f64, f64)> {
    let raw = std::env::var("GALLERY_SHOTS_HOVER").ok()?;
    let (x, y) = raw.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}
