#![allow(
    clippy::exit,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Offscreen renders of the [`Scrollable`] overlay scrollbar in a long list,
//! in **both** light and dark, for self-inspection without opening a window.
//! Drives the real five-pass pipeline (`sync` → `build_layout` →
//! `compute_layout` → `apply_layout` → `draw`) plus a real `CursorMoved`
//! event to exercise the hovered/active thumb state.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- scrollbar_shots`
//! Out:  `$SCROLLBAR_SHOTS_OUT` (default `/tmp/scrollbar_shots`) —
//!       scrollbar-{dark,light}-{idle,active,both}.png
//!
//! Each shot scrolls the list a little so the thumb sits mid-track:
//!   * `idle`   — the resting overlay thumb (neutral, no track chrome).
//!   * `active` — pointer parked on the thumb: brightened + track revealed.
//!   * `both`   — content overflowing on both axes: vertical + horizontal bars.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use tilo_ui::State;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Column, Row, ScrollAxis, Scrollable, Text, Widget};

const WIDTH: u32 = 460;
const HEIGHT: u32 = 620;
/// `WIDTH`/`HEIGHT` are small fixed layout constants, well inside `f32`'s
/// exact-integer range (2^24).
#[allow(
    clippy::cast_precision_loss,
    reason = "WIDTH/HEIGHT are small fixed constants, far below f32's exact-integer range"
)]
const WIDTH_F32: f32 = WIDTH as f32;
#[allow(
    clippy::cast_precision_loss,
    reason = "WIDTH/HEIGHT are small fixed constants, far below f32's exact-integer range"
)]
const HEIGHT_F32: f32 = HEIGHT as f32;

fn long_list(both: bool) -> Box<dyn Widget> {
    let mut col = Column::new().with_intrinsic_height();
    for i in 0..80usize {
        if both {
            // A wide grid row: several intrinsic-width cells laid out
            // horizontally (a Row never wraps its children), so the total
            // width exceeds the viewport and the horizontal bar appears.
            let mut r = Row::new();
            for c in 0..10 {
                r = r.push(Box::new(Text::new(&format!(
                    "r{i:02}c{c}: tilo-fs.service"
                ))));
            }
            col = col.push(Box::new(r));
        } else {
            col = col.push(Box::new(Text::new(&format!(
                "{i:>3}  2026-07-09T12:{:02}:{:02}Z  systemd[1]: Started Tilo session service.",
                i % 60,
                i.saturating_mul(7).checked_rem(60).unwrap_or(0)
            ))));
        }
    }
    Box::new(col)
}

fn cursor_at(p: Point) -> winit::event::WindowEvent {
    winit::event::WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

fn shoot(theme: &Theme, out: &Path, active: bool, both: bool) {
    let mut state = State::new();
    let axis = if both {
        ScrollAxis::Both
    } else {
        ScrollAxis::Vertical
    };
    let mut scroll = Scrollable::new(axis)
        .with_child(long_list(both))
        .with_height(HEIGHT_F32);

    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    scroll.sync(&mut state.doc, &state.root);
    let node = scroll.build_layout(&mut taffy, theme, &mut node_map);
    let layout_result = taffy.compute_layout_with_measure(
        node,
        Size {
            width: AvailableSpace::Definite(WIDTH_F32),
            height: AvailableSpace::Definite(HEIGHT_F32),
        },
        |known, available, node_id, _, _| {
            node_map.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                (*p).measure(known, available, &mut tr)
            })
        },
    );
    if let Err(e) = layout_result {
        eprintln!("layout failed: {e}");
        return;
    }

    // A first layout populates the content/viewport extents that `scroll_by`
    // clamps against; then scroll into the middle so the thumb sits mid-track
    // (which wakes the overlay bar to full opacity); a second layout pushes
    // the new offset down to the children.
    scroll.apply_layout(&taffy, node, Point::ZERO);
    scroll.scroll_by(0.0, 700.0);
    if both {
        scroll.scroll_by(320.0, 0.0);
    }
    scroll.apply_layout(&taffy, node, Point::ZERO);

    if active {
        // Park the pointer on the vertical thumb so it renders in its
        // brightened, track-revealed state (the real hover path).
        if let Some(thumb) = scroll.thumb_rect(ScrollAxis::Vertical) {
            let c = Point::new(thumb.x0.midpoint(thumb.x1), thumb.y0.midpoint(thumb.y1));
            let mut handled = false;
            let mut ft = None;
            scroll.handle_event(
                &cursor_at(c),
                &mut tilo_ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: &mut state.doc,
                    path: &state.root,
                    handled: &mut handled,
                    focus_target: &mut ft,
                    node_id: node,
                    taffy: &taffy,
                },
            );
        }
    }

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    scroll.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, HEIGHT);
    r.render_png(&scene, theme.bg_app, out);
    println!("wrote {}", out.display());
}

pub fn run(_args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let dir =
        std::env::var("SCROLLBAR_SHOTS_OUT").unwrap_or_else(|_| "/tmp/scrollbar_shots".to_string());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("failed to create output dir {dir}: {e}");
        return;
    }
    let dir = PathBuf::from(dir);

    for (theme, tag) in shot_themes() {
        shoot(
            &theme,
            &dir.join(format!("scrollbar-{tag}-idle.png")),
            false,
            false,
        );
        shoot(
            &theme,
            &dir.join(format!("scrollbar-{tag}-active.png")),
            true,
            false,
        );
        shoot(
            &theme,
            &dir.join(format!("scrollbar-{tag}-both.png")),
            true,
            true,
        );
    }
}

/// The palettes to shoot: `TILO_SHOTS_THEME` names one (its tag is the name,
/// lower-cased, non-alphanumerics as `-`); unset shoots dark and light. An
/// unknown name is an error, never an empty run that exits 0.
fn shot_themes() -> Vec<(Theme, String)> {
    match std::env::var("TILO_SHOTS_THEME") {
        Ok(name) => {
            let Some(theme) = Theme::named(&name) else {
                eprintln!("ui-shots: unknown TILO_SHOTS_THEME `{name}`");
                std::process::exit(2)
            };
            let tag = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect();
            vec![(theme, tag)]
        }
        Err(_) => vec![
            (Theme::dark(), "dark".to_string()),
            (Theme::light(), "light".to_string()),
        ],
    }
}
