//! Immutable keyboard geometry and paint shared by the live root and History.
use crate::layout::{Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
use crate::suggest::{Suggestion, suggestion_rects};
use crate::toolbar::{OskToolbarItem, toolbar_rects};
use crate::touch_model::{CORE_FRACTION, HitCell, core_zone};
use ui::tokens::colors::ON_FILL_MIN_CONTRAST;
use ui::{
    TextRenderer, Theme,
    icons::{self, Icon},
    paint::PaintCtx,
};
use vello::{
    Scene,
    kurbo::{Affine, Rect, RoundedRect},
    peniko::Color,
};

/// Widest the keyboard content grows; wider surfaces centre it.
pub(crate) const MAX_KEYBOARD_CONTENT_WIDTH: f64 = 1_120.0;

pub(crate) struct KeyboardGeometry {
    pub content_rect: Rect,
    /// The toolbar band.
    pub toolbar_area: Rect,
    pub toolbar: Vec<(usize, Rect)>,
    pub suggestions: Vec<(usize, Rect)>,
    pub suggestion_area: Rect,
    /// The band the keys fill.
    pub key_area: Rect,
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
) -> KeyboardGeometry {
    let width = rect.width().min(MAX_KEYBOARD_CONTENT_WIDTH);
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
    KeyboardGeometry {
        content_rect,
        toolbar_area,
        toolbar: toolbar_rects(toolbar, toolbar_area),
        suggestions: suggestion_rects(suggestions, suggestion_area),
        suggestion_area,
        key_area,
        keys: key_rects(layout, key_area, shift),
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
        // Surface background.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        // Toolbar.
        for (i, r) in self.toolbar_geom {
            // `toolbar_geom` is built from `self.toolbar` each frame, so `*i`
            // is always in range; `get` avoids a panic if the geometry is
            // ever stale for one frame.
            let Some(item) = self.toolbar.get(*i) else {
                continue;
            };
            let hovered = self.hovered_tool == Some(*i);
            let pressed = self.pressed_tool == Some(*i);
            draw_cell(
                &mut ctx,
                *r,
                CellFace::new(item.icon, &item.label, 0.34),
                CellState {
                    hovered,
                    pressed,
                    tone: CellTone::Flat,
                    ..Default::default()
                },
            );
        }

        // Suggestion bar: a subtle band, then a tappable chip per suggestion.
        if self.suggestion_area.area() > 0.0 && !self.emoji_panel_open {
            ctx.fill_rect(self.suggestion_area, theme.tokens.colors.surfaces.raised);
            for (i, r) in self.suggestion_geom {
                let Some(s) = self.suggestions.get(*i) else {
                    continue;
                };
                let hovered = self.hovered_sugg == Some(*i);
                let pressed = self.pressed_sugg == Some(*i);
                // The top chip is the highlighted (auto / best) pick.
                let top = *i == 0;
                draw_suggestion_chip(
                    &mut ctx,
                    *r,
                    &s.word,
                    CellState {
                        hovered,
                        pressed,
                        tone: if top {
                            CellTone::Primary
                        } else {
                            CellTone::Plain
                        },
                        ..Default::default()
                    },
                );
            }
            if self.suggestion_geom.is_empty() {
                let hint = self.empty_hint;
                let font = (self.suggestion_area.height() * 0.38).clamp(12.0, 18.0) as f32;
                // Centre on the measured line, as the key labels and chips
                // do: the nominal size under-reads the line at 1× and by
                // half a line at 2×, sinking the hint below the bar's centre.
                let (width, height) = ctx.text.measure(hint, font, None);
                let x = self.suggestion_area.center().x - f64::from(width) * 0.5;
                let y = self.suggestion_area.y0
                    + (self.suggestion_area.height() - f64::from(height)) * 0.5;
                ctx.text.draw_text(
                    ctx.scene,
                    hint,
                    font,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((x, y)),
                    None,
                );
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
            // One glyph (a letter, digit or punctuation mark) reads at the
            // letter size; word labels ("?123", "space") sit smaller.
            let font_scale = if label.chars().count() == 1 {
                0.42
            } else {
                0.30
            };
            // The Shift key's icon fills solid while Caps-Lock is latched, so a
            // sticky upper-case reads differently from a one-shot shift.
            let icon_filled =
                matches!(&key.action, KeyAction::Shift) && shift == ShiftState::CapsLock;
            let is_space = matches!(&key.action, KeyAction::Space);
            // While the space bar steers the caret the letters fade and the
            // space bar shows which way it moves.
            let dim_label = is_space || (self.cursor_mode && Key::is_letter(key));
            let shown = if is_space && self.cursor_mode {
                ""
            } else {
                label.as_str()
            };
            draw_cell(
                &mut ctx,
                *rect,
                CellFace::new(key.icon, shown, font_scale),
                CellState {
                    hovered,
                    pressed,
                    tone,
                    icon_filled,
                    dim_label,
                },
            );
            if is_space && self.cursor_mode {
                draw_cursor_chevrons(ctx.scene, ctx.transform, ctx.theme, *rect);
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
            draw_alternates(ctx.scene, ctx.transform, ctx.text, ctx.theme, popup);
        } else if let Some(id) = self.preview
            && let Some((_, rect)) = self.key_geom.iter().find(|(k, _)| *k == id)
            && let Some(key) = layout.key(id)
        {
            let label = key.display_label(shift);
            let bubble = preview_rect(*rect, self.rect);
            draw_key_preview(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                bubble,
                &label,
            );
        }
    }
}

/// Gutter between neighbouring keys: each cell is inset by this on every side.
fn key_pad(rect: Rect) -> f64 {
    (rect.height() * 0.06).clamp(2.0, 6.0)
}

/// `veneer` (a translucent state token) as it lands on opaque `ground`.
fn composite(ground: Color, veneer: Color) -> Color {
    let alpha = veneer.components[3];
    ui::tokens::colors::over(ground, veneer.with_alpha(1.0), alpha)
}

/// A key's fill at rest. A modifier is the key surface under the medium
/// veneer, one step quieter than a letter in either theme polarity, so the
/// letters lead; a latched or primary key carries the accent.
fn resting_fill(theme: &Theme, tone: CellTone) -> Option<Color> {
    let c = &theme.tokens.colors;
    match tone {
        CellTone::Plain => Some(c.surfaces.raised),
        CellTone::Modifier => Some(composite(c.surfaces.raised, c.overlay_medium)),
        CellTone::Active | CellTone::Primary => Some(theme.accent_primary),
        CellTone::Flat => None,
    }
}

/// A raised, shadowed rounded panel for floating feedback.
fn draw_float_panel(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    let t = &theme.tokens;
    let radius = f64::from(t.radii.lg);
    ui::surface::shadow(scene, transform, rect, radius, &t.elevation.e2);
    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        t.colors.surfaces.raised,
        None,
        &shape,
    );
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        t.colors.border_standard,
        None,
        &shape,
    );
}

/// Centre `label` in `rect` at `font` px.
fn draw_centred(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    rect: Rect,
    label: &str,
    font: f32,
    ink: Color,
) {
    // Centre on the measured line, not the nominal size, so the glyph stays
    // centred at every accessibility text scale.
    let (tw, th) = text_renderer.measure(label, font, None);
    let tx = rect.center().x - f64::from(tw) / 2.0;
    let ty = rect.y0 + (rect.height() - f64::from(th)) / 2.0;
    text_renderer.draw_text(
        scene,
        label,
        font,
        ink,
        transform * Affine::translate((tx, ty)),
        None,
    );
}

/// The press preview: the held key's glyph, enlarged in a raised bubble
/// above the finger, so the letter stays visible while the thumb covers the
/// key itself.
fn draw_key_preview(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    bubble: Rect,
    label: &str,
) {
    if label.is_empty() {
        return;
    }
    draw_float_panel(scene, transform, theme, bubble);
    let font = (bubble.height() * 0.5).clamp(18.0, 44.0) as f32;
    draw_centred(
        scene,
        transform,
        text_renderer,
        bubble,
        label,
        font,
        theme.tokens.colors.text_primary,
    );
}

/// The long-press row of accented variants, the selected one accent-filled.
fn draw_alternates(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    popup: &AlternatesPopup,
) {
    let (Some(first), Some(last)) = (popup.cells.first(), popup.cells.last()) else {
        return;
    };
    let t = &theme.tokens;
    draw_float_panel(scene, transform, theme, first.union(*last));
    let on_accent = t.colors.on_accent(ON_FILL_MIN_CONTRAST);
    for (i, (cell, option)) in popup.cells.iter().zip(&popup.options).enumerate() {
        let selected = i == popup.selected;
        let inner = cell.inset(-key_pad(*cell));
        if selected {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.accent_primary,
                None,
                &RoundedRect::from_rect(inner, f64::from(t.radii.md)),
            );
        }
        let font = (inner.height() * 0.5).clamp(14.0, 30.0) as f32;
        let ink = if selected {
            on_accent
        } else {
            t.colors.text_primary
        };
        draw_centred(scene, transform, text_renderer, inner, option, font, ink);
    }
}

/// Left and right chevrons on the space bar while it moves the caret.
fn draw_cursor_chevrons(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    let cell = rect.inset(-key_pad(rect));
    let s = (cell.height() * 0.45).clamp(14.0, 24.0);
    let y = cell.center().y - s / 2.0;
    let gap = s * 0.75;
    for (icon, x) in [
        (Icon::ChevronLeft, cell.center().x - gap - s),
        (Icon::ChevronRight, cell.center().x + gap),
    ] {
        icons::draw_icon(
            scene,
            transform,
            Rect::new(x, y, x + s, y + s),
            icon,
            theme.tokens.colors.text_primary,
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
    /// A quieter label: the space bar's name, or the letters while the space
    /// bar steers the caret.
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
    /// A latched state (Shift or Caps-Lock on).
    Active,
    /// The primary action (Return).
    Primary,
    /// Toolbar items: no chrome until hovered or pressed.
    Flat,
}

/// What a cell shows: an icon glyph, or a text label sized as a fraction of
/// the cell's height.
#[derive(Clone, Copy)]
pub(crate) enum CellFace<'a> {
    Icon(Icon),
    Label { text: &'a str, scale: f64 },
}

impl<'a> CellFace<'a> {
    /// The icon when there is one, else the label.
    pub(crate) fn new(icon: Option<Icon>, text: &'a str, scale: f64) -> Self {
        icon.map_or(Self::Label { text, scale }, Self::Icon)
    }
}

/// Draw a rounded "button" cell (used for both keys and toolbar items).
pub(crate) fn draw_cell(ctx: &mut PaintCtx<'_>, rect: Rect, face: CellFace<'_>, state: CellState) {
    let PaintCtx {
        scene,
        transform,
        text: text_renderer,
        theme,
    } = ctx.reborrow();
    let CellState {
        hovered,
        pressed,
        tone,
        icon_filled,
        dim_label,
    } = state;
    let t = &theme.tokens;
    let c = &t.colors;
    // Inset so adjacent cells have a visible gutter.
    let pad = key_pad(rect);
    let cell = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad);
    let shape = RoundedRect::from_rect(cell, f64::from(t.radii.md));
    if let Some(fill) = resting_fill(theme, tone) {
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
    }
    // State veneers over the resting fill: they darken a light key and
    // lighten a dark one, so a press reads in every theme.
    let veneer = if pressed {
        Some(c.overlay_strong)
    } else if hovered {
        Some(c.overlay_subtle)
    } else {
        None
    };
    if let Some(v) = veneer {
        scene.fill(vello::peniko::Fill::NonZero, transform, v, None, &shape);
    }
    if tone != CellTone::Flat {
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &shape,
        );
    }

    let fg = match tone {
        CellTone::Active | CellTone::Primary => c.on_accent(ON_FILL_MIN_CONTRAST),
        _ if dim_label => c.text_dim,
        _ => c.text_primary,
    };

    let (label, font_scale) = match face {
        CellFace::Label { text, scale } => (text, scale),
        CellFace::Icon(icon) => {
            let s = (cell.height() * 0.5).clamp(14.0, 26.0);
            let ix = cell.center().x - s / 2.0;
            let iy = cell.center().y - s / 2.0;
            let icon_rect = Rect::new(ix, iy, ix + s, iy + s);
            if icon_filled {
                // A latched state (e.g. Caps-Lock's Shift key) reads as a solid
                // glyph, distinct from the outline of the momentary state.
                icon.draw_filled(scene, transform, icon_rect, fg);
            } else {
                icons::draw_icon(
                    scene,
                    transform,
                    icon_rect,
                    icon,
                    fg,
                    icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            return;
        }
    };

    if label.is_empty() {
        return;
    }
    let font = (cell.height() * font_scale).clamp(11.0, 30.0) as f32;
    // Centre on the line the renderer paints (accessibility scale included),
    // not on the nominal font size: at 2× the glyphs are twice as tall and a
    // size-based offset sat them below the key centre and across the cell
    // boundary.
    let (tw, th) = text_renderer.measure(label, font, None);
    let tx = cell.center().x - f64::from(tw) / 2.0;
    let ty = cell.y0 + (cell.height() - f64::from(th)) / 2.0;
    text_renderer.draw_text(
        scene,
        label,
        font,
        fg,
        transform * Affine::translate((tx, ty)),
        None,
    );
}

/// Draw one suggestion chip: a pill with the word, themed by state. The
/// best (auto-inserted) chip, the one in the [`CellTone::Primary`] tone, is
/// highlighted; the rest are neutral. All colours come from theme tokens.
fn draw_suggestion_chip(ctx: &mut PaintCtx<'_>, rect: Rect, word: &str, state: CellState) {
    let PaintCtx {
        scene,
        transform,
        text: text_renderer,
        theme,
    } = ctx.reborrow();
    let CellState {
        hovered,
        pressed,
        tone,
        ..
    } = state;
    let top = tone == CellTone::Primary;
    let t = &theme.tokens;
    let pad_x = (rect.width() * 0.06).clamp(3.0, 10.0);
    let pad_y = (rect.height() * 0.14).clamp(2.0, 8.0);
    let chip = Rect::new(
        rect.x0 + pad_x,
        rect.y0 + pad_y,
        rect.x1 - pad_x,
        rect.y1 - pad_y,
    );
    // The strip is painted on the raised surface; the best pick is the
    // theme's accent chip resolved against it (opaque wash, an edge that
    // clears non-text contrast, ink that clears the text floor), the rest sit
    // quietly on the base surface.
    let ground = t.colors.surfaces.raised;
    let accent = t.colors.accent_chip_on(ground, ON_FILL_MIN_CONTRAST);
    let (chip_fill, edge, fg) = if top {
        (accent.fill, Some(accent.border), accent.ink)
    } else {
        (t.colors.surfaces.base, None, t.colors.text_primary)
    };
    let shape = RoundedRect::from_rect(chip, (chip.height() * 0.45).min(f64::from(t.radii.lg)));
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        chip_fill,
        None,
        &shape,
    );
    let veneer = if pressed {
        Some(t.colors.overlay_strong)
    } else if hovered {
        Some(t.colors.overlay_subtle)
    } else {
        None
    };
    if let Some(v) = veneer {
        scene.fill(vello::peniko::Fill::NonZero, transform, v, None, &shape);
    }
    if let Some(edge) = edge {
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            edge,
            None,
            &shape,
        );
    }
    if word.is_empty() {
        return;
    }
    let font = (chip.height() * 0.5).clamp(12.0, 22.0) as f32;
    // Measured line height, so the word stays inside the pill at every scale.
    let (tw, th) = text_renderer.measure(word, font, None);
    let tx = chip.center().x - f64::from(tw) / 2.0;
    let ty = chip.y0 + (chip.height() - f64::from(th)) / 2.0;
    text_renderer.draw_text(
        scene,
        word,
        font,
        fg,
        transform * Affine::translate((tx, ty)),
        None,
    );
}
