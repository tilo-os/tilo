//! [`Magnify`]: one item of a dock that grows under the pointer, and settles back when it
//! leaves, so a run of items reads as a dock rather than a row of buttons.
//!
//! The pointer's place is the toolkit's own ([`last_pointer`]), noted by the host as it hands
//! input to the widgets: the item itself receives no move once the pointer is off it, so a view
//! that tracked hover would stall at its last place.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::kurbo::{Point, Rect};

use tilo_ui_base::component::{Component, Described, PartViews};
use tilo_ui_base::motion::{MotionValue, SpringSpec, Timing, frame_now, last_pointer};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::EffectiveMotion;
use tilo_ui_base::widget::{DrawChild, Element, Spacer, Widget};

/// How far from an item's middle the pointer still makes it grow, in its own heights: three
/// items each way, as a dock's icons reach.
const REACH_HEIGHTS: f64 = 3.5;

/// The smallest reach, in logical pixels: an item smaller than a control should not magnify
/// only at the pixel under the pointer.
const REACH_MIN: f64 = 64.0;

/// How long the growth takes to settle after the pointer stops: short enough that the item
/// answers the pointer, long enough to read as motion rather than as a jump.
const SETTLE_MS: u64 = 150;

/// An item of a dock that magnifies under the pointer.
#[derive(Component, Default)]
#[component(name = "Magnify", role = "Group")]
pub struct MagnifyProps {
    /// How much larger the item under the pointer stands: `0.6` makes it a little over half
    /// again as large at its middle, falling away to its own size a few items off, and `0` leaves
    /// it its size.
    pub zoom: f64,
    /// What it holds: one item's face.
    #[component(part(default))]
    pub body: Option<Element>,
}

pub struct Magnify {
    zoom: f64,
    child: Element,
    /// How much larger than its own size the item stands this frame, and the motion taking it
    /// there.
    extra: MotionValue<f64>,
    /// What the child was last laid at, so a frame that moved it is laid out again.
    applied: f64,
    /// Where the item rests: the pointer's distance is measured to this, not to the grown item,
    /// so its own growth never chases the pointer that caused it.
    rest: Rect,
    /// The pointer left the window since it was last seen: nothing magnifies until it is back.
    away: bool,
    motion: EffectiveMotion,
    interaction: tilo_ui_base::interaction::InteractionEnvironment,
    rect: Rect,
    dirty: bool,
}

impl Magnify {
    fn new(zoom: f64, child: Element) -> Self {
        Self {
            zoom: zoom.clamp(0.0, 3.0),
            child,
            extra: MotionValue::new(0.0),
            applied: 0.0,
            rest: Rect::ZERO,
            away: false,
            motion: EffectiveMotion::default(),
            interaction: tilo_ui_base::interaction::InteractionEnvironment::default(),
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// How much larger the item is now, at the pointer: 0 at rest, `zoom` at its middle.
    fn target(&self) -> f64 {
        if self.zoom <= 0.0 || self.away || !self.interaction.supports_hover() {
            return 0.0;
        }
        let Some(pointer) = last_pointer() else {
            return 0.0;
        };
        let rest = if self.rest.width() > 0.0 && self.rest.height() > 0.0 {
            self.rest
        } else {
            self.rect
        };
        if rest.width() <= 0.0 || rest.height() <= 0.0 {
            return 0.0;
        }
        let reach = (rest.width().max(rest.height()) * REACH_HEIGHTS).max(REACH_MIN);
        let distance = (pointer - rest.center()).hypot();
        if distance >= reach {
            return 0.0;
        }
        let near = 1.0 - distance / reach;
        // Smooth at both ends: no edge where an item starts or stops growing.
        let eased = near * near * (3.0 - 2.0 * near);
        self.zoom * eased
    }

    /// Take `view` as what it holds, laid at the growth it stands at now: a rebuilt item must not
    /// fall back to its own size for a frame while the pointer is still over it.
    fn adopt(&mut self, mut view: Element) {
        view.set_visual_scale(1.0 + self.extra.get(frame_now()));
        self.child = view;
        self.dirty = true;
    }

    /// Follow the pointer: set the growth's target and lay the child at what that is now.
    fn follow(&mut self, now: crate::time::Instant) {
        let timing = if self.motion.still() {
            Timing::Instant
        } else {
            Timing::Spring(SpringSpec::settling_within(
                crate::time::Duration::from_millis(SETTLE_MS),
            ))
        };
        self.extra.set(self.target(), timing, now);
        let scale = self.extra.get(now);
        if (scale - self.applied).abs() > 1e-4 {
            self.applied = scale;
            self.child.set_visual_scale(1.0 + scale);
            self.dirty = true;
            // The item's own size follows the growth, so the run it stands in makes room for it.
            tilo_ui_base::widget::note_layout_moved();
        }
    }
}

impl Described for Magnify {
    type Props = MagnifyProps;

    fn from_props(_key: &str, props: MagnifyProps) -> Self {
        let MagnifyProps { zoom, body } = props;
        let child = body.unwrap_or_else(|| {
            Box::new(Spacer::token(tilo_ui_base::tokens::SpacingStep::S0)) as Element
        });
        Self::new(zoom, child)
    }

    fn set_props(&mut self, props: MagnifyProps) -> bool {
        self.zoom = props.zoom.clamp(0.0, 3.0);
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == "body").then_some(self as &mut dyn PartViews)
    }
}

impl PartViews for Magnify {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        (index == 0).then_some(&mut self.child)
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.adopt(view);
    }

    fn remove(&mut self, _index: usize) {
        self.child = Box::new(Spacer::token(tilo_ui_base::tokens::SpacingStep::S0));
        self.dirty = true;
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.adopt(view);
    }
}

impl Widget for Magnify {
    tilo_ui_base::described!();

    fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
        self.child.set_interaction_environment(environment);
    }

    fn update(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        let now = frame_now();
        self.follow(now);
        let moving = self.extra.is_moving(now);
        let child = self.child.update(state, path);
        moving || child
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.motion = theme.motion();
        let now = frame_now();
        self.follow(now);
        let scale = 1.0 + self.extra.get(now);
        self.applied = self.extra.get(now);
        self.child.set_visual_scale(scale);
        self.child.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: NodeId, origin: Point) {
        self.child.apply_layout(taffy, node, origin);
        let rect = self.child.get_rect();
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
        // Laid at its own size, this is where the item rests: every pointer is measured from
        // here, so the item's growth never moves the place it is measured against.
        if self.applied <= 1e-4 {
            self.rest = rect;
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, env) = ctx.parts();
        self.child.draw_child(scene, transform, text, theme, &env);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        match event {
            winit::event::WindowEvent::CursorMoved { .. } => self.away = false,
            winit::event::WindowEvent::CursorLeft { .. } => {
                self.away = true;
                self.dirty = true;
            }
            _ => {}
        }
        self.child.handle_event(event, ctx);
    }

    fn needs_animation_frame(&self) -> bool {
        self.extra.is_moving(frame_now()) || self.child.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.needs_animation_frame() || self.child.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.child.mark_drawn();
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty || self.child.is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.is_layout_dirty()
    }

    fn layout_measured(&mut self) {
        self.dirty = false;
    }

    fn take_structure_dirty(&mut self) -> bool {
        tilo_ui_base::widget::defaults::take_structure_dirty(self.children_mut())
    }

    fn name(&self) -> &'static str {
        "Magnify"
    }
}
