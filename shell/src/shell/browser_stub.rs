//! Browser stub `AppWidget` — plan 3 Task 14.
//!
//! The native Servo-backed browser is out of scope for the nirios shell
//! milestone, so the Browser tile is hosted by this minimal stub. It
//! advertises itself as an `AppWidget` of kind `Browser` and exposes an
//! injectable runner (defaulting to `xdg-open`) so external URL launches
//! can be driven and unit-tested without spawning real processes.
//!
//! Visual rendering is intentionally minimal: a card with a "Browser coming
//! soon" headline. The drawing path is exercised by the apps integration
//! test in Task 16; the unit tests in `tests/browser_stub.rs` cover the
//! metadata and runner wiring.

use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};
use quick_ui::Theme;

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

/// Boxed callback invoked by `open_external`. Boxed (rather than a generic
/// parameter) so the shell can swap the runner at runtime and tests can
/// inject a recorder without carrying generics through `TileContent`.
pub type Runner = Box<dyn FnMut(&str) + Send + Sync>;

pub struct BrowserStubWidget {
    rect: Rect,
    runner: Runner,
    last_url: Option<String>,
}

impl BrowserStubWidget {
    /// Construct with the default runner (`xdg-open`).
    pub fn new() -> Self {
        Self::with_runner(Box::new(default_xdg_open))
    }

    /// Construct with a caller-supplied runner. Primary test injection seam.
    pub fn with_runner(runner: Runner) -> Self {
        Self {
            rect: Rect::ZERO,
            runner,
            last_url: None,
        }
    }

    /// Record the URL and invoke the runner. Exposed for test injection —
    /// the integration test in `apps_integration.rs` substitutes a recording
    /// runner via `with_runner`.
    pub fn open_external(&mut self, url: &str) {
        self.last_url = Some(url.to_string());
        (self.runner)(url);
    }

    /// The most recent URL passed through `open_external`.
    pub fn last_url(&self) -> Option<&str> {
        self.last_url.as_deref()
    }
}

impl Default for BrowserStubWidget {
    fn default() -> Self {
        Self::new()
    }
}

fn default_xdg_open(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

impl Widget for BrowserStubWidget {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Card background using the OS-provided surface tokens.
        let tokens = &theme.tokens;
        let radius = tokens.radii.lg as f64;
        let card = vello::kurbo::RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &card,
        );

        // Large browser glyph centred above the headline — same codepoint as
        // the `IconDescriptor::Glyph` returned below so the tab icon and the
        // card glyph render identically.
        let glyph = "󰖟";
        let glyph_size = tokens.typography.scale.x2l;
        let (gw, _gh) = text_renderer.measure(glyph, glyph_size, None);
        let gx = self.rect.center().x - gw as f64 / 2.0;
        let gy = self.rect.center().y - (glyph_size as f64) - 32.0;
        text_renderer.draw_text(
            scene,
            glyph,
            glyph_size,
            tokens.colors.text_primary,
            transform * Affine::translate((gx, gy)),
            None,
        );

        // Headline.
        let headline = "Browser coming soon";
        let headline_size = tokens.typography.scale.xl;
        let (hw, _) = text_renderer.measure(headline, headline_size, None);
        let hx = self.rect.center().x - hw as f64 / 2.0;
        let hy = self.rect.center().y;
        text_renderer.draw_text(
            scene,
            headline,
            headline_size,
            tokens.colors.text_primary,
            transform * Affine::translate((hx, hy)),
            None,
        );

        // Sub-line.
        let sub = "The native browser isn't ready yet.";
        let sub_size = tokens.typography.scale.sm;
        let (sw, _) = text_renderer.measure(sub, sub_size, None);
        let sx = self.rect.center().x - sw as f64 / 2.0;
        let sy = hy + headline_size as f64 + 8.0;
        text_renderer.draw_text(
            scene,
            sub,
            sub_size,
            tokens.colors.text_dim,
            transform * Affine::translate((sx, sy)),
            None,
        );
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &str {
        "BrowserStubWidget"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl AppWidget for BrowserStubWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Browser
    }
    fn app_title(&self) -> &str {
        "Browser"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰖟")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Browser)
    }
}
