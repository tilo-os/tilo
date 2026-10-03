//! A plain column of retained widgets the app's views are built from.
//!
//! The workspace, the navigator and the query pane each own typed widgets (an
//! editor they bind to the document, a table they fill, a tab strip they
//! read), so none of them can hand its children to a generic `Column`. A
//! [`Frame`] holds what they have in common: the layout nodes of the slots, the
//! rect, and the routing of events and painting across them.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Position, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{EventCtx, TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

/// How a slot takes space in the frame's column.
#[derive(Clone, Copy, Debug)]
pub enum Place {
    /// Its natural height.
    Natural,
    /// Its natural height, at the start of the cross axis instead of
    /// stretched across it.
    Start,
    /// Whatever is left.
    Grow,
    /// A height the frame's owner computed.
    Fixed(f32),
    /// Whatever is left, up to this much.
    Capped(f32),
    /// Out of the flow: a dialog, which paints in the window's top layer.
    Overlay,
}

/// One widget and where it goes.
pub struct Slot<'a> {
    pub widget: &'a mut dyn Widget,
    pub place: Place,
}

impl<'a> Slot<'a> {
    pub fn new(widget: &'a mut dyn Widget, place: Place) -> Self {
        Self { widget, place }
    }
}

/// The column's own style.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStyle {
    /// Space between slots.
    pub gap: f32,
    /// Space around all of them.
    pub padding: f32,
    /// Lay the slots out in a row instead of a column.
    pub row: bool,
    /// Centre the slots across the frame instead of stretching them.
    pub centred: bool,
}

/// Layout state shared by every view built from slots.
#[derive(Default)]
pub struct Frame {
    nodes: Vec<NodeId>,
    rect: Rect,
}

impl Frame {
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Build the column. `owner` is the widget the frame belongs to, which the
    /// host maps the column's node to.
    pub fn build(
        &mut self,
        owner: *mut dyn Widget,
        slots: &mut [Slot<'_>],
        style: FrameStyle,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.nodes.clear();
        let row = style.row;
        for slot in slots.iter_mut() {
            let node = slot.widget.build_layout(taffy, theme, node_map);
            if let Ok(mut node_style) = taffy.style(node).cloned() {
                match slot.place {
                    Place::Natural => node_style.flex_shrink = 0.0,
                    Place::Start => {
                        node_style.flex_shrink = 0.0;
                        node_style.align_self = Some(taffy::AlignSelf::FlexStart);
                    }
                    Place::Grow => {
                        if row {
                            node_style.size.width = Dimension::auto();
                            node_style.min_size.width = Dimension::length(0.0);
                        } else {
                            node_style.size.height = Dimension::auto();
                            node_style.min_size.height = Dimension::length(0.0);
                        }
                        node_style.flex_grow = 1.0;
                        node_style.flex_shrink = 1.0;
                        node_style.flex_basis = Dimension::length(0.0);
                    }
                    Place::Capped(extent) => {
                        if row {
                            node_style.size.width = Dimension::auto();
                            node_style.max_size.width = Dimension::length(extent);
                            node_style.min_size.width = Dimension::length(0.0);
                        } else {
                            node_style.size.height = Dimension::auto();
                            node_style.max_size.height = Dimension::length(extent);
                            node_style.min_size.height = Dimension::length(0.0);
                        }
                        node_style.flex_grow = 1.0;
                        node_style.flex_shrink = 1.0;
                        node_style.flex_basis = Dimension::length(0.0);
                    }
                    Place::Fixed(extent) => {
                        if row {
                            node_style.size.width = Dimension::length(extent);
                        } else {
                            node_style.size.height = Dimension::length(extent);
                        }
                        node_style.flex_grow = 0.0;
                        node_style.flex_shrink = 0.0;
                        node_style.flex_basis = Dimension::auto();
                    }
                    Place::Overlay => {
                        node_style.position = Position::Absolute;
                        node_style.size = taffy::Size {
                            width: Dimension::length(0.0),
                            height: Dimension::length(0.0),
                        };
                    }
                }
                let _ = taffy.set_style(node, node_style);
            }
            self.nodes.push(node);
        }
        let gap = style.gap;
        let pad = style.padding;
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: if row {
                    taffy::FlexDirection::Row
                } else {
                    taffy::FlexDirection::Column
                },
                align_items: style.centred.then_some(taffy::AlignItems::Center),
                size: taffy::Size {
                    width: Dimension::percent(1.0),
                    height: if row {
                        Dimension::auto()
                    } else {
                        Dimension::percent(1.0)
                    },
                },
                min_size: taffy::Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                gap: taffy::Size {
                    width: taffy::style_helpers::length(if row { gap } else { 0.0 }),
                    height: taffy::style_helpers::length(if row { 0.0 } else { gap }),
                },
                padding: taffy::Rect {
                    left: taffy::style_helpers::length(pad),
                    right: taffy::style_helpers::length(pad),
                    top: taffy::style_helpers::length(pad),
                    bottom: taffy::style_helpers::length(pad),
                },
                ..Default::default()
            },
            &self.nodes,
        );
        node_map.insert(node, owner);
        node
    }

    /// Place the slots from the solved layout.
    pub fn apply(
        &mut self,
        slots: &mut [Slot<'_>],
        taffy: &TaffyTree<()>,
        node: NodeId,
        origin: Point,
    ) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let inner = Point::new(self.rect.x0, self.rect.y0);
        for (slot, node) in slots.iter_mut().zip(&self.nodes) {
            slot.widget.apply_layout(taffy, *node, inner);
        }
    }

    pub fn draw(
        slots: &mut [Slot<'_>],
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for slot in slots.iter_mut() {
            slot.widget.draw(scene, transform, text, theme, presence);
        }
    }

    /// Offer the event to the slots: one holding a pointer drag first, then
    /// overlays, then the rest in order, until one handles it.
    pub fn handle_event(
        &self,
        slots: &mut [Slot<'_>],
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) {
        if let Some(index) = slots
            .iter()
            .position(|slot| slot.widget.wants_pointer_capture())
            && let Some(node) = self.nodes.get(index).copied()
            && let Some(slot) = slots.get_mut(index)
        {
            slot.widget
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            return;
        }
        let overlays = slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| matches!(slot.place, Place::Overlay))
            .map(|(index, _)| index);
        let flow = slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| !matches!(slot.place, Place::Overlay))
            .map(|(index, _)| index);
        let order: Vec<usize> = overlays.chain(flow).collect();
        for index in order {
            let Some(node) = self.nodes.get(index).copied() else {
                continue;
            };
            let Some(slot) = slots.get_mut(index) else {
                continue;
            };
            slot.widget
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
    }
}

/// The `Widget` methods every frame-based view shares. The view provides
/// `frame`, `slots`, `children_ref` and `frame_style`; a call goes inside its
/// `impl Widget`.
macro_rules! frame_methods {
    () => {
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            theme: &ui::theme::Theme,
            node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
        ) -> taffy::NodeId {
            let owner: *mut dyn ui::widget::Widget = self;
            let style = self.frame_style(theme);
            let mut frame = std::mem::take(&mut self.frame);
            let node = frame.build(owner, &mut self.slots(), style, taffy, theme, node_map);
            self.frame = frame;
            node
        }

        fn apply_layout(
            &mut self,
            taffy: &taffy::TaffyTree<()>,
            node: taffy::NodeId,
            origin: vello::kurbo::Point,
        ) {
            let mut frame = std::mem::take(&mut self.frame);
            frame.apply(&mut self.slots(), taffy, node, origin);
            self.frame = frame;
        }

        fn draw(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text: &mut ui::text_render::TextRenderer,
            theme: &ui::theme::Theme,
            presence: &ui::presence::PresenceCtx,
        ) {
            $crate::frame::Frame::draw(&mut self.slots(), scene, transform, text, theme, presence);
        }

        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            ctx: &mut ui::widget::EventCtx<'_>,
        ) {
            let frame = std::mem::take(&mut self.frame);
            frame.handle_event(&mut self.slots(), event, ctx);
            self.frame = frame;
        }

        fn update(
            &mut self,
            state: &mut ui::widget::AppModel,
            path: &ui::widget::ModelPath,
        ) -> bool {
            let mut changed = false;
            for slot in self.slots() {
                changed |= slot.widget.update(state, path);
            }
            changed
        }

        fn children(&self) -> Vec<&dyn ui::widget::Widget> {
            self.children_ref()
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn ui::widget::Widget + '_)> {
            self.slots().into_iter().map(|slot| slot.widget).collect()
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            self.frame.rect()
        }

        fn needs_redraw(&self) -> bool {
            true
        }

        fn is_layout_dirty(&self) -> bool {
            true
        }

        fn take_structure_dirty(&mut self) -> bool {
            let mut dirty = std::mem::take(&mut self.structure_dirty);
            for slot in self.slots() {
                dirty |= slot.widget.take_structure_dirty();
            }
            dirty
        }

        fn wants_pointer_capture(&self) -> bool {
            self.children_ref()
                .iter()
                .any(|child| child.wants_pointer_capture())
        }

        fn needs_animation_frame(&self) -> bool {
            self.children_ref()
                .iter()
                .any(|child| child.needs_animation_frame())
        }
    };
}
pub(crate) use frame_methods;
