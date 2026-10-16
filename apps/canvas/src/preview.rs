//! A past version shown in place of the artwork while the person looks
//! back through its history: drawn over the canvas, read-only, until they
//! restore it or return to now.

use ui::LocalState;
use ui::automerge::AutoCommit;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect, Stroke};
use ui::vello::peniko::Fill;
use ui::widget::{AppModel, Element, EventCtx, ModelPath, Widget};
use ui::winit::event::{ElementState, KeyEvent, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};
use ui::{PresenceCtx, TextRenderer, Theme};

use crate::model::Artwork;
use crate::render::{Options, Renderer};

/// The canvas, with the version `preview` names drawn over it.
pub struct VersionPreview {
    canvas: Element,
    preview: LocalState<Option<String>>,
    /// The version shown and the artwork as it was then.
    shown: Option<(String, Artwork)>,
    /// Its own renderer: a past version's layers must not replace the live
    /// ones' simulations.
    renderer: Renderer,
    cursor: Point,
}

impl VersionPreview {
    pub fn new(
        canvas: Element,
        preview: LocalState<Option<String>>,
        content: Option<tilo_content::ContentStore>,
    ) -> Self {
        let mut renderer = Renderer::default();
        if let Some(content) = content {
            renderer.pictures.read_from(content);
        }
        Self {
            canvas,
            preview,
            shown: None,
            renderer,
            cursor: Point::ZERO,
        }
    }

    /// The artwork at `version`, a change id or a checkpoint name.
    fn read(doc: &AutoCommit, version: &str) -> Option<Artwork> {
        let heads = ui::history_source::steps::resolve(doc, version).ok()?;
        let past = doc.committed_fork_at(&heads).ok()?;
        Artwork::from_doc(&past).ok()
    }

    fn rect(&self) -> Rect {
        self.canvas.get_rect()
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "kurbo's Affine composition is float geometry"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let rect = self.rect();
        let Some((_, art)) = &self.shown else {
            return;
        };
        scene.push_layer(
            Fill::NonZero,
            ui::vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &rect,
        );
        // The past version hides the live artwork: the content ground over
        // it, and the version's sheet on that as the canvas lays its own.
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.content,
            None,
            &rect,
        );
        let margin = f64::from(theme.tokens.spacing.s6);
        let room = rect.inset(-margin);
        if room.width() > 1.0 && room.height() > 1.0 && art.width > 0.0 && art.height > 0.0 {
            let scale = (room.width() / art.width).min(room.height() / art.height);
            let origin = Point::new(
                room.center().x - art.width * scale / 2.0,
                room.center().y - art.height * scale / 2.0,
            );
            let place = Affine::translate(origin.to_vec2()) * Affine::scale(scale);
            let sheet = place.transform_rect_bbox(art.bounds());
            ui::surface::shadow(
                scene,
                transform,
                sheet,
                0.0,
                &theme.tokens.elevation.floating,
            );
            self.renderer
                .draw(art, scene, transform * place, text, &Options::default());
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                ui::surface::edge_on(colors.surfaces.content),
                None,
                &sheet,
            );
        }
        // A badge at the top says this is not the live artwork.
        let label = ui::i18n::I18nManager::lookup_plain("canvas-preview-banner");
        let width = ui::widget::BadgeFace::intrinsic_width(&label, text, theme);
        let height = ui::widget::BadgeFace::height(theme);
        let pad = f64::from(theme.tokens.spacing.s4);
        let badge = Rect::from_origin_size(
            Point::new(rect.center().x - width / 2.0, rect.y0 + pad),
            (width, height),
        );
        ui::widget::BadgeFace::new(badge, &label)
            .style(ui::widget::ChipStyle::Neutral)
            .paint(scene, transform, text, theme);
        scene.pop_layer();
    }
}

impl ui::widget::Decorator for VersionPreview {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.canvas.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.canvas.as_mut()
    }

    fn concrete_type_name(&self) -> &'static str {
        "VersionPreview"
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let wanted = self.preview.with(Clone::clone);
        if wanted.as_deref() != self.shown.as_ref().map(|(version, _)| version.as_str()) {
            let (doc, _) = ui::doc::document_ref(state, path);
            self.shown = wanted.and_then(|version| {
                let art = Self::read(doc, &version)?;
                Some((version, art))
            });
        }
        self.canvas.sync(state, path);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.canvas
            .draw(scene, transform, text_renderer, theme, presence);
        self.paint(scene, transform, text_renderer, theme);
    }

    fn needs_redraw(&self) -> bool {
        self.canvas.needs_redraw() || self.preview.with(Option::is_some) != self.shown.is_some()
    }

    /// While a past version is shown the canvas beneath takes no input;
    /// Escape returns to now.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = ctx.logical(position);
        }
        if self.shown.is_none() {
            self.canvas.handle_event(event, ctx);
            return;
        }
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.preview.set(None);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. }
                if self.rect().contains(self.cursor) =>
            {
                *ctx.handled = true;
            }
            _ => self.canvas.handle_event(event, ctx),
        }
    }
}
