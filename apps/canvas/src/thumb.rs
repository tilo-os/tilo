//! A small picture of the artwork, or of one layer of it, as a layers
//! panel shows it. It draws through the renderer the canvas draws with, so
//! a painted layer's simulation is reused, never run again for its
//! thumbnail.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::framework::ResourceSurface;
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use ui::vello::peniko::Fill;
use ui::widget::{AppModel, ModelPath, Widget};
use ui::{LocalState, PresenceCtx, TextRenderer, Theme};

use crate::model::Artwork;
use crate::render::{Options, Renderer};
use crate::resource::ArtworkResource;

/// The artwork, or one of its layers, fitted into a fixed box.
pub struct Thumb {
    rect: Rect,
    size: (f32, f32),
    art: Rc<Artwork>,
    layer: Option<String>,
    renderer: Rc<RefCell<Renderer>>,
    /// The chosen layer, and this picture's: ringed while they agree, so a
    /// layer with a mask shows which of the two is being painted.
    target: Option<(LocalState<String>, String)>,
}

impl Thumb {
    /// A `width` × `height` picture of `layer` (the whole artwork when
    /// `None`).
    pub fn new(
        renderer: Rc<RefCell<Renderer>>,
        layer: Option<String>,
        width: f32,
        height: f32,
    ) -> Self {
        Self {
            rect: Rect::ZERO,
            size: (width, height),
            art: Rc::new(Artwork::default()),
            layer,
            renderer,
            target: None,
        }
    }

    /// Ring the picture while `chosen` holds `id`.
    #[must_use]
    pub fn ringed_while(mut self, chosen: &LocalState<String>, id: &str) -> Self {
        self.target = Some((chosen.clone(), id.to_owned()));
        self
    }
}

impl Widget for Thumb {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt as _;
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: taffy::Dimension::length(self.size.0),
                height: taffy::Dimension::length(self.size.1),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "kurbo's Affine composition is float geometry"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() < 1.0 || self.art.width <= 0.0 || self.art.height <= 0.0 {
            return;
        }
        let radius = f64::from(theme.tokens.radii.sm);
        let frame = RoundedRect::from_rect(self.rect, radius);
        scene.push_layer(
            Fill::NonZero,
            ui::vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &frame,
        );
        scene.fill(Fill::NonZero, transform, theme.bg_app, None, &self.rect);
        let scale = (self.rect.width() / self.art.width).min(self.rect.height() / self.art.height);
        let offset = Point::new(
            self.rect.center().x - self.art.width * scale / 2.0,
            self.rect.center().y - self.art.height * scale / 2.0,
        );
        let to_thumb = transform * Affine::translate(offset.to_vec2()) * Affine::scale(scale);
        let options = Options {
            layers: self.layer.clone().map(|layer| vec![layer]),
            ..Options::default()
        };
        self.renderer
            .borrow_mut()
            .draw(&self.art, scene, to_thumb, text_renderer, &options);
        scene.pop_layer();
        let ringed = self
            .target
            .as_ref()
            .is_some_and(|(chosen, id)| chosen.with(|chosen| chosen == id));
        if ringed {
            // Outside the picture, so none of it is hidden by the ring.
            // Two of the chrome's rules: heavy enough to say "this one".
            let ring = 2.0 * f64::from(theme.tokens.metrics.chrome_rule_px);
            scene.stroke(
                &Stroke::new(ring),
                transform,
                theme.accent_primary,
                None,
                &frame
                    .rect()
                    .inflate(ring / 2.0, ring / 2.0)
                    .to_rounded_rect(radius + ring / 2.0),
            );
        } else {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &frame,
            );
        }
    }

    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {}

    fn name(&self) -> &'static str {
        "Thumb"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ResourceSurface<ArtworkResource> for Thumb {
    fn present(&mut self, state: &Rc<Artwork>) {
        self.art = Rc::clone(state);
    }
}
