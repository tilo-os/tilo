//! Browser stub `AppWidget`.
//!
//! The native Servo-backed browser is out of scope for the nirios shell
//! milestone, so the Browser tile is hosted by this minimal stub. It
//! advertises itself as an `AppWidget` of kind `Browser` and exposes an
//! injectable runner (defaulting to `xdg-open`) so external URL launches
//! can be driven and unit-tested without spawning real processes.
//!
//! Visual rendering is intentionally minimal: a card with a "Browser coming
//! soon" headline. The unit tests in `tests/browser_stub.rs` cover the
//! metadata and runner wiring.

use ui::ColorCompat;
use ui::Theme;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

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
    /// Scene cache — the stub is purely a function of `rect`'s size, so
    /// this is encoded once and replayed via the outer transform.
    /// Mirrors `ui::widget::button` and `shell::desktop_backdrop`.
    cached: Scene,
    /// Rect at which `cached` was encoded. `Rect::ZERO` means no cache
    /// yet; a size mismatch triggers a rebuild.
    cache_rect: Rect,
}

/// Bundles the render/theme handles [`BrowserStubWidget::draw`]'s
/// per-section paint helpers need, so each helper takes one context
/// argument instead of four positional ones.
struct PaintCtx<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text_renderer: &'a mut TextRenderer,
    theme: &'a Theme,
}

/// Bit-exact float inequality, for cache-invalidation checks that need
/// to catch any change — including one too small for a tolerance-based
/// comparison to notice — rather than clippy's `float_cmp`-flagged `!=`.
fn f64_ne(a: f64, b: f64) -> bool {
    a.to_bits() != b.to_bits()
}

impl BrowserStubWidget {
    pub fn new() -> Self {
        Self::with_runner(Box::new(default_xdg_open))
    }

    /// Primary test injection seam.
    pub fn with_runner(runner: Runner) -> Self {
        Self {
            rect: Rect::ZERO,
            runner,
            last_url: None,
            cached: Scene::new(),
            cache_rect: Rect::ZERO,
        }
    }

    pub fn open_external(&mut self, url: &str) {
        self.last_url = Some(url.to_string());
        (self.runner)(url);
    }

    pub fn last_url(&self) -> Option<&str> {
        self.last_url.as_deref()
    }

    /// The outer rounded card, clipped against `rect` so the chrome
    /// paints inside the host's tile boundary. Matches
    /// `nirios-concept/src/components/apps/BrowserApp.tsx`. Split out
    /// of [`Widget::draw`].
    fn paint_card_background(ctx: &mut PaintCtx, rect: Rect) {
        let tokens = &ctx.theme.tokens;
        let radius = f64::from(tokens.radii.lg);
        let card = vello::kurbo::RoundedRect::from_rect(rect, radius);
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            tokens.colors.surfaces.window_card,
            None,
            &card,
        );
    }

    /// The chrome strip: its background, the divider under it, and the
    /// three (uncoloured) traffic-light dots. Split out of
    /// [`Widget::draw`].
    fn paint_chrome_bar(ctx: &mut PaintCtx, rect: Rect, chrome_h: f64) {
        let tokens = &ctx.theme.tokens;
        let chrome_rect = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + chrome_h);
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            tokens.colors.surfaces.raised,
            None,
            &chrome_rect,
        );
        let divider = Rect::new(
            rect.x0,
            rect.y0 + chrome_h - 1.0,
            rect.x1,
            rect.y0 + chrome_h,
        );
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            tokens.colors.border_standard,
            None,
            &divider,
        );

        // Concept doesn't colour the traffic lights red/yellow/green.
        let light_y = rect.y0 + chrome_h / 2.0;
        for i in 0..3 {
            let cx = rect.x0 + 22.0 + f64::from(i) * 18.0;
            let circ = vello::kurbo::Circle::new(Point::new(cx, light_y), 6.0);
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ctx.transform,
                tokens.colors.text_dim,
                None,
                &circ,
            );
        }
    }

    /// The pill-shaped address bar with its globe glyph and placeholder
    /// URL — skipped when `rect` is too narrow for it to read cleanly.
    /// Split out of [`Widget::draw`].
    fn paint_address_bar(ctx: &mut PaintCtx, rect: Rect, chrome_h: f64) {
        let light_y = rect.y0 + chrome_h / 2.0;
        // Concept clamps the address bar to `max-w-2xl` (≈ 672 px); here
        // it's the card width minus the gutter and padding instead.
        let bar_left = rect.x0 + 78.0;
        let bar_right = (rect.x1 - 24.0).min(rect.x0 + 78.0 + 560.0);
        if bar_right <= bar_left + 80.0 {
            return;
        }
        let tokens = &ctx.theme.tokens;
        let bar_rect = Rect::new(bar_left, light_y - 14.0, bar_right, light_y + 14.0);
        let pill = vello::kurbo::RoundedRect::from_rect(bar_rect, 14.0);
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            tokens.colors.surfaces.window_card,
            None,
            &pill,
        );
        ctx.scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            ctx.transform,
            tokens.colors.border_standard,
            None,
            &pill,
        );

        let globe = "󰖟";
        let glyph_size = tokens.typography.scale.base;
        let (ggw, ggh) = ctx.text_renderer.measure(globe, glyph_size, None);
        let gx = bar_rect.x0 + 14.0;
        let gy = bar_rect.center().y - f64::from(ggh) / 2.0;
        ctx.text_renderer.draw_text(
            ctx.scene,
            globe,
            glyph_size,
            tokens.colors.text_dim,
            ctx.transform.pre_translate((gx, gy)),
            None,
        );
        let url = "localhost:3000";
        let (_uw, uh) = ctx.text_renderer.measure(url, glyph_size, None);
        let ux = gx + f64::from(ggw) + 10.0;
        let uy = bar_rect.center().y - f64::from(uh) / 2.0;
        ctx.text_renderer.draw_text(
            ctx.scene,
            url,
            glyph_size,
            tokens.colors.text_dim,
            ctx.transform.pre_translate((ux, uy)),
            None,
        );
    }

    /// The "App Preview" tile + headline + subtitle beneath the address
    /// bar — painted only if there's enough vertical room for the whole
    /// composition. Split out of [`Widget::draw`].
    fn paint_preview_tile(ctx: &mut PaintCtx, rect: Rect, chrome_h: f64) {
        let body_top = rect.y0 + chrome_h + 16.0;
        if rect.y1 - body_top <= 200.0 {
            return;
        }
        let tokens = &ctx.theme.tokens;
        let tile_size = 96.0_f64;
        // A `Point` rather than two `tile_c{x,y}` locals — same value,
        // but doesn't read as two near-identical bindings in one scope.
        let tile_center = Point::new(rect.center().x, body_top + 96.0);
        let tile_rect = Rect::new(
            tile_center.x - tile_size / 2.0,
            tile_center.y - tile_size / 2.0,
            tile_center.x + tile_size / 2.0,
            tile_center.y + tile_size / 2.0,
        );
        let tile = vello::kurbo::RoundedRect::from_rect(tile_rect, 20.0);
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            tokens.colors.accent[2].with_alpha_factor(0.18),
            None,
            &tile,
        );
        let inner_size = 48.0_f64;
        let inner_rect = Rect::new(
            tile_center.x - inner_size / 2.0,
            tile_center.y - inner_size / 2.0,
            tile_center.x + inner_size / 2.0,
            tile_center.y + inner_size / 2.0,
        );
        let inner = vello::kurbo::RoundedRect::from_rect(inner_rect, 10.0);
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            tokens.colors.accent[5],
            None,
            &inner,
        );

        // `text_primary` (pure white) would be invisible on this white
        // card, so use dark-on-white instead.
        let on_card_headline = tokens.colors.neutral[1];
        let on_card_subtle = tokens.colors.neutral[4];

        let headline = "App Preview";
        let headline_size = tokens.typography.scale.xl;
        let (hw, _hh) = ctx.text_renderer.measure(headline, headline_size, None);
        let hx = rect.center().x - f64::from(hw) / 2.0;
        let hy = tile_center.y + tile_size / 2.0 + 24.0;
        ctx.text_renderer.draw_text(
            ctx.scene,
            headline,
            headline_size,
            on_card_headline,
            ctx.transform.pre_translate((hx, hy)),
            None,
        );

        let sub = "Running locally on port 3000";
        let sub_size = tokens.typography.scale.sm;
        let (sw, _) = ctx.text_renderer.measure(sub, sub_size, None);
        let sx = rect.center().x - f64::from(sw) / 2.0;
        let sy = hy + f64::from(headline_size) + 8.0;
        ctx.text_renderer.draw_text(
            ctx.scene,
            sub,
            sub_size,
            on_card_subtle,
            ctx.transform.pre_translate((sx, sy)),
            None,
        );
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
        let node = match taffy.new_leaf(taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        }) {
            Ok(node) => node,
            Err(err) => {
                // `TaffyTree::new_leaf` only ever returns `Ok` for a
                // fresh, unattached leaf; this arm exists so a future
                // upstream change degrades the browser stub instead of
                // aborting the whole session.
                tracing::error!("browser_stub: leaf allocation failed: {err}");
                NodeId::from(0u64)
            }
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Purely a function of rect size; replay via translation when
        // only the position moved. Bit-exact comparison, not "close
        // enough": any change (even one ULP) must invalidate the cache,
        // so a tolerant comparison would let a real resize slip through
        // unnoticed.
        let size_changed = f64_ne(self.rect.width(), self.cache_rect.width())
            || f64_ne(self.rect.height(), self.cache_rect.height());
        let needs_rebuild = self.cache_rect == Rect::ZERO || size_changed;
        if !needs_rebuild {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            // `pre_translate` is kurbo's non-operator equivalent of
            // `out_transform * Affine::translate((dx, dy))`.
            out_scene.append(&self.cached, Some(out_transform.pre_translate((dx, dy))));
            return;
        }

        // `std::mem::take` keeps the borrow checker happy across the
        // `&self.rect` read below (no closure over `&mut self`).
        let mut local = std::mem::take(&mut self.cached);
        local.reset();
        let rect = self.rect;
        let mut ctx = PaintCtx {
            scene: &mut local,
            transform: Affine::IDENTITY,
            text_renderer,
            theme,
        };
        let chrome_h = 48.0_f64;
        Self::paint_card_background(&mut ctx, rect);
        Self::paint_chrome_bar(&mut ctx, rect, chrome_h);
        Self::paint_address_bar(&mut ctx, rect, chrome_h);
        Self::paint_preview_tile(&mut ctx, rect, chrome_h);

        out_scene.append(&local, Some(out_transform));
        self.cached = local;
        self.cache_rect = self.rect;
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

    fn name(&self) -> &'static str {
        "BrowserStubWidget"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The painted pixels are frame-invariant once `cache_rect` is
    /// populated, so not-dirty until a resize (caught by the cache's
    /// size-mismatch check) or a fresh instance (`cache_rect == ZERO`).
    fn needs_redraw(&self) -> bool {
        self.cache_rect == Rect::ZERO
    }

    /// No intrinsic size — stretches to `percent(1.0)` of its tile
    /// slot, so layout is clean after the first pass.
    fn is_layout_dirty(&self) -> bool {
        self.cache_rect == Rect::ZERO
    }

    fn mark_drawn(&mut self) {
        // `cache_rect` carries the "has been drawn" signal directly.
    }
}

impl AppWidget for BrowserStubWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Browser
    }
    fn app_title(&self) -> &'static str {
        "Browser"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰖟")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Browser)
    }
}
