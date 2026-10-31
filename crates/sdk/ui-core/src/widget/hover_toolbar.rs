//! [`HoverToolbar`]: controls shown over the trailing top corner of a row while it is hovered
//! or holds focus: a message's quick reactions and Reply.
//!
//! The toolbar is laid out with the row (a layer straddling the row's top edge at its trailing
//! corner, so a one-line message's text stays clear), so showing it moves nothing. It comes the moment the pointer is over the row
//! ([`crate::motion::Transient::CONFIRMATION`]) and fades when it leaves; while hidden it takes
//! no pointer input, but it stays in the focus order: Tab from the row reaches its controls,
//! and focus in them keeps it shown. Showing and hiding it damages only the toolbar.

use std::cell::Cell;
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::motion::{Presence, Transient};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{MotionSource, Rhythm};
use crate::widget::arrange::{At, Layers, Row};
use crate::widget::context_menu_area::focus_within;
use crate::widget::{Decorator, Element, EventCtx, Widget};

/// How the toolbar is shown, shared by the row that decides and the toolbar that draws.
#[derive(Default)]
struct Shown {
    /// How far it is shown (`0..=1`), as last drawn.
    openness: Cell<f64>,
    /// It changed since it was last drawn.
    changed: Cell<bool>,
}

/// A row with controls shown over its trailing top corner while it is hovered or holds focus.
#[crate::component::not_a_component(part, "shown by the `ui.HoverToolbar` attachment")]
pub struct HoverToolbar {
    layers: Layers,
    shown: Rc<Shown>,
    presence: Presence,
    hovered: bool,
    focused: bool,
    /// The person uses a touchscreen: nothing hovers, so only focus shows the toolbar (a long
    /// press offers the same on the row's menu).
    touch: bool,
}

impl HoverToolbar {
    /// `row`, with `controls` shown over it on hover and focus.
    #[must_use]
    pub fn new(row: Element, controls: Vec<Element>) -> Self {
        let shown = Rc::new(Shown::default());
        let mut bar = Row::row().gap(Rhythm::Tight);
        for control in controls {
            bar = bar.push(control);
        }
        let face = ToolbarFace {
            // A floating surface, as every control that stands over content.
            bar: Box::new(
                crate::widget::Surface::new(crate::widget::SurfaceLevel::Floating, Box::new(bar))
                    .padded(Rhythm::Tight),
            ),
            shown: Rc::clone(&shown),
        };
        let layers = Layers::layers()
            .push(row)
            // Over the row's top edge, half above it: a one-line message keeps its text clear.
            .push_straddling(Box::new(face), At::TopTrailing);
        Self {
            layers,
            shown,
            presence: Presence::closed(),
            hovered: false,
            focused: false,
            touch: false,
        }
    }

    /// Whether the toolbar is shown now (or on its way).
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.presence.is_open()
    }

    /// The row under the toolbar.
    #[must_use]
    pub fn row(&self) -> Option<&dyn Widget> {
        self.layers.children().into_iter().next()
    }

    /// Show it while the row is hovered or holds focus.
    fn follow(&mut self, theme: Option<&Theme>) {
        let want = self.hovered || self.focused;
        let now = crate::motion::frame_now();
        if want != self.presence.is_open() {
            let motion = theme.map_or_else(crate::tokens::EffectiveMotion::default, Theme::motion);
            let source = if self.hovered {
                MotionSource::Pointer
            } else {
                MotionSource::Keyboard
            };
            self.presence
                .show(want, Transient::CONFIRMATION, motion, source, now);
        }
        let openness = self.presence.progress(now);
        if (openness - self.shown.openness.get()).abs() > f64::EPSILON {
            self.shown.openness.set(openness);
            self.shown.changed.set(true);
        }
    }
}

impl Decorator for HoverToolbar {
    type Inner = Layers;

    fn inner(&self) -> &Layers {
        &self.layers
    }

    fn inner_mut(&mut self) -> &mut Layers {
        &mut self.layers
    }

    fn name(&self) -> &'static str {
        "HoverToolbar"
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let at: Point = ctx.logical(position);
                self.hovered = !self.touch && self.layers.get_rect().contains(at);
            }
            WindowEvent::CursorLeft { .. } => self.hovered = false,
            // A finger: the pointer the platform makes of it hovers nothing.
            WindowEvent::Touch(_) => {
                self.touch = true;
                self.hovered = false;
            }
            _ => {}
        }
        self.layers.handle_event(event, ctx);
        self.focused = focus_within(&self.layers);
        self.follow(None);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.focused = focus_within(&self.layers);
        self.follow(Some(theme));
        self.layers.draw(scene, transform, text, theme, presence);
    }

    fn needs_animation_frame(&self) -> bool {
        self.presence.is_moving(crate::motion::frame_now()) || self.layers.needs_animation_frame()
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.touch = environment.modality == crate::interaction::InputModality::Touch;
        if self.touch {
            self.hovered = false;
        }
        self.layers.set_interaction_environment(environment);
    }
}

/// The toolbar as drawn: faded in and out with the row's hover, taking the pointer only while
/// shown.
struct ToolbarFace {
    bar: Element,
    shown: Rc<Shown>,
}

impl ToolbarFace {
    fn visible(&self) -> bool {
        self.shown.openness.get() > 0.0
    }
}

impl Decorator for ToolbarFace {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.bar.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.bar.as_mut()
    }

    fn name(&self) -> &'static str {
        "HoverToolbarFace"
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
        );
        // Hidden, it takes no pointer; the keyboard reaches it, which shows it.
        if pointer && !self.visible() {
            return;
        }
        self.bar.handle_event(event, ctx);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let openness = self.shown.openness.get();
        if openness <= 0.0 {
            return;
        }
        if openness >= 1.0 {
            self.bar.draw(scene, transform, text, theme, presence);
            return;
        }
        let rect = self.bar.get_rect();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "an opacity between 0 and 1"
        )]
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            openness as f32,
            transform,
            &rect,
        );
        self.bar.draw(scene, transform, text, theme, presence);
        scene.pop_layer();
    }

    fn needs_redraw(&self) -> bool {
        self.shown.changed.get() || (self.visible() && self.bar.needs_redraw())
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.shown.changed.get() {
            regions.push(self.bar.get_rect());
        } else if self.visible() {
            self.bar.collect_damage(regions);
        }
    }

    fn mark_drawn(&mut self) {
        self.shown.changed.set(false);
        self.bar.mark_drawn();
    }
}

#[cfg(test)]
#[path = "hover_toolbar_tests.rs"]
mod tests;
