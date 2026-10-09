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
use ui::vello::kurbo::{Affine, Point, Rect, RoundedRect};
use ui::vello::peniko::Fill;
use ui::widget::{AppModel, ModelPath, Widget};
use ui::{LocalState, PresenceCtx, TextRenderer, Theme};

use crate::model::Artwork;
use crate::render::{Options, Renderer};
use crate::resource::ArtworkResource;

/// The part of an artwork a picture shows, worked out from the artwork
/// as it is now: the artwork to draw (the same one, or a part cut from
/// it), the region of it, and whether it is drawn without its ground.
pub type Framing = Rc<dyn Fn(&Rc<Artwork>) -> Option<(Rc<Artwork>, Rect, bool)>>;

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
    /// A part of the artwork instead of all of it, as an export takes it.
    framing: Option<Framing>,
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
            framing: None,
        }
    }

    /// Show the part `framing` takes of the artwork, as it changes.
    #[must_use]
    pub fn framed(mut self, framing: Framing) -> Self {
        self.framing = Some(framing);
        self
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
        let style = Style {
            size: taffy::Size {
                width: taffy::Dimension::length(self.size.0),
                height: taffy::Dimension::length(self.size.1),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        let radius = f64::from(theme.tokens.radii.item);
        let frame = RoundedRect::from_rect(self.rect, radius);
        scene.push_layer(
            Fill::NonZero,
            ui::vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &frame,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &self.rect,
        );
        let (art, region, bare) = match &self.framing {
            Some(framing) => {
                framing(&self.art).unwrap_or_else(|| (Rc::clone(&self.art), Rect::ZERO, false))
            }
            None => (Rc::clone(&self.art), self.art.bounds(), false),
        };
        if region.width() > 0.0 && region.height() > 0.0 {
            let scale =
                (self.rect.width() / region.width()).min(self.rect.height() / region.height());
            let shown = Rect::from_center_size(
                self.rect.center(),
                (region.width() * scale, region.height() * scale),
            );
            if bare {
                checker(scene, transform, shown, theme);
            }
            // Only the part is shown, however the box's shape differs.
            scene.push_layer(
                Fill::NonZero,
                ui::vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &shown,
            );
            let to_thumb = transform
                * Affine::translate(shown.origin().to_vec2())
                * Affine::scale(scale)
                * Affine::translate(-region.origin().to_vec2());
            let options = Options {
                layers: self.layer.clone().map(|layer| vec![layer]),
                bare,
                live: true,
                ..Options::default()
            };
            self.renderer
                .borrow_mut()
                .draw(&art, scene, to_thumb, text_renderer, &options);
            scene.pop_layer();
        }
        scene.pop_layer();
        let ringed = self
            .target
            .as_ref()
            .is_some_and(|(chosen, id)| chosen.with(|chosen| chosen == id));
        ui::surface::ContentFrame::Thumbnail { selected: ringed }
            .draw(scene, transform, self.rect, theme);
    }

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {}

    fn name(&self) -> &'static str {
        "Thumb"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// The squares that say "transparent" under a picture without its ground.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a few squares of float geometry across a small box"
)]
fn checker(scene: &mut Scene, transform: Affine, area: Rect, theme: &Theme) {
    // The checker's square is a step of the theme's spacing.
    let square_side = f64::from(theme.tokens.spacing.s2);
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.content,
        None,
        &area,
    );
    let columns = (area.width() / square_side).ceil() as usize;
    let rows = (area.height() / square_side).ceil() as usize;
    for row in 0..rows {
        for column in (row % 2..columns).step_by(2) {
            let x = area.x0 + column as f64 * square_side;
            let y = area.y0 + row as f64 * square_side;
            let square = Rect::new(
                x,
                y,
                (x + square_side).min(area.x1),
                (y + square_side).min(area.y1),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &square,
            );
        }
    }
}

impl ResourceSurface<ArtworkResource> for Thumb {
    fn present(&mut self, state: &Rc<Artwork>) {
        self.art = Rc::clone(state);
    }
}
