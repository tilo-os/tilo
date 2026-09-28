//! Remote-cursor bounds helper and overlay widget.
//!
//! There is deliberately no simulated feed here: a fabricated peer is
//! indistinguishable on screen from a real one, which makes "is
//! collaboration working?" unanswerable by looking. Peers reach this
//! overlay from `shell::shell::peer_presence` or not at all.
//!
//! [`RemoteCursorOverlay`] draws a caret triangle + label chip for each
//! remote cursor over the active tile, clipped to the tile rect so it can
//! never leak pixels into surrounding chrome. Bounds are `[x, y, w, h]` in
//! scene-space, matching `PresenceCtx::remote_cursors[_].position`.

/// Returns true when `pos` lies inside `bounds = [x, y, w, h]`, half-open
/// on the right and bottom edges, matching taffy-derived `Rect`s elsewhere.
pub fn cursor_in_bounds(pos: [f32; 2], bounds: [f32; 4]) -> bool {
    let [x, y, w, h] = bounds;
    let [px, py] = pos;
    px >= x && px < x + w && py >= y && py < y + h
}

/// Overlay widget that draws remote peer cursors on top of the active tile.
///
/// Bounds default to zero; the shell sets them each frame before draw.
/// Out-of-bounds cursors are filtered before draw and the scene is also
/// clipped to the rect, so a cursor drifting mid-frame still can't paint
/// over surrounding chrome.
#[derive(Debug, Clone, Default)]
pub struct RemoteCursorOverlay {
    active_tile_bounds: [f32; 4],
    /// Layout rect assigned by taffy — the authoritative rect for
    /// `Widget::get_rect`.
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

    /// Remote cursors that should render on top of the active tile this
    /// frame, in stable input order.
    pub fn cursors_to_draw<'a>(
        &self,
        presence: &'a ui::presence::PresenceCtx,
    ) -> Vec<&'a ui::presence::RemoteCursor> {
        presence
            .remote_cursors
            .iter()
            // A pointer the compositor overlay already paints is not
            // painted again here — see `ui::presence::PointerOwner`.
            .filter(|c| c.painted_by_window())
            .filter(|c| cursor_in_bounds(c.position, self.active_tile_bounds))
            .collect()
    }
}

/// Caret triangle half-width (scene-space logical pixels); points
/// down-and-right so the apex sits on the actual pointer coordinate.
const CARET_SIZE: f64 = 10.0;
/// Gap between the caret apex and the label chip's top-left corner.
const CHIP_OFFSET: f64 = 2.0;
/// Padding inside the label chip around the peer name text.
const CHIP_PADDING_X: f64 = 6.0;
const CHIP_PADDING_Y: f64 = 2.0;

/// Convert a peer's `[f32; 4]` RGBA color into a `vello::peniko::Color`.
fn peer_color_to_peniko(c: [f32; 4]) -> vello::peniko::Color {
    // Clamped into u8's range on the line above, so the truncation and
    // sign-loss this cast could otherwise cause cannot happen.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "explicitly clamped into u8's range immediately before the cast"
    )]
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    vello::peniko::Color::from_rgba8(to_u8(c[0]), to_u8(c[1]), to_u8(c[2]), to_u8(c[3]))
}

impl ui::widget::Widget for RemoteCursorOverlay {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        // Stacked by a parent `Stack`, so it fills the available box and
        // relies on `active_tile_bounds` (set externally) for its clip rect.
        // `new_leaf` can't fail in practice for a single-node insert; a
        // dummy id here just means the fallback is unreachable by real
        // traversal instead of panicking.
        let node = taffy
            .new_leaf(taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap_or(taffy::NodeId::new(0));
        node_map.insert(node, std::ptr::from_mut::<dyn ui::widget::Widget>(self));
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = vello::kurbo::Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        // vello rejects an empty layer, and the overlay may not be positioned yet.
        if bw <= 0.0 || bh <= 0.0 {
            return;
        }
        let clip_rect = vello::kurbo::Rect::new(
            f64::from(bx),
            f64::from(by),
            f64::from(bx + bw),
            f64::from(by + bh),
        );

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip_rect,
        );

        // Theme type tokens already carry the user's text scale, and this
        // overlay paints with those resolved sizes; the renderer must not
        // scale them again (HIG §7a) — reset for this pass, as the shell's
        // immediate-mode chrome passes do, and restored below.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        let label_size = theme.tokens.typography.scale.sm;
        let chip_bg = theme.tokens.colors.surfaces.raised;
        // The chip's edge derives from its own fill (HIG §9.5): the declared
        // border was three shades off `raised` on dark and vanished.
        let chip_border = ui::surface::edge_on(chip_bg);
        let label_color = theme.tokens.colors.text_primary;
        let chip_radius = f64::from(theme.tokens.radii.sm);

        for cursor in cursors {
            let [px, py] = cursor.position;
            let apex = vello::kurbo::Point::new(f64::from(px), f64::from(py));
            let color = peer_color_to_peniko(cursor.color);

            let mut caret = vello::kurbo::BezPath::new();
            caret.move_to(apex);
            caret.line_to((apex.x + CARET_SIZE, apex.y + CARET_SIZE * 0.4));
            caret.line_to((apex.x + CARET_SIZE * 0.4, apex.y + CARET_SIZE));
            caret.close_path();
            scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &caret);

            // The chip wraps the label's measured line, not its font size:
            // a 12 px face lays out taller than 12 px, and sizing the chip
            // from the size alone let the glyphs ride onto the underline.
            let (text_w, text_h) = text_renderer.measure(&cursor.label, label_size, None);
            let chip_size = vello::kurbo::Size::new(
                f64::from(text_w) + CHIP_PADDING_X * 2.0,
                f64::from(text_h) + CHIP_PADDING_Y * 2.0,
            );
            let chip_left = apex.x + CARET_SIZE + CHIP_OFFSET;
            let chip_top = apex.y + CARET_SIZE + CHIP_OFFSET;
            let chip_bounds = vello::kurbo::Rect::from_origin_size(
                vello::kurbo::Point::new(chip_left, chip_top),
                chip_size,
            );
            let chip = vello::kurbo::RoundedRect::from_rect(chip_bounds, chip_radius);
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
            // Peer-colored underline so the chip's owner is obvious.
            let underline = vello::kurbo::Rect::new(
                chip_bounds.x0,
                chip_bounds.y1 - 2.0,
                chip_bounds.x1,
                chip_bounds.y1,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                color,
                None,
                &underline,
            );

            let text_y = chip_bounds.y0 + CHIP_PADDING_Y;
            text_renderer.draw_text(
                scene,
                &cursor.label,
                label_size,
                label_color,
                transform.pre_translate(vello::kurbo::Vec2::new(
                    chip_bounds.x0 + CHIP_PADDING_X,
                    text_y,
                )),
                None,
            );
        }

        text_renderer.set_text_scale(renderer_scale);
        scene.pop_layer();
    }

    fn name(&self) -> &'static str {
        "RemoteCursorOverlay"
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }
}
