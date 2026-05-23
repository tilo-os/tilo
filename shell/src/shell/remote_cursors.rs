//! Remote-cursor bounds helper, overlay widget, and a simulated feed.
//!
//! T24 landed the smallest load-bearing API: a pure predicate used when the
//! overlay decides which `PresenceCtx::remote_cursors` entries should render
//! over the active tile.
//!
//! T25 added `SimulatedFeed`, which emits a single deterministic fake remote
//! cursor so the chrome can be demoed without a real peer on the wire. It
//! writes directly into `PresenceCtx::remote_cursors`, which is the same
//! surface the real collaboration code feeds into, so the overlay widget
//! doesn't need to know whether its input is real or simulated.
//!
//! T26 fleshes out [`RemoteCursorOverlay`] into a real [`Widget`] that draws
//! a small caret triangle + label chip for each remote cursor currently over
//! the active tile. Cursors outside the tile's bounds are filtered out via
//! `cursor_in_bounds`, and the scene is clipped to the tile rect so the
//! overlay can never leak pixels into surrounding chrome. Per-cursor colors
//! come from `RemoteCursor::color` (RGBA f32); label text + chip background
//! use the theme's neutral surfaces so overlay text stays legible regardless
//! of peer color.
//!
//! The bounds are `[x, y, w, h]` in scene-space — the same coordinate space
//! that `PresenceCtx::remote_cursors[_].position` uses, so callers can pass
//! the active tile's layout rect directly.

/// Returns true when `pos` lies inside the rectangle `bounds = [x, y, w, h]`.
///
/// The check is half-open on the right and bottom edges, matching the
/// convention used by taffy-derived `Rect`s elsewhere in the shell.
pub fn cursor_in_bounds(pos: [f32; 2], bounds: [f32; 4]) -> bool {
    let [x, y, w, h] = bounds;
    let [px, py] = pos;
    px >= x && px < x + w && py >= y && py < y + h
}

/// Overlay widget that draws remote peer cursors on top of the active tile.
///
/// The active-tile bounds default to a zero rect; the shell sets them on each
/// frame before delegating draw. Cursors whose scene-space position falls
/// outside those bounds are filtered before draw and the scene is additionally
/// clipped to the same rect, so the overlay can never paint over surrounding
/// chrome even if a cursor position drifts slightly out of bounds mid-frame.
#[derive(Debug, Clone, Default)]
pub struct RemoteCursorOverlay {
    active_tile_bounds: [f32; 4],
    /// Layout rect assigned by taffy. Mirrors the active-tile bounds in
    /// steady state but is the authoritative rect for `Widget::get_rect`.
    rect: vello::kurbo::Rect,
}

impl RemoteCursorOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_tile_bounds(&self) -> [f32; 4] {
        self.active_tile_bounds
    }

    pub fn set_active_tile_bounds(&mut self, bounds: [f32; 4]) {
        self.active_tile_bounds = bounds;
    }

    /// Collects the remote cursors that should render on top of the active
    /// tile this frame. Cursors whose position falls outside the active tile
    /// rect are filtered out via `cursor_in_bounds`; the returned slice
    /// preserves the input order so peer-ordering is stable across frames.
    pub fn cursors_to_draw<'a>(
        &self,
        presence: &'a ui::presence::PresenceCtx,
    ) -> Vec<&'a ui::presence::RemoteCursor> {
        presence
            .remote_cursors
            .iter()
            .filter(|c| cursor_in_bounds(c.position, self.active_tile_bounds))
            .collect()
    }
}

/// Caret triangle half-width (in scene-space logical pixels). The caret
/// points down-and-right from the cursor position so the apex sits on the
/// actual pointer coordinate, matching the shared "peer caret" helper the
/// spec §Presence calls for.
const CARET_SIZE: f64 = 10.0;
/// Gap between the caret apex and the label chip's top-left corner.
const CHIP_OFFSET: f64 = 2.0;
/// Padding inside the label chip around the peer name text.
const CHIP_PADDING_X: f64 = 6.0;
const CHIP_PADDING_Y: f64 = 2.0;

/// Convert a peer's `[f32; 4]` RGBA color into a `vello::peniko::Color`.
fn peer_color_to_peniko(c: [f32; 4]) -> vello::peniko::Color {
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    vello::peniko::Color::rgba8(to_u8(c[0]), to_u8(c[1]), to_u8(c[2]), to_u8(c[3]))
}

impl ui::widget::Widget for RemoteCursorOverlay {
    fn sync(&mut self, _state: &mut automerge::AutoCommit, _path: &automerge::ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        // The overlay sits on top of the active tile; in the shell tree it's
        // stacked by a parent `Stack`, so it fills the available box and
        // relies on `active_tile_bounds` (set externally) for its clip rect.
        let node = taffy
            .new_leaf(taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn ui::widget::Widget);
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        let l = taffy.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let cursors = self.cursors_to_draw(presence);
        if cursors.is_empty() {
            return;
        }

        let [bx, by, bw, bh] = self.active_tile_bounds;
        // Skip when the overlay hasn't been positioned yet — drawing into a
        // zero-area clip is wasteful and vello rejects empty layers.
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let clip_rect = vello::kurbo::Rect::new(
            bx as f64,
            by as f64,
            (bx + bw) as f64,
            (by + bh) as f64,
        );

        // Clip to the active-tile rect so out-of-bounds cursors (e.g. mid-
        // frame drift between filter and draw) never paint over chrome.
        scene.push_layer(
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip_rect,
        );

        let label_size = theme.tokens.typography.scale.sm;
        let chip_bg = theme.tokens.colors.surfaces.raised;
        let chip_border = theme.tokens.colors.border_standard;
        let label_color = theme.tokens.colors.text_primary;
        let chip_radius = theme.tokens.radii.sm as f64;

        for cursor in cursors {
            let [px, py] = cursor.position;
            let apex = vello::kurbo::Point::new(px as f64, py as f64);
            let color = peer_color_to_peniko(cursor.color);

            // Caret triangle: apex at the cursor position, opening down-right.
            let mut caret = vello::kurbo::BezPath::new();
            caret.move_to(apex);
            caret.line_to((apex.x + CARET_SIZE, apex.y + CARET_SIZE * 0.4));
            caret.line_to((apex.x + CARET_SIZE * 0.4, apex.y + CARET_SIZE));
            caret.close_path();
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                color,
                None,
                &caret,
            );

            // Label chip: a rounded rect sized to the peer's name, tinted
            // with the raised surface so it remains legible over any tile
            // background, bordered with the peer color.
            let (text_w, _text_h) = text_renderer.measure(&cursor.label, label_size, None);
            let chip_w = text_w as f64 + CHIP_PADDING_X * 2.0;
            let chip_h = label_size as f64 + CHIP_PADDING_Y * 2.0;
            let chip_x = apex.x + CARET_SIZE + CHIP_OFFSET;
            let chip_y = apex.y + CARET_SIZE + CHIP_OFFSET;
            let chip_rect = vello::kurbo::Rect::new(
                chip_x,
                chip_y,
                chip_x + chip_w,
                chip_y + chip_h,
            );
            let chip = vello::kurbo::RoundedRect::from_rect(chip_rect, chip_radius);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                chip_bg,
                None,
                &chip,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                chip_border,
                None,
                &chip,
            );
            // Thin peer-colored underline so the chip's owner is obvious
            // even when two peers pick similar surface-tinted chips.
            let underline = vello::kurbo::Rect::new(
                chip_rect.x0,
                chip_rect.y1 - 2.0,
                chip_rect.x1,
                chip_rect.y1,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                color,
                None,
                &underline,
            );

            let text_y = chip_rect.y0 + CHIP_PADDING_Y;
            text_renderer.draw_text(
                scene,
                &cursor.label,
                label_size,
                label_color,
                transform
                    * vello::kurbo::Affine::translate((chip_rect.x0 + CHIP_PADDING_X, text_y)),
                None,
            );
        }

        scene.pop_layer();
    }

    fn handle_event(
        &mut self,
        _event: &winit::event::WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut automerge::AutoCommit,
        _path: &automerge::ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<taffy::NodeId>,
        _node_id: taffy::NodeId,
        _taffy: &taffy::TaffyTree<()>,
    ) {
        // The overlay is purely visual: remote cursor motion is fed in via
        // `PresenceCtx`, not winit events.
    }

    fn name(&self) -> &str {
        "RemoteCursorOverlay"
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }
}

/// Deterministic fake cursor source used by the shell demo/tests.
///
/// Spawns a single peer ("Ada") whose cursor traces a smooth Lissajous-ish
/// path derived from accumulated time, so scrubbing through frames or
/// replaying with the same seed always produces the same trajectory. The
/// seed only affects the peer's display color — the motion is seed-independent
/// so tests can assert position equality/inequality without knowing the seed.
#[derive(Debug, Clone)]
pub struct SimulatedFeed {
    t: std::time::Duration,
    seed: u64,
    user: String,
    color: [f32; 4],
}

impl SimulatedFeed {
    /// Creates a feed with a single peer "Ada" whose display color is derived
    /// from `seed`. The accumulated time starts at zero so the first `tick`
    /// produces a position offset from the idle origin.
    pub fn new_with_seed(seed: u64) -> Self {
        Self {
            t: std::time::Duration::ZERO,
            seed,
            user: "Ada".to_string(),
            color: color_from_seed(seed),
        }
    }

    /// Creates a feed pinned to a concept-known user id. The id runs through
    /// [`ui::presence::user_color`], so canonical ids (`me`, `alice`,
    /// `bob`, `charlie`) pick up the reference-mockup palette verbatim. The
    /// `display_name` is what renders in the name chip above the caret — it
    /// is decoupled from the id so a peer can show up as "Alice" while
    /// staying attributed to the `alice` pinned colour.
    pub fn new_with_user_id(user_id: &str, display_name: &str) -> Self {
        let (r, g, b) = ui::presence::user_color(user_id);
        let color = [
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            1.0,
        ];
        Self {
            t: std::time::Duration::ZERO,
            // Seed is unused on the render path for user-id-pinned feeds;
            // kept non-zero so the `sim-<seed>` user_id on the RemoteCursor
            // stays distinct from a seed=0 feed if both ever coexist.
            seed: 0xa11ce,
            user: display_name.to_string(),
            color,
        }
    }

    /// Advances the feed by `dt` and rewrites `ctx.remote_cursors` with the
    /// single simulated peer's current position. Subsequent calls replace the
    /// vec contents so a single feed never leaks multiple phantom cursors.
    pub fn tick(&mut self, dt: std::time::Duration, ctx: &mut ui::presence::PresenceCtx) {
        self.t += dt;
        let t_secs = self.t.as_secs_f32();
        let position = [
            200.0 + 80.0 * t_secs.sin(),
            150.0 + 60.0 * (t_secs * 0.7).cos(),
        ];
        ctx.remote_cursors.clear();
        ctx.remote_cursors.push(ui::presence::RemoteCursor {
            user_id: format!("sim-{}", self.seed),
            color: self.color,
            position,
            label: self.user.clone(),
        });
    }
}

/// Maps `seed` to a stable, pleasant-looking RGBA color. We avoid pulling a
/// random generator into the hot path: a splitmix-style mix of the seed gives
/// us three independent bytes, normalised to `[0.3, 1.0]` so the cursor stays
/// visible over both light and dark tile backgrounds.
fn color_from_seed(seed: u64) -> [f32; 4] {
    let mut z = seed.wrapping_add(0x9e3779b97f4a7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^= z >> 31;
    let r = ((z & 0xff) as f32) / 255.0;
    let g = (((z >> 8) & 0xff) as f32) / 255.0;
    let b = (((z >> 16) & 0xff) as f32) / 255.0;
    // Lift each channel into [0.3, 1.0] so no component is too dark.
    let lift = |c: f32| 0.3 + 0.7 * c;
    [lift(r), lift(g), lift(b), 1.0]
}
