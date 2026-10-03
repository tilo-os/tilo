//! A past version shown in place of the artwork while the person looks
//! back through its history: drawn over the canvas, read-only, until they
//! restore it or return to now.

use ui::LocalState;
use ui::automerge::AutoCommit;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect, Stroke, Vec2};
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
    pub fn new(canvas: Element, preview: LocalState<Option<String>>) -> Self {
        Self {
            canvas,
            preview,
            shown: None,
            renderer: Renderer::default(),
            cursor: Point::ZERO,
        }
    }

    /// The artwork at `version`, a change id or a checkpoint name.
    fn read(doc: &mut AutoCommit, version: &str) -> Option<Artwork> {
        let heads = ui::history_source::steps::resolve(doc, version).ok()?;
        let past = doc.fork_at(&heads).ok()?;
        Some(crate::doc::read(&past))
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
        scene.fill(Fill::NonZero, transform, theme.bg_app, None, &rect);
        let margin = f64::from(theme.tokens.spacing.s6);
        let room = rect.inset(-margin);
        if room.width() > 1.0 && room.height() > 1.0 && art.width > 0.0 && art.height > 0.0 {
            let scale = (room.width() / art.width).min(room.height() / art.height);
            let origin = Point::new(
                room.center().x - art.width * scale / 2.0,
                room.center().y - art.height * scale / 2.0,
            );
            let to_screen = transform * Affine::translate(origin.to_vec2()) * Affine::scale(scale);
            self.renderer
                .draw(art, scene, to_screen, text, &Options::default());
            scene.stroke(
                &Stroke::new(1.0 / scale),
                to_screen,
                theme.border_standard,
                None,
                &art.bounds(),
            );
        }
        // An accent rule round the view says this is not the live artwork.
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.accent_primary,
            None,
            &rect.inset(-1.0),
        );
        let label = ui::i18n::I18nManager::lookup_plain("canvas-preview-banner");
        let size = theme.tokens.typography.scale.sm;
        let (width, height) = text.measure(&label, size, None);
        let pad = f64::from(theme.tokens.spacing.s2);
        let pill = Rect::from_origin_size(
            Point::new(
                rect.center().x - f64::from(width) / 2.0 - pad * 2.0,
                rect.y0 + pad * 2.0,
            ),
            (f64::from(width) + pad * 4.0, f64::from(height) + pad * 2.0),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.accent_primary,
            None,
            &pill.to_rounded_rect(pill.height() / 2.0),
        );
        text.draw_text(
            scene,
            &label,
            size,
            theme
                .tokens
                .colors
                .on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            transform * Affine::translate(pill.origin().to_vec2() + Vec2::new(pad * 2.0, pad)),
            None,
        );
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

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = self.canvas.build_layout(taffy, theme, node_map);
        // The host reaches the canvas through this view of it.
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let wanted = self.preview.with(Clone::clone);
        if wanted.as_deref() != self.shown.as_ref().map(|(version, _)| version.as_str()) {
            let (doc, _) = ui::doc::document(state, path);
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

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.canvas.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
        self.paint(scene, transform, text_renderer, theme);
    }

    fn needs_redraw(&self) -> bool {
        self.canvas.needs_redraw() || self.preview.with(Option::is_some) != self.shown.is_some()
    }

    /// While a past version is shown the canvas beneath takes no input;
    /// Escape returns to now.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
