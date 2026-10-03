//! `SeekBar` — the SponsorBlock-aware scrubber.
//!
//! This is the differentiating widget of the whole app: the seek bar is
//! painted with the community `SponsorBlock` segments inline, each in its
//! canonical category colour, so before you even press play you can see
//! "there's a 90-second sponsor read at the 24-minute mark". Click or drag
//! anywhere to seek; the root auto-skips enabled categories as the
//! playhead crosses them.
//!
//! Track / played / knob colours come from the OS [`Theme`]; only the
//! segment bands use `SponsorBlock`'s brand colours (documented in
//! [`crate::model::Category::rgb`]), because matching what users already
//! recognise is the whole point.

use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::action::{self, Action, ActionBus};
use crate::model::Segment;

pub struct SeekBar {
    duration: f64,
    playhead: f64,
    segments: Vec<Segment>,
    enabled: Vec<bool>, // parallel to segments — whether that category is active
    bus: ActionBus,
    rect: Rect,
    hovered: bool,
    dragging: bool,
    dirty: bool,
    test_id: Option<String>,
}

impl SeekBar {
    pub fn new(
        bus: &ActionBus,
        duration: f64,
        playhead: f64,
        segments: Vec<Segment>,
        enabled: Vec<bool>,
    ) -> Self {
        Self {
            duration: duration.max(1.0),
            playhead,
            segments,
            enabled,
            bus: bus.clone(),
            rect: Rect::ZERO,
            hovered: false,
            dragging: false,
            dirty: true,
            test_id: None,
        }
    }

    /// Stable, addressable id so headless tests can resolve the scrubber's
    /// laid-out rect and drive a real drag-to-seek through it.
    #[must_use]
    pub fn id(mut self, id: &str) -> Self {
        self.test_id = Some(id.to_string());
        self
    }

    fn fraction_at(&self, x: f64) -> f64 {
        ((x - self.rect.x0) / self.rect.width().max(1.0)).clamp(0.0, 1.0)
    }
}

impl Widget for SeekBar {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = super::leaf_node(
            taffy,
            Style {
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: length(26.0_f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = super::layout_rect(taffy, node, origin);
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "conventional short names for the track geometry (x0/x1: span, w: width, \
                  cy: vertical center) used consistently across this module's draw code"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let cy = self.rect.center().y;
        let track_h = if self.hovered || self.dragging {
            8.0
        } else {
            6.0
        };
        let x0 = self.rect.x0;
        let x1 = self.rect.x1;
        let w = (x1 - x0).max(1.0);

        // Track.
        let track = RoundedRect::new(
            x0,
            cy - track_h / 2.0,
            x1,
            cy + track_h / 2.0,
            track_h / 2.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(255, 255, 255, 38),
            None,
            &track,
        );

        // Played portion (theme accent).
        let frac = (self.playhead / self.duration).clamp(0.0, 1.0);
        let played = RoundedRect::new(
            x0,
            cy - track_h / 2.0,
            x0 + w * frac,
            cy + track_h / 2.0,
            track_h / 2.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.accent_primary,
            None,
            &played,
        );

        // SponsorBlock segment bands, in their category colours.
        for (i, seg) in self.segments.iter().enumerate() {
            let s = (seg.start / self.duration).clamp(0.0, 1.0);
            let e = (seg.end / self.duration).clamp(0.0, 1.0);
            if e <= s {
                continue;
            }
            let (r, g, b) = seg.category.rgb();
            let active = self.enabled.get(i).copied().unwrap_or(true);
            let alpha = if active { 255 } else { 110 };
            // Bands sit a few px taller than the track so the SponsorBlock
            // segments read clearly even when short.
            let bh = track_h / 2.0 + 3.0;
            let band = RoundedRect::new(
                x0 + w * s,
                cy - bh,
                (x0 + w * e).max(x0 + w * s + 2.0),
                cy + bh,
                2.0,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(r, g, b, alpha),
                None,
                &band,
            );
        }

        // Playhead knob.
        let kx = x0 + w * frac;
        let kr = if self.hovered || self.dragging {
            8.0
        } else {
            6.0
        };
        scene.fill(
            Fill::NonZero,
            transform,
            Color::WHITE,
            None,
            &Circle::new(Point::new(kx, cy), kr),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.accent_primary,
            None,
            &Circle::new(Point::new(kx, cy), kr - 2.5),
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let hov = self.rect.inflate(0.0, 6.0).contains(p);
                if hov != self.hovered {
                    self.hovered = hov;
                    self.dirty = true;
                }
                if self.dragging {
                    let f = self.fraction_at(p.x);
                    action::push(&self.bus, Action::Seek(f));
                    self.playhead = f * self.duration;
                    *ctx.handled = true;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.hovered {
                    self.dragging = true;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.dragging {
                    *ctx.handled = true;
                }
                self.dragging = false;
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.dragging
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    fn name(&self) -> &'static str {
        "SeekBar"
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}
