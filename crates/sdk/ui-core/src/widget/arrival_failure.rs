//! The one face for something that did not arrive.
//!
//! A piece of data that failed to load ([`ArrivingView`](super::ArrivingView))
//! and a window that did not open show the same thing: an error banner that
//! says what failed, then why, with the way to try again as its action
//! ([`arrival_failure`]). A window shows it as a [`WindowFailure`]: the same
//! banner, centred in the window's area at the readable width.

use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{Banner, EventCtx, TaffyTreeExt, Widget};

/// The failure face: `title` says what failed (never an app's name, which
/// its window already shows), `reason` says why. The caller adds the way to
/// try again with [`Banner::with_action`].
#[must_use]
pub fn arrival_failure(title: impl Into<String>, reason: impl Into<String>) -> Banner {
    Banner::error(title).with_message(reason)
}

/// The failure face filling a window: the banner centred in the area at the
/// readable width, standing at rest from its first frame (the window's
/// arrival brings it in).
pub struct WindowFailure {
    face: Box<dyn Widget>,
    rect: Rect,
}

impl WindowFailure {
    #[must_use]
    pub fn new(face: Banner) -> Self {
        Self {
            face: Box::new(face.arrives_in_place()),
            rect: Rect::ZERO,
        }
    }
}

impl Widget for WindowFailure {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let face = self.face.build_layout(taffy, theme, node_map);
        if let Ok(style) = taffy.style(face) {
            let mut style = style.clone();
            style.max_size.width = length(theme.tokens.metrics.readable_width);
            let _ = taffy.set_style(face, style);
        }
        let gutter = theme
            .tokens
            .spacing
            .rhythm
            .get(crate::tokens::Rhythm::Group);
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            justify_content: Some(taffy::JustifyContent::Center),
            align_items: Some(taffy::AlignItems::Center),
            padding: super::layout::insets_all(gutter),
            size: taffy::Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            ..Default::default()
        };
        let node = taffy.parent_node(style, &[face]);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Some(&face) = taffy.children(node).unwrap_or_default().first() {
            self.face.apply_layout(taffy, face, self.rect.origin());
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
        crate::widget::draw_child(
            self.face.as_mut(),
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let Some(&face) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first() {
            self.face
                .handle_event(event, &mut ctx.reborrow(ctx.path, face));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.face.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.face.as_mut()]
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.face.collect_damage(regions);
    }

    fn name(&self) -> &'static str {
        "WindowFailure"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
#[path = "arrival_failure_tests.rs"]
mod tests;
