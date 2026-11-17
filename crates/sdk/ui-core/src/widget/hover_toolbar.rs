//! [`HoverToolbar`] — controls shown over the trailing top corner of a row while it is
//! hovered or holds focus: a message's quick reactions and Reply.

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
use crate::widget::DrawChild;
use crate::widget::arrange::{Align, At, Layers, Row, Stack};
use crate::widget::context_menu_area::focus_within;
use crate::widget::{Decorator, Element, EventCtx, Widget};

/// Where a row's controls stand.
#[derive(Clone, Copy)]
enum Stands {
    /// Centred on the row's top edge at its trailing corner.
    Straddling,
    /// In a band of their own this tall over the row, at its trailing end.
    Above(f64),
}

/// How the toolbar is shown, shared by the row that decides and the toolbar that draws.
#[derive(Default)]
struct Shown {
    openness: Cell<f64>,
    changed: Cell<bool>,
}

/// A row with controls shown over its trailing top corner while it is hovered or holds
/// focus.
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
    /// Where its controls stand, which says what focus shows them.
    stands: Stands,
}

impl HoverToolbar {
    /// `row`, with `controls` shown over it on hover and focus.
    #[must_use]
    pub fn new(row: Element, controls: Vec<Element>) -> Self {
        Self::around(
            crate::widget::arrange::Part::new(row),
            controls,
            Stands::Straddling,
            None,
        )
    }

    /// `row` filling the room it is given under a band `band` logical pixels tall its controls
    /// are shown in — content at a size of its own, a place on a canvas — with
    /// `controls` at the band's trailing end, over the content's top edge rather than on it, on
    /// hover and focus: the band is kept whether they are shown or not, so showing them covers
    /// nothing of the content and moves nothing, and the pointer that went to them is still over
    /// what they belong to.
    #[must_use]
    pub fn above(row: Element, controls: Vec<Element>, band: f64) -> Self {
        let mut part = crate::widget::arrange::Part::new(row);
        part.fit = crate::widget::arrange::Fit::fill(1.0);
        Self::around(part, controls, Stands::Above(band), None)
    }

    /// [`Self::above`], with `caption` at the band's leading end at rest as well as on
    /// hover: what the content is, said quietly while its controls are away (a place on a canvas
    /// names what it shows). The band is as tall either way, so nothing moves when the controls
    /// come; a caption longer than a line wraps, and the band grows to say it in full.
    #[must_use]
    pub fn above_captioned(
        row: Element,
        controls: Vec<Element>,
        band: f64,
        caption: Element,
    ) -> Self {
        let mut part = crate::widget::arrange::Part::new(row);
        part.fit = crate::widget::arrange::Fit::fill(1.0);
        Self::around(part, controls, Stands::Above(band), Some(caption))
    }

    /// The row in `row`, with `controls` shown on hover and focus where `stands` says, and
    /// `caption` at rest in a band over it.
    fn around(
        row: crate::widget::arrange::Part,
        controls: Vec<Element>,
        stands: Stands,
        caption: Option<Element>,
    ) -> Self {
        let shown = Rc::new(Shown::default());
        let mut bar = Row::row().gap(Rhythm::Tight);
        for control in controls {
            bar = bar.push(control);
        }
        // A floating surface, as every control that stands over content.
        let surface: Element = Box::new(
            crate::widget::Surface::new(crate::widget::SurfaceLevel::Floating, Box::new(bar))
                .padded(Rhythm::Tight),
        );
        let face = ToolbarFace {
            bar: surface,
            shown: Rc::clone(&shown),
        };
        let layers = match stands {
            // Over the row's top edge, half above it: a one-line message keeps its text clear.
            Stands::Straddling => Layers::layers()
                .push_part(row)
                .push_straddling(Box::new(face), At::TopTrailing),
            // The band over the content, the controls at its trailing end, a tight step above
            // the content's edge.
            Stands::Above(height) => {
                // The band is as tall as it is told whatever the controls are, so the content
                // under it stands where its pin says; the controls sit at its trailing end, on
                // its foot. A caption longer than a line wraps, and the band grows down to hold
                // all of it: the caption is said in full, never cut short.
                let mut bar = Stack::row()
                    .fill_width()
                    .fill_height()
                    .gap(Rhythm::Related)
                    .distribute(crate::widget::arrange::Distribute::End)
                    .align(Align::End);
                // The caption takes the band's room up to the controls, at its leading end,
                // a tight step above the content's edge as the controls stand.
                let captioned = caption.is_some();
                if let Some(caption) = caption {
                    let mut said = crate::widget::arrange::Part::new(caption);
                    said.fit = crate::widget::arrange::Fit::fill(1.0);
                    bar = bar.push_part(said);
                }
                let bar = bar.push(Box::new(face));
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a band a few dozen pixels tall"
                )]
                let tall = crate::widget::arrange::Length::Dp(height as f32);
                let mut band = crate::widget::arrange::Part::new(Box::new(bar));
                let sizing = crate::widget::arrange::Sizing::hug().min(tall);
                band.fit = crate::widget::arrange::Fit::fill(1.0).height(if captioned {
                    sizing
                } else {
                    sizing.max(tall)
                });
                let column = Stack::stack()
                    .fill_width()
                    .fill_height()
                    .gap(Rhythm::None)
                    .push_part(band)
                    .push_part(row);
                let mut whole = crate::widget::arrange::Part::new(Box::new(column));
                whole.fit = crate::widget::arrange::Fit::fill(1.0);
                Layers::layers().push_part(whole)
            }
        };
        Self {
            layers,
            shown,
            presence: Presence::closed(),
            hovered: false,
            focused: false,
            touch: false,
            stands,
        }
    }

    /// Whether focus is where it shows the controls. Over a row (a message), focus anywhere in
    /// the row does, as hover does. Over content of its own in a band (a place on a canvas), only
    /// focus in the controls does: the content's own focus — a field, a list, a text being read —
    /// is the content's, and a handle shown for it would stand over a place at rest. The pointer
    /// shows them, and Tab reaches them, which shows them too.
    fn focus_shows(&self) -> bool {
        match self.stands {
            Stands::Straddling => focus_within(&self.layers),
            Stands::Above(_) => self
                .layers
                .children()
                .into_iter()
                .next()
                .and_then(|column| column.children().into_iter().next())
                .is_some_and(focus_within),
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

    /// What it holds; a page shows its controls while the pointer is over it or focus is in it.
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        self.layers.html(cx);
        cx.attr("data-hover-controls", "");
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let at: Point = ctx.logical(position);
                self.hovered = !self.touch && self.layers.get_rect().contains(at);
            }
            WindowEvent::CursorLeft { .. } => self.hovered = false,
            WindowEvent::Touch(_) => {
                self.touch = true;
                self.hovered = false;
            }
            _ => {}
        }
        self.layers.handle_event(event, ctx);
        self.focused = self.focus_shows();
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
        self.focused = self.focus_shows();
        self.follow(Some(theme));
        self.layers
            .draw_child(scene, transform, text, theme, presence);
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
            self.bar.draw_child(scene, transform, text, theme, presence);
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
        self.bar.draw_child(scene, transform, text, theme, presence);
        scene.pop_layer();
    }

    fn needs_redraw(&self) -> bool {
        self.shown.changed.get()
            || (self.visible() && (self.bar.needs_redraw() || self.bar.needs_animation_frame()))
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
