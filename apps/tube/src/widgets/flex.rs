//! `Flex` — the workhorse layout + surface + click primitive for Tube.
//!
//! The toolkit ships `Row`/`Column`/`Container`, but each bakes in a fixed
//! padding/gap/background and none of them are clickable. Tube's grid,
//! cards, nav rail, panels and chips all want fine control over sizing,
//! gaps, fills, rounding and "this whole box is a button". Rather than
//! fight three widgets we compose one configurable box:
//!
//! * direction (row / column), gap, padding, wrap
//! * sizing per-axis (auto / fill / fixed px / percent) + flex grow
//! * an optional rounded fill + border read from the [`Theme`]
//! * an optional click [`Action`] pushed onto the shared [`ActionBus`],
//!   with a hover/press surface treatment
//! * optional clip (for scroll regions / rounded thumbnails)
//!
//! All colours come in as [`Color`] arguments resolved by the caller from
//! the live `Theme`, so theming stays OS-controlled.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{fr, length, minmax, percent, repeat};
use taffy::{Dimension, NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::action::{self, Action, ActionBus};

/// Anything that can be turned into a boxed widget for `Flex::push`.
/// Lets call sites pass a bare widget, a `Flex` chain, or an already-boxed
/// `Box<dyn Widget>` (from the `text`/`icon`/`pill` helpers) interchangeably.
pub trait IntoElement {
    fn into_element(self) -> Box<dyn Widget>;
}

impl<W: Widget + 'static> IntoElement for W {
    fn into_element(self) -> Box<dyn Widget> {
        Box::new(self)
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Axis {
    Row,
    Col,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Len {
    Auto,
    Fill,
    Px(f32),
    Pct(f32),
}

impl Len {
    fn dim(self) -> Dimension {
        match self {
            Len::Auto => Dimension::auto(),
            Len::Fill => percent(1.0_f32),
            Len::Px(p) => length(p),
            Len::Pct(p) => percent(p),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Justify {
    Start,
    Center,
    End,
    Between,
}

/// Background fill + border treatment for a box.
#[derive(Clone, Copy)]
pub struct Surface {
    pub fill: Option<Color>,
    pub radius: f64,
    pub border: Option<(f64, Color)>,
}

pub struct Flex {
    children: Vec<Box<dyn Widget>>,
    axis: Axis,
    gap: f32,
    pad: [f32; 4], // l, t, r, b
    width: Len,
    height: Len,
    min_w: Option<f32>,
    min_h: Option<f32>,
    max_w: Option<f32>,
    align: Align,
    justify: Justify,
    wrap: bool,
    /// When set, lay children out as a responsive CSS grid: as many equal
    /// columns as fit at ≥ this many logical px, each column flexing to fill the
    /// row (`minmax(min, 1fr)` under `auto-fill`). Cards scale with the width
    /// and only wrap to a new column when they'd exceed the natural max — so a
    /// row never leaves a gutter on the right. Takes precedence over `wrap`.
    grid_min_col: Option<f32>,
    grow: f32,
    shrink: f32,
    block: bool,
    clip: bool,
    /// When set, this box is absolutely positioned filling its containing
    /// block (inset 0 on all sides) so it stacks *over* its siblings — used for
    /// the command-palette overlay on top of the shell.
    overlay: bool,
    /// Like `overlay` for positioning (absolute, fills the containing block and
    /// stacks over siblings) but WITHOUT the modal pointer capture: events that
    /// miss this box's interactive children fall through to the content beneath.
    /// Used for a small on-video affordance (the `SponsorBlock` "Skip" button).
    float: bool,

    surface: Option<Surface>,
    hover_surface: Option<Surface>,

    // Click support.
    bus: Option<ActionBus>,
    action: Option<Action>,

    // Runtime state.
    rect: Rect,
    hovered: bool,
    pressed: bool,
    dirty: bool,
    test_id: Option<String>,
}

impl Flex {
    pub fn new(axis: Axis) -> Self {
        Self {
            children: Vec::new(),
            axis,
            gap: 0.0,
            pad: [0.0; 4],
            width: Len::Auto,
            height: Len::Auto,
            min_w: None,
            min_h: None,
            max_w: None,
            align: if axis == Axis::Row {
                Align::Center
            } else {
                Align::Start
            },
            justify: Justify::Start,
            wrap: false,
            grid_min_col: None,
            grow: 0.0,
            shrink: 0.0,
            block: false,
            clip: false,
            overlay: false,
            float: false,
            surface: None,
            hover_surface: None,
            bus: None,
            action: None,
            rect: Rect::ZERO,
            hovered: false,
            pressed: false,
            dirty: true,
            test_id: None,
        }
    }

    pub fn row() -> Self {
        Self::new(Axis::Row)
    }
    pub fn col() -> Self {
        Self::new(Axis::Col)
    }

    pub fn push(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_element());
        self
    }

    pub fn push_if(self, cond: bool, child: impl IntoElement) -> Self {
        if cond { self.push(child) } else { self }
    }

    pub fn gap(mut self, g: f32) -> Self {
        self.gap = g;
        self
    }

    pub fn pad(mut self, p: f32) -> Self {
        self.pad = [p; 4];
        self
    }

    pub fn pad_xy(mut self, x: f32, y: f32) -> Self {
        self.pad = [x, y, x, y];
        self
    }

    pub fn pad_each(mut self, l: f32, t: f32, r: f32, b: f32) -> Self {
        self.pad = [l, t, r, b];
        self
    }

    pub fn w(mut self, w: Len) -> Self {
        self.width = w;
        self
    }
    pub fn h(mut self, h: Len) -> Self {
        self.height = h;
        self
    }
    pub fn fill_w(self) -> Self {
        self.w(Len::Fill)
    }
    pub fn fill_h(self) -> Self {
        self.h(Len::Fill)
    }
    pub fn fixed(self, w: f32, h: f32) -> Self {
        self.w(Len::Px(w)).h(Len::Px(h))
    }
    pub fn min_w(mut self, w: f32) -> Self {
        self.min_w = Some(w);
        self
    }
    pub fn min_h(mut self, h: f32) -> Self {
        self.min_h = Some(h);
        self
    }
    pub fn max_w(mut self, w: f32) -> Self {
        self.max_w = Some(w);
        self
    }

    pub fn align(mut self, a: Align) -> Self {
        self.align = a;
        self
    }
    pub fn justify(mut self, j: Justify) -> Self {
        self.justify = j;
        self
    }
    pub fn wrap(mut self) -> Self {
        self.wrap = true;
        self
    }

    /// Lay children out as a responsive grid whose columns are each at least
    /// `min_col_px` wide and flex to fill the row. As the box widens the columns
    /// grow (up to ~the point another `min_col_px` column fits, then a column is
    /// added and they snap back) — so cards scale with the window and the row is
    /// always exactly filled, never leaving blank space on the right. The grid's
    /// `gap` is the existing [`Flex::gap`].
    pub fn grid(mut self, min_col_px: f32) -> Self {
        self.grid_min_col = Some(min_col_px.max(1.0));
        self
    }
    pub fn grow(mut self, g: f32) -> Self {
        self.grow = g;
        self
    }
    pub fn shrink(mut self, s: f32) -> Self {
        self.shrink = s;
        self
    }
    /// Lay children out as a block stack (needed when this box is the
    /// direct child of a `Scrollable` so its height sums instead of
    /// truncating). See `ui::widget::Column::with_intrinsic_height`.
    pub fn block(mut self) -> Self {
        self.block = true;
        self
    }
    pub fn clip(mut self) -> Self {
        self.clip = true;
        self
    }

    /// Make this box an absolutely-positioned overlay filling its containing
    /// block (inset 0), so it stacks on top of its siblings. The containing
    /// block is the nearest positioned ancestor — the shell root sets itself
    /// `position: relative` implicitly by being the flex parent.
    pub fn overlay(mut self) -> Self {
        self.overlay = true;
        self
    }

    /// Float this box above its in-flow siblings (absolute, fills the containing
    /// block) with correct z-order + hit priority but no modal capture — clicks
    /// that miss its children pass through to the content beneath. For a small
    /// on-video affordance like the "Skip" button.
    pub fn float(mut self) -> Self {
        self.float = true;
        self
    }

    pub fn surface(mut self, s: Surface) -> Self {
        self.surface = Some(s);
        self
    }
    pub fn hover_surface(mut self, s: Surface) -> Self {
        self.hover_surface = Some(s);
        self
    }

    /// Make the whole box a button that pushes `action` on click.
    pub fn on_click(mut self, bus: &ActionBus, action: Action) -> Self {
        self.bus = Some(bus.clone());
        self.action = Some(action);
        self
    }

    pub fn id(mut self, id: &str) -> Self {
        self.test_id = Some(id.to_string());
        self
    }

    fn shape(&self, radius: f64) -> RoundedRect {
        RoundedRect::from_rect(self.rect, radius)
    }

    fn paint_surface(&self, scene: &mut Scene, transform: Affine, s: &Surface) {
        if let Some(fill) = s.fill {
            scene.fill(Fill::NonZero, transform, fill, None, &self.shape(s.radius));
        }
        if let Some((w, c)) = s.border {
            scene.stroke(&Stroke::new(w), transform, c, None, &self.shape(s.radius));
        }
    }
}

impl Widget for Flex {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        for c in &mut self.children {
            c.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child_nodes: Vec<NodeId> = self
            .children
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();

        let align_items = Some(match self.align {
            Align::Start => taffy::AlignItems::FlexStart,
            Align::Center => taffy::AlignItems::Center,
            Align::End => taffy::AlignItems::FlexEnd,
            Align::Stretch => taffy::AlignItems::Stretch,
        });
        let justify_content = Some(match self.justify {
            Justify::Start => taffy::JustifyContent::FlexStart,
            Justify::Center => taffy::JustifyContent::Center,
            Justify::End => taffy::JustifyContent::FlexEnd,
            Justify::Between => taffy::JustifyContent::SpaceBetween,
        });

        let style = Style {
            display: if self.grid_min_col.is_some() {
                taffy::Display::Grid
            } else if self.block {
                taffy::Display::Block
            } else {
                taffy::Display::Flex
            },
            position: if self.overlay || self.float {
                taffy::Position::Absolute
            } else {
                taffy::Position::Relative
            },
            // Absolute + inset 0 => fill the containing block, stacking over
            // in-flow siblings.
            inset: if self.overlay || self.float {
                taffy::Rect {
                    left: length(0.0_f32),
                    right: length(0.0_f32),
                    top: length(0.0_f32),
                    bottom: length(0.0_f32),
                }
            } else {
                taffy::Rect::auto()
            },
            flex_direction: match self.axis {
                Axis::Row => taffy::FlexDirection::Row,
                Axis::Col => taffy::FlexDirection::Column,
            },
            flex_wrap: if self.wrap {
                taffy::FlexWrap::Wrap
            } else {
                taffy::FlexWrap::NoWrap
            },
            align_items,
            justify_content,
            // Responsive grid track template: `repeat(auto-fill, minmax(min,
            // 1fr))` — as many ≥min-wide columns as fit, each flexing (`1fr`) to
            // fill the row. Grid items default to `justify_self: stretch`, so a
            // card fills its column exactly — the property that removes the
            // right-edge gutter. Empty (no-op) in flex/block mode.
            grid_template_columns: self
                .grid_min_col
                .map(|min| vec![repeat("auto-fill", vec![minmax(length(min), fr(1.0_f32))])])
                .unwrap_or_default(),
            gap: taffy::Size {
                width: length(self.gap),
                height: length(self.gap),
            },
            padding: taffy::Rect {
                left: length(self.pad[0]),
                top: length(self.pad[1]),
                right: length(self.pad[2]),
                bottom: length(self.pad[3]),
            },
            size: taffy::Size {
                width: self.width.dim(),
                height: self.height.dim(),
            },
            min_size: taffy::Size {
                width: self.min_w.map_or(taffy::Dimension::auto(), length),
                height: self.min_h.map_or(taffy::Dimension::auto(), length),
            },
            max_size: taffy::Size {
                width: self.max_w.map_or(taffy::Dimension::auto(), length),
                height: taffy::Dimension::auto(),
            },
            flex_grow: self.grow,
            flex_shrink: self.shrink,
            flex_basis: if self.grow > 0.0 {
                length(0.0_f32)
            } else {
                taffy::Dimension::auto()
            },
            overflow: if self.clip {
                taffy::Point {
                    x: taffy::Overflow::Hidden,
                    y: taffy::Overflow::Hidden,
                }
            } else {
                taffy::Point {
                    x: taffy::Overflow::Visible,
                    y: taffy::Overflow::Visible,
                }
            },
            ..Default::default()
        };
        let _ = theme;

        let node = if child_nodes.is_empty() {
            super::leaf_node(taffy, style)
        } else {
            super::parent_node(taffy, style, &child_nodes)
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = super::node_layout(taffy, node);
        let w = f64::from(layout.size.width.max(layout.content_size.width));
        let h = f64::from(layout.size.height.max(layout.content_size.height));
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + w,
            origin.y + f64::from(layout.location.y) + h,
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Ok(children) = taffy.children(node) {
            for (cw, cn) in self.children.iter_mut().zip(children.iter()) {
                cw.apply_layout(taffy, *cn, new_origin);
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Background / hover surface.
        let surf = if self.hovered {
            self.hover_surface.as_ref().or(self.surface.as_ref())
        } else {
            self.surface.as_ref()
        };
        if let Some(s) = surf {
            self.paint_surface(scene, transform, s);
        }

        if self.clip {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &self.rect,
            );
        }
        // In-flow children first, then floating children on top (z-order).
        for c in self.children.iter_mut().filter(|c| !c.is_floating()) {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
        for c in self.children.iter_mut().filter(|c| c.is_floating()) {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
        if self.clip {
            scene.pop_layer();
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }
        let surf = if self.hovered {
            self.hover_surface.as_ref().or(self.surface.as_ref())
        } else {
            self.surface.as_ref()
        };
        if let Some(s) = surf {
            self.paint_surface(scene, transform, s);
        }
        if self.clip {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &self.rect,
            );
        }
        for c in self.children.iter_mut().filter(|c| !c.is_floating()) {
            c.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
            );
        }
        for c in self.children.iter_mut().filter(|c| c.is_floating()) {
            c.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
            );
        }
        if self.clip {
            scene.pop_layer();
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Children first so inner interactive widgets win.
        if let Ok(child_nodes) = taffy.children(node_id) {
            if let Some(idx) = self.children.iter().position(|c| c.wants_pointer_capture()) {
                let cn = child_nodes[idx];
                self.children[idx].handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    cn,
                    taffy,
                );
                return;
            }
            // Floating children (e.g. an on-video "Skip" button) get first dibs
            // so a click on them isn't stolen by the surface beneath; a miss
            // falls through to the in-flow children below.
            for (cw, &cn) in self.children.iter_mut().zip(child_nodes.iter()) {
                if !cw.is_floating() {
                    continue;
                }
                cw.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    cn,
                    taffy,
                );
                if *handled {
                    break;
                }
            }
            if !*handled {
                for (cw, &cn) in self.children.iter_mut().zip(child_nodes.iter()) {
                    if cw.is_floating() {
                        continue;
                    }
                    cw.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        cn,
                        taffy,
                    );
                    if *handled {
                        break;
                    }
                }
            }
        }

        if self.action.is_none() {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hov = self.rect.contains(p);
                if hov != self.hovered {
                    self.hovered = hov;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.hovered && !*handled {
                    self.pressed = true;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.pressed && self.hovered && !*handled {
                    if let (Some(bus), Some(act)) = (&self.bus, &self.action) {
                        action::push(bus, act.clone());
                    }
                    *handled = true;
                }
                self.pressed = false;
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.children
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.children
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn wants_pointer_capture(&self) -> bool {
        // An overlay captures all pointer events so it behaves modally — the
        // parent routes exclusively to it, keeping clicks off the shell beneath
        // (e.g. the top-bar menu button under the command-palette scrim). A
        // `float` deliberately does NOT capture (see `is_floating`).
        self.overlay || self.children.iter().any(|c| c.wants_pointer_capture())
    }

    fn is_floating(&self) -> bool {
        self.float
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.children.iter().any(|c| c.needs_redraw())
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        for c in &mut self.children {
            c.mark_drawn();
        }
    }
    fn is_layout_dirty(&self) -> bool {
        self.children.iter().any(|c| c.is_layout_dirty())
    }
    fn take_structure_dirty(&mut self) -> bool {
        // Not `.any()`: every child's dirty flag must be taken, not just the
        // first that's set, so this must not short-circuit.
        let mut dirty = false;
        for c in &mut self.children {
            dirty |= c.take_structure_dirty();
        }
        dirty
    }

    fn name(&self) -> &'static str {
        "Flex"
    }
    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}
