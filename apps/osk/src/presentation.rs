//! Immutable keyboard geometry and paint shared by the live root and History.
use crate::layout::{Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
use crate::suggest::{Suggestion, suggestion_rects};
use crate::toolbar::{OskToolbarItem, toolbar_rects};
use crate::touch_model::{CORE_FRACTION, HitCell, core_zone};
use ui::chrome::{SelectionState, selection_floor_ramp};
use ui::text_render::TextWeight;
use ui::widget::{Button, ButtonFace, ButtonRole, ChipFace, ChipLead};
use ui::{
    TextRenderer, TextRole, TextStyle, Theme,
    icons::{self, Icon},
    paint::PaintCtx,
};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
    peniko::Color,
};

/// Widest the keyboard content grows; wider surfaces centre it.
pub(crate) const MAX_KEYBOARD_CONTENT_WIDTH: f64 = 1_120.0;

/// A glyph key's character, as a fraction of its cell height. Keys are a
/// content surface: the glyph follows the key it sits on, not the text scale.
const KEY_GLYPH_CONTENT_PX_RATIO: f64 = 0.46;
/// The press preview's glyph, as a fraction of the bubble's height.
const PREVIEW_GLYPH_CONTENT_PX_RATIO: f64 = 0.5;
/// A long-press option's glyph, as a fraction of its cell's height.
const OPTION_GLYPH_CONTENT_PX_RATIO: f64 = 0.5;
/// Gutter between neighbouring keys, as a fraction of a row's height.
const KEY_GAP_RATIO: f64 = 0.06;

pub(crate) struct KeyboardGeometry {
    pub content_rect: Rect,
    /// The toolbar band.
    pub toolbar_area: Rect,
    pub toolbar: Vec<(usize, Rect)>,
    pub suggestions: Vec<(usize, Rect)>,
    pub suggestion_area: Rect,
    /// The band the keys fill: where a touch belongs to the keys.
    pub key_area: Rect,
    /// The rows the keys are laid out in: the key band less one key gap above
    /// and below, so the first and last rows keep the same distance from the
    /// band edges as every row keeps from the next.
    pub key_rows: Rect,
    pub keys: Vec<(KeyId, Rect)>,
}
pub(crate) fn keyboard_geometry(
    rect: Rect,
    layout: &KeyboardLayout,
    toolbar: &[OskToolbarItem],
    suggestions: &[Suggestion],
    shift: ShiftState,
    toolbar_height: f32,
    suggestion_height: f32,
    gutter: f64,
) -> KeyboardGeometry {
    // Every band shares one width: the window less a side gutter, so no key,
    // chip or button touches the window edge.
    let width = (rect.width() - 2.0 * gutter).clamp(0.0, MAX_KEYBOARD_CONTENT_WIDTH);
    let x = rect.center().x - width * 0.5;
    let content_rect = Rect::new(x, rect.y0, x + width, rect.y1);
    let h = content_rect.height();
    let toolbar_h = f64::from(toolbar_height).min(h);
    let suggestion_h = f64::from(suggestion_height).min((h - toolbar_h).max(0.0));
    let toolbar_y1 = content_rect.y0 + toolbar_h;
    let suggestion_y1 = toolbar_y1 + suggestion_h;
    let toolbar_area = Rect::new(x, rect.y0, x + width, toolbar_y1);
    let suggestion_area = Rect::new(x, toolbar_y1, x + width, suggestion_y1);
    let key_area = Rect::new(x, suggestion_y1, x + width, rect.y1);
    let rows = layout.row_count().max(1) as f64;
    let key_pad = key_pad_for_height(key_area.height() / rows);
    let key_rows = Rect::new(x, key_area.y0 + key_pad, x + width, key_area.y1 - key_pad);
    let key_rows = if key_rows.height() > 0.0 {
        key_rows
    } else {
        key_area
    };
    // As many chips as the band holds at a readable width (a slot is at
    // least three band heights wide): a narrow window shows fewer chips
    // instead of squeezing every word into an elided stub.
    let slots = (suggestion_area.width() / (suggestion_area.height() * 3.0))
        .floor()
        .max(1.0) as usize;
    let shown = suggestions.get(..slots).unwrap_or(suggestions);
    KeyboardGeometry {
        content_rect,
        toolbar_area,
        toolbar: toolbar_rects(toolbar, toolbar_area),
        suggestions: suggestion_rects(shown, suggestion_area),
        suggestion_area,
        key_area,
        key_rows,
        keys: key_rects(layout, key_rows, shift),
    }
}

pub(crate) struct KeyboardPresentation<'a> {
    pub rect: Rect,
    pub layout: &'a KeyboardLayout,
    pub toolbar: &'a [OskToolbarItem],
    pub suggestions: &'a [Suggestion],
    pub toolbar_geom: &'a [(usize, Rect)],
    pub suggestion_geom: &'a [(usize, Rect)],
    pub key_geom: &'a [(KeyId, Rect)],
    pub suggestion_area: Rect,
    pub shift: ShiftState,
    pub hovered_key: Option<KeyId>,
    pub pressed_key: Option<KeyId>,
    /// Keys held down by a finger that has not typed yet.
    pub finger_keys: &'a [KeyId],
    pub hovered_tool: Option<usize>,
    pub pressed_tool: Option<usize>,
    pub hovered_sugg: Option<usize>,
    pub pressed_sugg: Option<usize>,
    pub empty_hint: &'a str,
    /// The emoji panel puts its search field over the suggestion bar.
    pub emoji_panel_open: bool,
    /// The character key a finger or the pointer holds down, previewed in a
    /// bubble above it. `None` in secret fields.
    pub preview: Option<KeyId>,
    /// The long-press alternates row, when one is open.
    pub alternates: Option<&'a AlternatesPopup>,
    /// A drag on the space bar is moving the caret.
    pub cursor_mode: bool,
    /// The debug overlay of effective hit areas (empty when off).
    pub hit_cells: &'a [HitCell],
}

/// The accented variants offered by a long press on a letter, with the one
/// under the finger.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AlternatesPopup {
    pub options: Vec<String>,
    pub selected: usize,
    /// Where each option is drawn, parallel to `options`.
    pub cells: Vec<Rect>,
}

impl AlternatesPopup {
    pub fn new(key_rect: Rect, options: Vec<String>, bounds: Rect) -> Self {
        let cells = alternates_cells(key_rect, options.len(), bounds);
        Self {
            options,
            selected: 0,
            cells,
        }
    }

    /// Select the option nearest `x`. Only the horizontal position counts, so
    /// a finger drifting up or down keeps its choice.
    pub fn track(&mut self, x: f64) {
        let nearest = self
            .cells
            .iter()
            .enumerate()
            .min_by(|a, b| {
                let da = (a.1.center().x - x).abs();
                let db = (b.1.center().x - x).abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i);
        if let Some(i) = nearest {
            self.selected = i;
        }
    }

    pub fn chosen(&self) -> Option<&str> {
        self.options.get(self.selected).map(String::as_str)
    }
}

/// Key-sized cells in one row just above `key_rect`, parallel to the options.
fn alternates_cells(key_rect: Rect, n: usize, bounds: Rect) -> Vec<Rect> {
    let w = key_rect.width();
    let h = key_rect.height();
    let y0 = (key_rect.y0 - h).max(bounds.y0);
    // The first option (the plain letter) sits right above the finger, so a
    // press released without sliding, or with a small wobble, keeps it. The
    // rest run towards the side with room: rightwards, or leftwards near the
    // right edge, the way a thumb naturally slides away from the edge.
    let span = w * n.saturating_sub(1) as f64;
    let room_right = bounds.x1 - key_rect.x1;
    let room_left = key_rect.x0 - bounds.x0;
    let step = if room_right >= span || room_right >= room_left {
        w
    } else {
        -w
    };
    // When the row cannot fit either way, slide it back inside the bounds.
    let (lo, hi) = if step > 0.0 {
        (key_rect.x0, key_rect.x0 + span + w)
    } else {
        (key_rect.x0 - span, key_rect.x1)
    };
    let shift = if hi > bounds.x1 {
        bounds.x1 - hi
    } else if lo < bounds.x0 {
        bounds.x0 - lo
    } else {
        0.0
    };
    (0..n)
        .map(|i| {
            let x = key_rect.x0 + shift + step * i as f64;
            Rect::new(x, y0, x + w, y0 + h)
        })
        .collect()
}

/// Where the press preview bubble for `key_rect` sits: wider and taller than
/// the key, floating above it, clamped inside `bounds`.
pub(crate) fn preview_rect(key_rect: Rect, bounds: Rect) -> Rect {
    let pad = key_pad(key_rect);
    let cell = key_rect.inset(-pad);
    let w = (cell.width() * 1.5).max(cell.width() + 16.0);
    let h = cell.height() * 1.25;
    let x0 = (cell.center().x - w * 0.5).clamp(bounds.x0, (bounds.x1 - w).max(bounds.x0));
    let y0 = (cell.y0 - h - pad).max(bounds.y0);
    Rect::new(x0, y0, x0 + w, y0 + h)
}
impl KeyboardPresentation<'_> {
    #[allow(clippy::too_many_lines, reason = "preserved normal paint order")]
    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // No ground of its own: the keyboard stands on the window's.
        let ground = theme.tokens.colors.surfaces.content;
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);

        // Toolbar.
        for (i, r) in self.toolbar_geom {
            // `toolbar_geom` is built from `self.toolbar` each frame, so `*i`
            // is always in range; `get` avoids a panic if the geometry is
            // ever stale for one frame.
            let Some(item) = self.toolbar.get(*i) else {
                continue;
            };
            draw_toolbar_item(
                &mut ctx,
                *r,
                item,
                self.hovered_tool == Some(*i),
                self.pressed_tool == Some(*i),
            );
        }

        // The bands are told apart by one hairline each, never by a fill.
        if self.suggestion_area.area() > 0.0 {
            let (x0, x1) = (self.rect.x0, self.rect.x1);
            for y in [self.suggestion_area.y0, self.suggestion_area.y1] {
                ui::surface::separator(ctx.scene, ctx.transform, x0, x1, y, 0.0, ground);
            }
        }

        // Suggestion bar: a tappable chip per suggestion.
        if self.suggestion_area.area() > 0.0 && !self.emoji_panel_open {
            for (i, r) in self.suggestion_geom {
                let Some(s) = self.suggestions.get(*i) else {
                    continue;
                };
                let hovered = self.hovered_sugg == Some(*i) || self.pressed_sugg == Some(*i);
                // The top chip is the highlighted (auto / best) pick.
                draw_suggestion_chip(&mut ctx, *r, &s.word, *i == 0, hovered);
            }
            if self.suggestion_geom.is_empty() {
                let hint = self.empty_hint;
                let size = ctx.measure_role(hint, TextRole::Caption, None);
                let origin = Point::new(
                    self.suggestion_area.center().x - size.width * 0.5,
                    self.suggestion_area.center().y - size.height * 0.5,
                );
                let ink = theme.tokens.colors.text_dim;
                ctx.text_role(hint, TextRole::Caption, ink, origin, None);
            }
        }

        // Keys.
        let shift = self.shift;
        let layout = self.layout;
        for (id, rect) in self.key_geom {
            let Some(key) = layout.key(*id) else { continue };
            let label = key.display_label(shift);
            let hovered = self.hovered_key == Some(*id);
            let pressed = self.pressed_key == Some(*id) || self.finger_keys.contains(id);
            let tone = match &key.action {
                KeyAction::Char(_) | KeyAction::Space => CellTone::Plain,
                KeyAction::Enter => CellTone::Primary,
                KeyAction::Shift if shift.is_upper() => CellTone::Active,
                _ => CellTone::Modifier,
            };
            // The Shift key's icon fills solid while Caps-Lock is latched, so a
            // sticky upper-case reads differently from a one-shot shift.
            let icon_filled =
                matches!(&key.action, KeyAction::Shift) && shift == ShiftState::CapsLock;
            let is_space = matches!(&key.action, KeyAction::Space);
            // While the space bar steers the caret the letters fade and the
            // space bar shows which way it moves.
            let dim_label = self.cursor_mode && Key::is_letter(key);
            let shown = if is_space && self.cursor_mode {
                ""
            } else {
                label.as_str()
            };
            draw_cell(
                &mut ctx,
                *rect,
                CellFace::new(key.icon, shown),
                CellState {
                    hovered,
                    pressed,
                    tone,
                    icon_filled,
                    dim_label,
                },
            );
            if is_space && self.cursor_mode {
                draw_cursor_chevrons(&mut ctx, *rect);
            }
        }

        if !self.hit_cells.is_empty() {
            draw_hit_overlay(
                ctx.scene,
                ctx.transform,
                ctx.theme,
                self.hit_cells,
                self.key_geom,
                layout,
            );
        }

        // Floating feedback goes last so it overlaps the rows above.
        if let Some(popup) = self.alternates {
            draw_alternates(&mut ctx, popup);
        } else if let Some(id) = self.preview
            && let Some((_, rect)) = self.key_geom.iter().find(|(k, _)| *k == id)
            && let Some(key) = layout.key(id)
        {
            let label = key.display_label(shift);
            let bubble = preview_rect(*rect, self.rect);
            draw_key_preview(&mut ctx, bubble, &label);
        }
    }
}

/// Gutter between neighbouring keys: each cell is inset by this on every side.
pub(crate) fn key_pad(rect: Rect) -> f64 {
    key_pad_for_height(rect.height())
}

/// [`key_pad`] for a row `height` tall.
pub(crate) fn key_pad_for_height(height: f64) -> f64 {
    (height * KEY_GAP_RATIO).clamp(2.0, 6.0)
}

/// The face of a key at rest, hovered or pressed, standing on the window
/// ground. A character key is a floor lifted off the ground; a modifier
/// stays on the ground itself until touched, so the letters lead.
fn key_fill(theme: &Theme, tone: CellTone, hovered: bool, pressed: bool) -> Color {
    let c = &theme.tokens.colors;
    let ground = c.surfaces.content;
    match tone {
        CellTone::Active | CellTone::Primary => {
            if pressed || hovered {
                c.intent.inverted_hover
            } else {
                c.intent.inverted
            }
        }
        CellTone::Plain => {
            selection_floor_ramp(theme, SelectionState::new(false, hovered, pressed), ground)
        }
        CellTone::Modifier | CellTone::Flat => {
            if pressed {
                c.lift_on(ground, 1.0)
            } else if hovered {
                c.lift_on(ground, 0.5)
            } else {
                ground
            }
        }
    }
}

/// A floating panel for key feedback: the floating material, the panel edge
/// and the one floating shadow. Returns the panel's radius.
fn draw_float_panel(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) -> f64 {
    let radius = f64::from(theme.tokens.radii.panel);
    ui::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );
    ui::surface::panel(scene, transform, rect, radius, theme);
    radius
}

/// How far above its baseline the middle of a glyph's body sits, in em.
/// Lower-case letters, punctuation and marks ride the x-height body, so a
/// comma, a full stop and a letter on neighbouring keys share one baseline;
/// capitals and digits are centred on their cap height.
fn body_middle(ch: char) -> f64 {
    const X_HEIGHT: f64 = 0.51;
    const CAP_HEIGHT: f64 = 0.72;
    if ch.is_uppercase() || ch.is_numeric() {
        CAP_HEIGHT * 0.5
    } else {
        X_HEIGHT * 0.5
    }
}

/// Marks that are a dot or a tick in any face: drawn larger and heavier than
/// a letter, so a key that carries only one still reads as that mark.
const SMALL_MARKS: &str = ".,:;'\"-_*~^`";
/// How much larger a small mark is drawn than a letter on the same key.
const SMALL_MARK_SCALE: f32 = 1.35;

/// Paint one character centred on `rect`: horizontally on its advance,
/// vertically on the body of its kind (see [`body_middle`]). A small mark is
/// drawn larger but stays on the baseline the letters share.
fn draw_glyph(ctx: &mut PaintCtx<'_>, rect: Rect, ch: &str, content_px: f32, ink: Color) {
    let first = ch.chars().next().unwrap_or(' ');
    let small = ch.chars().count() == 1 && SMALL_MARKS.contains(first);
    let (px, weight) = if small {
        (content_px * SMALL_MARK_SCALE, TextWeight::Semibold)
    } else {
        (content_px, TextWeight::Medium)
    };
    let style = TextStyle::content(px).with_weight(weight);
    let (advance, _) = ctx.text.measure_style(ch, None, &style);
    let baseline = ctx.text.first_baseline_style(ch, None, &style);
    // The baseline is where the letter size puts it, not the mark's own.
    let middle = body_middle(first) * f64::from(content_px);
    let origin = Point::new(
        rect.center().x - f64::from(advance) * 0.5,
        rect.center().y + middle - f64::from(baseline),
    );
    ctx.text(ch, &style, ink, origin, None);
}

/// Paint a word label (`?123`, `space`) centred on `rect` in `Label`.
///
/// A word wider than the key is elided, never left to spill over its edges.
fn draw_word(ctx: &mut PaintCtx<'_>, rect: Rect, word: &str, ink: Color) {
    let room = (rect.width() - 2.0 * key_pad(rect)).max(0.0) as f32;
    let style = ctx.theme.text(TextRole::Label);
    let fitted = ctx.text.elide_to_width_style(word, room, &style);
    let word = fitted.as_str();
    let size = ctx.measure_role(word, TextRole::Label, None);
    let origin = Point::new(
        rect.center().x - size.width * 0.5,
        rect.center().y - size.height * 0.5,
    );
    ctx.text_role(word, TextRole::Label, ink, origin, None);
}

/// A label that is one character reads at the key's glyph size; a word sits
/// in the label role.
fn draw_label(ctx: &mut PaintCtx<'_>, rect: Rect, label: &str, ratio: f64, ink: Color) {
    if label.chars().count() == 1 {
        let label_px = ctx.theme.text(TextRole::Label).size;
        let px = ((rect.height() * ratio) as f32).max(label_px);
        draw_glyph(ctx, rect, label, px, ink);
    } else {
        draw_word(ctx, rect, label, ink);
    }
}

/// The press preview: the held key's glyph, enlarged in a floating bubble
/// above the finger, so the letter stays visible while the thumb covers the
/// key itself.
fn draw_key_preview(ctx: &mut PaintCtx<'_>, bubble: Rect, label: &str) {
    if label.is_empty() {
        return;
    }
    draw_float_panel(ctx.scene, ctx.transform, ctx.theme, bubble);
    let ink = ctx.theme.tokens.colors.text_primary;
    draw_label(ctx, bubble, label, PREVIEW_GLYPH_CONTENT_PX_RATIO, ink);
}

/// The long-press row of accented variants; the one under the finger is the
/// armed row of a popover, inverted.
fn draw_alternates(ctx: &mut PaintCtx<'_>, popup: &AlternatesPopup) {
    let (Some(first), Some(last)) = (popup.cells.first(), popup.cells.last()) else {
        return;
    };
    let panel_radius = draw_float_panel(ctx.scene, ctx.transform, ctx.theme, first.union(*last));
    let colors = ctx.theme.tokens.colors.clone();
    for (i, (cell, option)) in popup.cells.iter().zip(&popup.options).enumerate() {
        let selected = i == popup.selected;
        let pad = key_pad(*cell);
        let inner = cell.inset(-pad);
        let ink = if selected {
            let radius = f64::from(
                ctx.theme
                    .tokens
                    .radii
                    .concentric(panel_radius as f32, pad as f32),
            );
            ctx.fill_round(inner, radius, colors.intent.inverted);
            colors.intent.on_inverted
        } else {
            colors.text_primary
        };
        draw_label(ctx, inner, option, OPTION_GLYPH_CONTENT_PX_RATIO, ink);
    }
}

/// Left and right chevrons on the space bar while it moves the caret.
fn draw_cursor_chevrons(ctx: &mut PaintCtx<'_>, rect: Rect) {
    let cell = rect.inset(-key_pad(rect));
    let s = f64::from(ctx.theme.tokens.metrics.icon_md);
    let y = cell.center().y - s / 2.0;
    let gap = s * 0.75;
    let ink = ctx.theme.tokens.colors.text_primary;
    for (icon, x) in [
        (Icon::ChevronLeft, cell.center().x - gap - s),
        (Icon::ChevronRight, cell.center().x + gap),
    ] {
        icons::draw_icon(
            ctx.scene,
            ctx.transform,
            Rect::new(x, y, x + s, y + s),
            icon,
            ink,
            icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }
}

/// Debug overlay: tints each sampled cell by the key a tap there resolves
/// to, strongly where that differs from the drawn key, and outlines every
/// letter's core zone (inside which the drawn key always wins).
fn draw_hit_overlay(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    cells: &[HitCell],
    key_geom: &[(KeyId, Rect)],
    layout: &KeyboardLayout,
) {
    let s = &theme.tokens.colors.semantic;
    // Four colours so no two neighbouring keys share one: along a row the
    // index steps by two, between rows by one.
    let palette = [s.info, s.success, s.warning, s.danger];
    let colour = |id: KeyId| {
        palette
            .get((id.row + 2 * id.col) % palette.len())
            .copied()
            .unwrap_or(s.info)
    };
    for cell in cells {
        let moved = cell.drawn != cell.key;
        let alpha = if moved { 0.8 } else { 0.3 };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colour(cell.key).with_alpha(alpha),
            None,
            &cell.rect,
        );
    }
    for (id, rect) in key_geom {
        if layout.key(*id).is_some_and(Key::is_letter) {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.text_primary,
                None,
                &core_zone(*rect, CORE_FRACTION),
            );
        }
    }
}

/// Interaction/visual flags for one drawn cell, bundled so [`draw_cell`]
/// doesn't take separate `bool` parameters.
#[derive(Clone, Copy, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "this struct IS the bool-bundling fix for draw_cell's argument count; \
              its fields are independent interaction/visual flags for one \
              cell, and splitting them into sub-structs would not express any \
              real invariant between them"
)]
pub(crate) struct CellState {
    pub(crate) hovered: bool,
    pub(crate) pressed: bool,
    pub(crate) tone: CellTone,
    /// Solid (vs. outline) icon glyph — used for a latched state like
    /// Caps-Lock's Shift key.
    pub(crate) icon_filled: bool,
    /// A quieter label: the letters while the space bar steers the caret.
    pub(crate) dim_label: bool,
}

/// What a cell is, which decides its fill and ink.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CellTone {
    /// A character key: the raised key surface.
    #[default]
    Plain,
    /// Shift, Backspace, layer and emoji keys.
    Modifier,
    /// A latched state (Shift or Caps-Lock on): the inverted fill, like a
    /// switch that is on.
    Active,
    /// The primary action (Return): the inverted fill.
    Primary,
    /// A bare cell with no key surface of its own.
    Flat,
}

/// What a cell shows: an icon glyph, or a text label.
#[derive(Clone, Copy)]
pub(crate) enum CellFace<'a> {
    Icon(Icon),
    Label { text: &'a str },
}

impl<'a> CellFace<'a> {
    /// The icon when there is one, else the label.
    pub(crate) fn new(icon: Option<Icon>, text: &'a str) -> Self {
        icon.map_or(Self::Label { text }, Self::Icon)
    }
}

/// Draw one key: an edged rounded cell with its icon or label.
pub(crate) fn draw_cell(ctx: &mut PaintCtx<'_>, rect: Rect, face: CellFace<'_>, state: CellState) {
    let CellState {
        hovered,
        pressed,
        tone,
        icon_filled,
        dim_label,
    } = state;
    let theme = ctx.theme;
    let c = &theme.tokens.colors;
    // Inset so adjacent cells have a visible gutter.
    let cell = rect.inset(-key_pad(rect));
    let radius = f64::from(theme.tokens.radii.control);
    if tone != CellTone::Flat {
        ui::surface::card(
            ctx.scene,
            ctx.transform,
            cell,
            radius,
            key_fill(theme, tone, hovered, pressed),
        );
    }

    let fg = match tone {
        CellTone::Active | CellTone::Primary => c.intent.on_inverted,
        _ if dim_label => c.text_dim,
        _ => c.text_primary,
    };
    match face {
        CellFace::Label { text } => {
            if !text.is_empty() {
                draw_label(ctx, cell, text, KEY_GLYPH_CONTENT_PX_RATIO, fg);
            }
        }
        CellFace::Icon(icon) => {
            let s = f64::from(theme.tokens.metrics.icon_md);
            let icon_rect = Rect::from_center_size(cell.center(), (s, s));
            if icon_filled {
                // A latched state (e.g. Caps-Lock's Shift key) reads as a solid
                // glyph, distinct from the outline of the momentary state.
                icon.draw_filled(ctx.scene, ctx.transform, icon_rect, fg);
            } else {
                icons::draw_icon(
                    ctx.scene,
                    ctx.transform,
                    icon_rect,
                    icon,
                    fg,
                    icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
    }
}

/// One toolbar item: an icon-only plain button centred in its cell. The cell
/// is the hit target; the face is the control's own size.
fn draw_toolbar_item(
    ctx: &mut PaintCtx<'_>,
    cell: Rect,
    item: &OskToolbarItem,
    hovered: bool,
    pressed: bool,
) {
    let control = ui::widget::ControlSize::Regular;
    let square = ButtonFace::height(control, ctx.theme);
    let rect = Rect::from_center_size(cell.center(), (square, square));
    let face = match item.icon {
        Some(icon) => ButtonFace::icon_only(rect, icon, &item.label),
        None => ButtonFace::new(rect, &item.label).role(ButtonRole::Plain),
    };
    let face = face.control_size(control).hovered(hovered).pressed(pressed);
    Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
}

/// Draw one suggestion chip. The best (auto-inserted) pick is the lifted
/// chip; the rest are quiet.
fn draw_suggestion_chip(ctx: &mut PaintCtx<'_>, rect: Rect, word: &str, top: bool, hovered: bool) {
    let gap = f64::from(ctx.theme.tokens.spacing.s2);
    let height = ChipFace::height(ctx.theme);
    // As wide as the word needs, centred in its slot, so the label never
    // sits against one end of an evenly split band.
    let room = (rect.width() - 2.0 * gap).max(0.0);
    let natural = ChipFace::intrinsic_width(word, ChipLead::None, false, ctx.text, ctx.theme);
    let chip = Rect::from_center_size(rect.center(), (natural.min(room), height));
    // The auto pick is the chip in the lifted tier, like a selected segment:
    // an emphasis that carries no colour.
    ChipFace::new(chip, word)
        .selected(top)
        .hovered(hovered)
        .paint(ctx.scene, ctx.transform, ctx.text, ctx.theme);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolbar::OskToolbarItem;

    #[test]
    fn every_band_shares_one_width_inset_by_the_gutter() {
        let layout = KeyboardLayout::qwerty();
        let toolbar = OskToolbarItem::default_set();
        let window = Rect::new(0.0, 0.0, 390.0, 312.0);
        let gutter = 8.0;
        let g = keyboard_geometry(
            window,
            &layout,
            &toolbar,
            &[],
            ShiftState::Off,
            48.0,
            40.0,
            gutter,
        );
        for band in [g.toolbar_area, g.suggestion_area, g.key_area] {
            assert_eq!(band.x0, gutter, "band starts at the gutter");
            assert_eq!(band.x1, 390.0 - gutter, "band ends at the gutter");
        }
        assert!(g.keys.iter().all(|(_, r)| r.x0 >= gutter - 1e-6));
    }

    #[test]
    fn the_gutter_never_collapses_a_narrow_window_to_nothing() {
        let layout = KeyboardLayout::qwerty();
        let g = keyboard_geometry(
            Rect::new(0.0, 0.0, 10.0, 312.0),
            &layout,
            &[],
            &[],
            ShiftState::Off,
            48.0,
            40.0,
            8.0,
        );
        assert!(g.content_rect.width() >= 0.0);
    }

    #[test]
    fn letters_punctuation_and_marks_share_one_baseline_and_capitals_centre_on_their_height() {
        for c in [
            'a', 'g', 'j', 'p', 'q', 'y', 'b', 'é', '.', ',', '-', '\'', '$',
        ] {
            assert_eq!(
                body_middle(c),
                body_middle('x'),
                "{c} rides the shared body"
            );
        }
        for c in ['H', 'Q', '7'] {
            assert!(
                body_middle(c) > body_middle('x'),
                "{c} is centred on its cap height"
            );
        }
    }

    #[test]
    fn the_key_rows_keep_one_key_gap_from_the_band_edges() {
        let layout = KeyboardLayout::qwerty();
        let g = keyboard_geometry(
            Rect::new(0.0, 0.0, 720.0, 360.0),
            &layout,
            &[],
            &[],
            ShiftState::Off,
            48.0,
            40.0,
            8.0,
        );
        let pad = key_pad_for_height(g.key_area.height() / layout.row_count() as f64);
        assert!((g.key_rows.y0 - g.key_area.y0 - pad).abs() < 1e-6);
        assert!((g.key_area.y1 - g.key_rows.y1 - pad).abs() < 1e-6);
    }

    #[test]
    fn a_narrow_band_shows_fewer_chips_so_none_is_squeezed() {
        let layout = KeyboardLayout::qwerty();
        let words: Vec<Suggestion> = ["help", "held", "hell", "helped", "helping"]
            .iter()
            .map(|w| Suggestion::new(*w, 1.0))
            .collect();
        let chips = |width: f64| {
            keyboard_geometry(
                Rect::new(0.0, 0.0, width, 312.0),
                &layout,
                &[],
                &words,
                ShiftState::Off,
                48.0,
                40.0,
                8.0,
            )
            .suggestions
            .len()
        };
        assert_eq!(chips(420.0), 3);
        assert_eq!(chips(720.0), 5);
    }
}
