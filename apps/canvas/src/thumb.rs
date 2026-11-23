//! A small picture of the artwork, or of one layer of it, as a layers
//! panel shows it. It draws through the renderer the canvas draws with, so
//! a painted layer's simulation is reused, never run again for its
//! thumbnail. It follows the document the view reads, as the canvas does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::framework::Live;
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Rect, RoundedRect};
use ui::vello::peniko::Fill;
use ui::widget::{AppModel, ModelPath, Widget};
use ui::{LocalState, PresenceCtx, TextRenderer, Theme};

use crate::model::Artwork;
use crate::render::{Options, Renderer};

/// The part of an artwork a picture shows, worked out from the artwork
/// as it is now: the artwork to draw (the same one, or a part cut from
/// it), the region of it, and whether it is drawn without its ground.
pub type Framing = Rc<dyn Fn(&Rc<Artwork>) -> Option<(Rc<Artwork>, Rect, bool)>>;

pub struct Thumb {
    rect: Rect,
    size: (f32, f32),
    live: Live<Artwork>,
    /// The artwork as this picture last showed it, and what of the document
    /// it was (see [`Live::changed_since`]).
    art: Rc<Artwork>,
    presented: Option<(u64, u64)>,
    layer: Option<String>,
    renderer: Rc<RefCell<Renderer>>,
    /// The chosen layer, and this picture's: ringed while they agree, so a
    /// layer with a mask shows which of the two is being painted.
    target: Option<(LocalState<String>, String)>,
    framing: Option<Framing>,
    /// The ring it last painted, so a change of selection repaints it.
    ringed: bool,
    /// What this picture paints changed: it repaints once, and rests after
    /// the frame draws it.
    dirty: bool,
}

impl Thumb {
    /// A `width` × `height` picture of `layer` (the whole artwork when
    /// `None`) of the artwork `live` holds.
    pub fn new(
        live: Live<Artwork>,
        renderer: Rc<RefCell<Renderer>>,
        layer: Option<String>,
        width: f32,
        height: f32,
    ) -> Self {
        Self {
            rect: Rect::ZERO,
            size: (width, height),
            art: live.get(),
            live,
            presented: None,
            layer,
            renderer,
            target: None,
            framing: None,
            ringed: false,
            dirty: true,
        }
    }

    #[must_use]
    pub fn framed(mut self, framing: Framing) -> Self {
        self.framing = Some(framing);
        self
    }

    #[must_use]
    pub fn ringed_while(mut self, chosen: &LocalState<String>, id: &str) -> Self {
        self.target = Some((chosen.clone(), id.to_owned()));
        self
    }

    /// Show the artwork as the document holds it now, and the ring the
    /// selection asks for: whether either changed.
    fn follow(&mut self) -> bool {
        let mut changed = false;
        if let Some(art) = self.live.changed_since(&mut self.presented) {
            self.art = art;
            changed = true;
        }
        let ringed = self
            .target
            .as_ref()
            .is_some_and(|(chosen, id)| chosen.with(|chosen| chosen == id));
        if ringed != self.ringed {
            self.ringed = ringed;
            changed = true;
        }
        self.dirty |= changed;
        changed
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
            size: taffy::Size::from_lengths(self.size.0, self.size.1),
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

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

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.follow();
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.follow()
    }

    ui::redraw_flag!();

    fn name(&self) -> &'static str {
        "Thumb"
    }
}

#[allow(
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
