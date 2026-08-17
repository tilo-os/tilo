//! Offscreen renders of the [`Scrollable`] overlay scrollbar in a long list,
//! in **both** light and dark, for self-inspection without opening a window.
//! Drives the real five-pass pipeline (sync → build_layout → compute_layout →
//! apply_layout → draw) plus a real `CursorMoved` event to exercise the
//! hovered/active thumb state.
//!
//! Run:  cargo run -p ui --example scrollbar_shots
//! Out:  $SCROLLBAR_SHOTS_OUT (default /tmp/scrollbar_shots) —
//!       scrollbar-{dark,light}-{idle,active,both}.png
//!
//! Each shot scrolls the list a little so the thumb sits mid-track:
//!   * `idle`   — the resting overlay thumb (neutral, no track chrome).
//!   * `active` — pointer parked on the thumb: brightened + track revealed.
//!   * `both`   — content overflowing on both axes: vertical + horizontal bars.
//!   * `glide`  — a smooth wheel scroll caught mid-ease: the rendered offset is
//!                still interpolating toward its (further) target, overlay lit.

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
use ui::widget::{Column, Row, ScrollAxis, Scrollable, Text, Widget};

const WIDTH: u32 = 460;
const HEIGHT: u32 = 620;

fn long_list(both: bool) -> Box<dyn Widget> {
    let mut col = Column::new().with_intrinsic_height();
    for i in 0..80 {
        if both {
            // A wide grid row: several intrinsic-width cells laid out
            // horizontally (a Row never wraps its children), so the total
            // width exceeds the viewport and the horizontal bar appears.
            let mut r = Row::new();
            for c in 0..10 {
                r = r.push(Box::new(Text::new(&format!("r{i:02}c{c}: qos-fs.service"))));
            }
            col = col.push(Box::new(r));
        } else {
            col = col.push(Box::new(Text::new(&format!(
                "{i:>3}  2026-07-09T12:{:02}:{:02}Z  systemd[1]: Started qOS session service.",
                i % 60,
                (i * 7) % 60
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

#[derive(Clone, Copy, PartialEq)]
enum Motion {
    /// Settled: the offset has fully arrived at its scroll position.
    Rest,
    /// Mid-ease: a wheel flick just landed and the offset is still gliding
    /// toward a further target (exercises the smooth-scroll path).
    Glide,
}

fn shoot(theme: &Theme, out: PathBuf, active: bool, both: bool, motion: Motion) {
    let mut state = State::new();
    let axis = if both { ScrollAxis::Both } else { ScrollAxis::Vertical };
    let mut scroll = Scrollable::new(axis)
        .with_child(long_list(both))
        .with_height(HEIGHT as f32);

    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    scroll.sync(&mut state.doc, &state.root);
    let node = scroll.build_layout(&mut taffy, theme, &mut node_map);
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

    // A first layout populates the content/viewport extents that `scroll_by`
    // clamps against; then scroll into the middle so the thumb sits mid-track
    // (which wakes the overlay bar to full opacity); a second layout pushes
    // the new offset down to the children.
    scroll.apply_layout(&taffy, node, Point::ZERO);
    match motion {
        Motion::Rest => {
            scroll.scroll_by(0.0, 700.0);
            if both {
                scroll.scroll_by(320.0, 0.0);
            }
        }
        Motion::Glide => {
            // A smooth wheel flick: push the target well ahead, then advance a
            // single ease frame so the rendered offset is caught mid-glide
            // (visibly short of its target) with the overlay lit by the scroll.
            scroll.smooth_scroll_by(0.0, 900.0);
            scroll.ease_step();
        }
    }
    scroll.apply_layout(&taffy, node, Point::ZERO);

    if active {
        // Park the pointer on the vertical thumb so it renders in its
        // brightened, track-revealed state (the real hover path).
        if let Some(thumb) = scroll.thumb_rect(ScrollAxis::Vertical) {
            let c = Point::new((thumb.x0 + thumb.x1) / 2.0, (thumb.y0 + thumb.y1) / 2.0);
            let mut handled = false;
            let mut ft = None;
            scroll.handle_event(
                &cursor_at(c),
                1.0,
                &winit::event::Modifiers::default(),
                &mut state.doc,
                &state.root,
                &mut handled,
                &mut ft,
                node,
                &taffy,
            );
        }
    }

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    scroll.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, HEIGHT);
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {}", out.display());
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let dir = std::env::var("SCROLLBAR_SHOTS_OUT")
        .unwrap_or_else(|_| "/tmp/scrollbar_shots".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let dir = PathBuf::from(dir);

    for (theme, tag) in [(Theme::dark(), "dark"), (Theme::light(), "light")] {
        shoot(&theme, dir.join(format!("scrollbar-{tag}-idle.png")), false, false, Motion::Rest);
        shoot(&theme, dir.join(format!("scrollbar-{tag}-active.png")), true, false, Motion::Rest);
        shoot(&theme, dir.join(format!("scrollbar-{tag}-both.png")), true, true, Motion::Rest);
        shoot(&theme, dir.join(format!("scrollbar-{tag}-glide.png")), false, false, Motion::Glide);
    }
}
