//! Catalogue artwork leaf with a useful monogram fallback.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::style_helpers::length;
use taffy::{NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, Role};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Fill, ImageData};

pub struct Artwork {
    store: Option<ui::ImageStore>,
    url: String,
    image: Option<ImageData>,
    label: String,
    monogram: String,
    width: f32,
    height: f32,
    radius: f64,
    rect: Rect,
    dirty: bool,
}

impl Artwork {
    pub fn new(
        store: Option<ui::ImageStore>,
        url: Option<&str>,
        label: &str,
        width: f32,
        height: f32,
    ) -> Self {
        let monogram = if width >= 160.0 {
            "Preview unavailable".into()
        } else {
            label
                .chars()
                .find(|c| c.is_alphanumeric())
                .map_or_else(|| "•".into(), |c| c.to_uppercase().collect())
        };
        Self {
            store,
            url: url.unwrap_or_default().to_string(),
            image: None,
            label: label.to_string(),
            monogram,
            width,
            height,
            radius: if width <= 56.0 { 12.0 } else { 16.0 },
            rect: Rect::ZERO,
            dirty: true,
        }
    }
}

impl Widget for Artwork {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        if self.image.is_none()
            && !self.url.is_empty()
            && let Some(store) = &self.store
            && let Some(image) = store.get(&self.url)
        {
            self.image = Some(image);
            self.dirty = true;
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &ui::Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: length(self.width),
                height: length(self.height),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Ok(layout) = taffy.layout(node) {
            self.rect = Rect::new(
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
                origin.x + f64::from(layout.location.x + layout.size.width),
                origin.y + f64::from(layout.location.y + layout.size.height),
            );
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &ui::Theme,
        _presence: &PresenceCtx,
    ) {
        if let Some(image) = &self.image {
            ui::image_store::paint_image_cover(scene, transform, self.rect, image, self.radius);
            return;
        }
        scene.fill(
            Fill::NonZero,
            transform,
            theme.bg_hover,
            None,
            &RoundedRect::from_rect(self.rect, self.radius),
        );
        let size = if self.width < 160.0 { 20.0 } else { 14.0 };
        let (tw, th) = text.measure(&self.monogram, size, None);
        text.draw_text(
            scene,
            &self.monogram,
            size,
            theme.text_primary,
            transform.pre_translate(
                (
                    self.rect.center().x - f64::from(tw) / 2.0,
                    self.rect.center().y - f64::from(th) / 2.0,
                )
                    .into(),
            ),
            None,
        );
    }
    fn handle_event(
        &mut self,
        _: &winit::event::WindowEvent,
        _: f64,
        _: &winit::event::Modifiers,
        _: &mut AutoCommit,
        _: &ObjId,
        _: &mut bool,
        _: &mut Option<NodeId>,
        _: NodeId,
        _: &TaffyTree<()>,
    ) {
    }
    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            AccessNode::new(Role::Image)
                .with_name(format!("{} artwork", self.label))
                .with_bounds(self.rect),
        )
    }
    fn name(&self) -> &'static str {
        "StoreArtwork"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}
