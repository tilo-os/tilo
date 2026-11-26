//! One view swapped for another whole, where it stands: a stage's page, a slide. The view that
//! was shown stays laid out while it fades out, and the one that replaces it fades in over it
//! (`motion::SWAP`), both at the same place; once the swap has played only the new one stands.
//!
//! A host builds one when a view that declares its content swapped whole (`transition:
//! .cross_fade`) changes what it shows: the reconciler hands the widget it had to
//! [`Swapped::new`] and what it built for the new content beside it. The pace is the view's
//! `motion:`, the theme's own otherwise; a change no hand made, a still theme and reduced motion
//! land at once, and then the old view is not kept at all.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use tilo_ui_base::{widget::AppModel, widget::DrawChild, widget::DrawCtx, widget::Element, widget::EventCtx, widget::ModelPath, widget::TaffyTreeExt, widget::Widget};
use tilo_ui_base::{motion::CrossFade, motion::Feel, motion::frame_now, motion::recent_input};
use tilo_ui_base::theme::Theme;
use crate::time::Instant;
use tilo_ui_base::{tokens::EffectiveMotion, tokens::MotionSource};

#[tilo_ui_base::component::not_a_component(
    framework,
    "a host keeps the view it showed while a whole one is swapped for it"
)]
pub struct Swapped {
    current: Element,
    /// The view that was shown while it fades out, laid out over the current one.
    previous: Option<Element>,
    /// How opaque the outgoing view was drawn when the swap began: a swap that interrupts one
    /// carries the view it caught part-way, never a jump to full.
    previous_opacity: f32,
    /// The swap's progress: 0 at the moment it began, 1 when only the current view stands.
    fade: CrossFade<u64>,
    generation: u64,
    /// The pace the swap plays at, as the view's `motion:` names it.
    feel: Feel,
    theme: Option<Theme>,
    /// A swap the reconciler just made, which starts on the next frame: only then is the theme
    /// known, and the theme is what says how the person has motion set.
    pending: bool,
    /// The weight the view fills its room with (`fill`), so the swap takes it too.
    fill: f32,
    /// Reported to the host when this widget's composition changed: the old view kept, or let go.
    structure_dirty: bool,
    id: Option<String>,
    rect: Rect,
}

impl Swapped {
    /// `previous` (what the view showed) swapped for `current` (what it shows now).
    #[must_use]
    pub fn new(previous: Element, current: Element) -> Self {
        Self {
            current,
            previous: Some(previous),
            previous_opacity: 1.0,
            fade: CrossFade::of_spec(tilo_ui_base::motion::SWAP, 0),
            generation: 1,
            feel: Feel::Standard,
            theme: None,
            pending: true,
            fill: 0.0,
            structure_dirty: true,
            id: None,
            rect: Rect::ZERO,
        }
    }

    /// Swap what is shown for `next`, the view showing now fading out where it stands. The swap
    /// already fading (if any) is dropped: the view the person sees is the one that goes on
    /// fading from where it is drawn.
    pub fn swap(&mut self, next: Element) {
        let now = frame_now();
        self.previous_opacity = self.fade.incoming(now);
        self.previous = Some(std::mem::replace(&mut self.current, next));
        self.pending = true;
        self.structure_dirty = true;
    }

    /// What the view shows now.
    #[must_use]
    pub fn current(&self) -> &dyn Widget {
        self.current.as_ref()
    }

    /// What the view shows now, to change or take.
    pub fn current_mut(&mut self) -> &mut Element {
        &mut self.current
    }

    /// The view that is fading out, while it still stands.
    #[must_use]
    pub fn previous(&self) -> Option<&dyn Widget> {
        self.previous.as_deref()
    }

    /// Whether a swap is playing or about to: the old view still stands.
    #[must_use]
    pub fn is_swapping(&self) -> bool {
        self.previous.is_some()
    }

    /// How opaque the view that is going out and the one coming in are drawn at `now`, while a
    /// swap plays: the old at its own opacity fading out over the first 60 %, the new fading in
    /// over the whole motion.
    #[must_use]
    pub fn opacities(&self, now: Instant) -> Option<(f32, f32)> {
        if !self.fade.is_moving(now) {
            return None;
        }
        let outgoing = self
            .fade
            .outgoing(now)
            .map_or(0.0, |(_, gliding)| self.previous_opacity * gliding);
        Some((outgoing, self.fade.incoming(now)))
    }

    /// The pace the swap plays at (`motion:` on the view).
    pub fn set_feel(&mut self, feel: Feel) {
        self.feel = feel;
    }

    /// The weight the view fills its room with.
    pub fn set_fill(&mut self, fill: f32) {
        self.fill = fill;
    }

    /// Address this widget by a stable id, for tests and inspection.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Start the swap the reconciler made, now that the theme is known, and let the old view go
    /// once it has faded (immediately, for one that lands).
    fn begin(&mut self, motion: EffectiveMotion, now: Instant) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        // A press or a gesture plays the swap; a key plays it at `fast` (the whole view is what
        // changed, which is what a reader watches), and a change no hand made lands.
        let source = recent_input(now).unwrap_or(MotionSource::Action);
        self.generation = self.generation.wrapping_add(1);
        self.fade
            .show(self.generation, self.feel.over(motion), source, now);
        self.settle(now);
    }

    /// Let the old view go once it has faded out. A swap still waiting to begin has not faded: the
    /// old view stays until it does, so a sync before the first layout never drops it.
    fn settle(&mut self, now: Instant) {
        if self.pending {
            return;
        }
        if self.previous.is_some() && !self.fade.is_moving(now) {
            self.previous = None;
            self.structure_dirty = true;
        }
    }

    fn starts(&mut self) {
        if self.pending
            && let Some(theme) = self.theme.clone()
        {
            self.begin(theme.motion(), frame_now());
        }
    }
}

/// The container's style: one cell both views fill, as tall as the taller of them where the
/// view's own height is open.
fn container(fill: f32) -> taffy::Style {
    let style = taffy::Style {
        display: taffy::Display::Grid,
        grid_template_columns: vec![taffy::style_helpers::minmax(
            taffy::style_helpers::length(0.0_f32),
            taffy::style_helpers::fr(1.0_f32),
        )],
        grid_template_rows: vec![taffy::style_helpers::minmax(
            taffy::style_helpers::length(0.0_f32),
            taffy::style_helpers::fr(1.0_f32),
        )],
        size: taffy::Size {
            width: taffy::style_helpers::percent(1.0_f32),
            height: taffy::Dimension::auto(),
        },
        min_size: taffy::Size {
            width: taffy::style_helpers::length(0.0_f32),
            height: taffy::Dimension::auto(),
        },
        ..taffy::Style::default()
    };
    tilo_ui_base::widget::layout::filling(style, fill)
}

/// Both views stand in the one cell, over each other, each taking the whole of it: the page that
/// came paints where the page that went stood.
fn cell() -> taffy::Style {
    let line = taffy::Line {
        start: taffy::style_helpers::line(1),
        end: taffy::GridPlacement::Auto,
    };
    taffy::Style {
        grid_column: line.clone(),
        grid_row: line,
        size: taffy::Size {
            width: taffy::style_helpers::percent(1.0_f32),
            height: taffy::style_helpers::percent(1.0_f32),
        },
        ..taffy::Style::default()
    }
}

impl Widget for Swapped {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.starts();
        self.settle(frame_now());
        self.current.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.starts();
        let now = frame_now();
        let was = self.previous.is_some();
        self.settle(now);
        self.current.update(state, path) | (was != self.previous.is_some())
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        self.begin(theme.motion(), frame_now());
        let current = self.current.build_layout(taffy, theme, node_map);
        let previous = self
            .previous
            .as_mut()
            .map(|previous| previous.build_layout(taffy, theme, node_map));
        let mut children = Vec::with_capacity(2);
        if let Some(previous) = previous {
            children.push(previous);
        }
        children.push(current);
        for child in &children {
            let _ = taffy.set_style(*child, cell());
        }
        taffy.widget_node(container(self.fill), &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        let origin = self.rect.origin();
        let mut index = 0;
        if let Some(previous) = self.previous.as_mut()
            && let Ok(child) = taffy.child_at_index(node, index)
        {
            previous.apply_layout(taffy, child, origin);
            index += 1;
        }
        if let Ok(child) = taffy.child_at_index(node, index) {
            self.current.apply_layout(taffy, child, origin);
        }
    }

    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let (scene, transform, text, theme, env) = ctx.parts();
        let now = frame_now();
        let opacities = self.opacities(now);
        if let (Some(previous), Some((outgoing, _))) = (self.previous.as_mut(), opacities)
            && outgoing > 0.0
        {
            let rect = self.rect;
            tilo_ui_base::widget::in_opacity_layer(scene, transform, rect, outgoing, |scene| {
                previous.draw_child(scene, transform, text, theme, &env);
            });
        }
        let incoming = opacities.map_or_else(|| self.fade.incoming(now), |(_, incoming)| incoming);
        if incoming <= 0.0 {
            return;
        }
        let rect = self.rect;
        let current = &mut self.current;
        tilo_ui_base::widget::in_opacity_layer(scene, transform, rect, incoming, |scene| {
            current.draw_child(scene, transform, text, theme, &env);
        });
    }

    /// The person acts on the view being shown: the one fading out is on its way out and takes
    /// nothing. It is laid out last, over the one it replaces.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let Some(current) = ctx
            .taffy
            .children(ctx.node_id)
            .ok()
            .and_then(|children| children.last().copied())
        else {
            return;
        };
        let path = ctx.path;
        self.current
            .handle_event(event, &mut ctx.reborrow(path, current));
    }

    fn wants_pointer_capture(&self) -> bool {
        self.current.wants_pointer_capture()
    }

    fn touch_claim(&self, point: Point) -> tilo_ui_base::interaction::TouchClaim {
        self.current.touch_claim(point)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.current.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.current.as_mut()]
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        if let Some(previous) = self.previous.as_deref() {
            visitor(previous);
        }
        visitor(self.current.as_ref());
        true
    }

    fn needs_animation_frame(&self) -> bool {
        // One frame after the old view's last lets it go: until it is dropped it is in the tree.
        self.pending
            || self.previous.is_some()
            || self.fade.is_moving(frame_now())
            || self.current.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.pending
            || self.fade.is_moving(frame_now())
            || self.current.needs_redraw()
            || self.needs_animation_frame()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.previous.is_some() || self.current.has_pending_visual_work()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        self.current.take_structure_dirty() | own
    }

    fn name(&self) -> &'static str {
        "Swapped"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
