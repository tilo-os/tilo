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
        // Concept-style browser chrome: white card, 48 px header with three
        // traffic-light circles on the left + a pill-shaped address bar
        // centred in the remaining width, and a body with a placeholder
        // "App Preview" canvas. Matches
        // `nirios-concept/src/components/apps/BrowserApp.tsx`; the outer
        // rounded card is clipped against `self.rect` so the chrome paints
        // inside the host's tile boundary.
        let tokens = &theme.tokens;
        let radius = tokens.radii.lg as f64;
        let card = vello::kurbo::RoundedRect::from_rect(self.rect, radius);

        // White card background, matching concept's `bg-white`.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.window_card,
            None,
            &card,
        );

        let chrome_h = 48.0_f64;
        let chrome_rect = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + chrome_h,
        );

        // Chrome strip — slate-50-ish tone. The window card is already
        // white, so we overlay a thin slate-tinted band clipped to the
        // chrome rect for the header.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &chrome_rect,
        );
        // Divider under the chrome.
        let divider = Rect::new(
            self.rect.x0,
            self.rect.y0 + chrome_h - 1.0,
            self.rect.x1,
            self.rect.y0 + chrome_h,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.border_standard,
            None,
            &divider,
        );

        // Three traffic-light circles (slate-300-ish, concept doesn't
        // colour them red/yellow/green). 12 px diameter, 8 px apart,
        // starting 16 px from the chrome's left edge.
        let light_y = self.rect.y0 + chrome_h / 2.0;
        for i in 0..3 {
            let cx = self.rect.x0 + 22.0 + (i as f64) * 18.0;
            let circ = vello::kurbo::Circle::new(Point::new(cx, light_y), 6.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.text_dim,
                None,
                &circ,
            );
        }

        // Address-bar pill. Concept clamps it to `max-w-2xl` (≈ 672 px);
        // here we clamp by the card width minus the traffic-light gutter
        // and right-hand padding, so it centres inside the remaining band.
        let bar_left = self.rect.x0 + 78.0;
        let bar_right = (self.rect.x1 - 24.0).min(self.rect.x0 + 78.0 + 560.0);
        if bar_right > bar_left + 80.0 {
            let bar_rect = Rect::new(bar_left, light_y - 14.0, bar_right, light_y + 14.0);
            let pill = vello::kurbo::RoundedRect::from_rect(bar_rect, 14.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.surfaces.window_card,
                None,
                &pill,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                tokens.colors.border_standard,
                None,
                &pill,
            );

            // Globe glyph + URL text inside the pill.
            let globe = "󰖟";
            let glyph_size = tokens.typography.scale.base;
            let (ggw, ggh) = text_renderer.measure(globe, glyph_size, None);
            let gx = bar_rect.x0 + 14.0;
            let gy = bar_rect.center().y - ggh as f64 / 2.0;
            text_renderer.draw_text(
                scene,
                globe,
                glyph_size,
                tokens.colors.text_dim,
                transform * Affine::translate((gx, gy)),
                None,
            );
            let url = "localhost:3000";
            let (_uw, uh) = text_renderer.measure(url, glyph_size, None);
            let ux = gx + ggw as f64 + 10.0;
            let uy = bar_rect.center().y - uh as f64 / 2.0;
            text_renderer.draw_text(
                scene,
                url,
                glyph_size,
                tokens.colors.text_dim,
                transform * Affine::translate((ux, uy)),
                None,
            );
        }

        // Body: a centred indigo tile + headline, in the concept's
        // "App Preview" placeholder style. Painted only if we have
        // enough vertical room for the whole composition.
        let body_top = self.rect.y0 + chrome_h + 16.0;
        if self.rect.y1 - body_top > 200.0 {
            let tile_size = 96.0_f64;
            let tile_cx = self.rect.center().x;
            let tile_cy = body_top + 96.0;
            let tile_rect = Rect::new(
                tile_cx - tile_size / 2.0,
                tile_cy - tile_size / 2.0,
                tile_cx + tile_size / 2.0,
                tile_cy + tile_size / 2.0,
            );
            let tile = vello::kurbo::RoundedRect::from_rect(tile_rect, 20.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.accent[2].with_alpha_factor(0.18),
                None,
                &tile,
            );
            let inner_size = 48.0_f64;
            let inner_rect = Rect::new(
                tile_cx - inner_size / 2.0,
                tile_cy - inner_size / 2.0,
                tile_cx + inner_size / 2.0,
                tile_cy + inner_size / 2.0,
            );
            let inner = vello::kurbo::RoundedRect::from_rect(inner_rect, 10.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.accent[5],
                None,
                &inner,
            );

            // On the white browser card `text_primary` (pure white) would
            // be invisible — use the dark-on-white pairing the concept shows
            // (`text-slate-800` headline, `text-slate-500` subtitle).
            let on_card_headline = tokens.colors.neutral[1]; // slate-900 in dark theme
            let on_card_subtle = tokens.colors.neutral[4]; // slate-600

            let headline = "App Preview";
            let headline_size = tokens.typography.scale.xl;
            let (hw, _hh) = text_renderer.measure(headline, headline_size, None);
            let hx = self.rect.center().x - hw as f64 / 2.0;
            let hy = tile_cy + tile_size / 2.0 + 24.0;
            text_renderer.draw_text(
                scene,
                headline,
                headline_size,
                on_card_headline,
                transform * Affine::translate((hx, hy)),
                None,
            );

            let sub = "Running locally on port 3000";
            let sub_size = tokens.typography.scale.sm;
            let (sw, _) = text_renderer.measure(sub, sub_size, None);
            let sx = self.rect.center().x - sw as f64 / 2.0;
            let sy = hy + headline_size as f64 + 8.0;
            text_renderer.draw_text(
                scene,
                sub,
                sub_size,
                on_card_subtle,
                transform * Affine::translate((sx, sy)),
                None,
            );
        }
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
