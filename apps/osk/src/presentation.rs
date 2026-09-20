//! Immutable keyboard geometry and paint shared by the live root and History.
use crate::layout::{Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
use crate::suggest::{Suggestion, suggestion_rects};
use crate::toolbar::{OskToolbarItem, toolbar_rects};
use ui::{
    TextRenderer, Theme,
    icons::{self, Icon},
};
use vello::{
    Scene,
    kurbo::{Affine, Rect, RoundedRect},
};

pub(crate) struct KeyboardGeometry {
    pub content_rect: Rect,
    pub toolbar: Vec<(usize, Rect)>,
    pub suggestions: Vec<(usize, Rect)>,
    pub suggestion_area: Rect,
    pub keys: Vec<(KeyId, Rect)>,
}
#[allow(
    clippy::too_many_arguments,
    reason = "immutable geometry inputs mirror the live keyboard bands"
)]
pub(crate) fn keyboard_geometry(
    rect: Rect,
    layout: &KeyboardLayout,
    toolbar: &[OskToolbarItem],
    suggestions: &[Suggestion],
    shift: ShiftState,
    toolbar_height: f32,
    suggestion_height: f32,
) -> KeyboardGeometry {
    let width = rect.width().min(1120.0);
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
        toolbar: toolbar_rects(toolbar, toolbar_area),
        suggestions: suggestion_rects(suggestions, suggestion_area),
        suggestion_area,
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
    pub hovered_tool: Option<usize>,
    pub pressed_tool: Option<usize>,
    pub hovered_sugg: Option<usize>,
    pub pressed_sugg: Option<usize>,
    pub empty_hint: &'a str,
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
                scene,
                transform,
                text_renderer,
                theme,
                *r,
                item.icon,
                &item.label,
                CellState {
                    hovered,
                    pressed,
                    ..Default::default()
                },
                0.34,
            );
        }

        // Suggestion bar: a subtle band, then a tappable chip per suggestion.
        if self.suggestion_area.area() > 0.0 {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &self.suggestion_area,
            );
            for (i, r) in self.suggestion_geom {
                let Some(s) = self.suggestions.get(*i) else {
                    continue;
                };
                let hovered = self.hovered_sugg == Some(*i);
                let pressed = self.pressed_sugg == Some(*i);
                // The top chip is the highlighted (auto / best) pick.
                let top = *i == 0;
                draw_suggestion_chip(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    *r,
                    &s.word,
                    hovered,
                    pressed,
                    top,
                );
            }
            if self.suggestion_geom.is_empty() {
                let hint = self.empty_hint;
                let font = (self.suggestion_area.height() * 0.38).clamp(12.0, 18.0) as f32;
                // Centre on the measured line, as the key labels and chips
                // do: the nominal size under-reads the line at 1× and by
                // half a line at 2×, sinking the hint below the bar's centre.
                let (width, height) = text_renderer.measure(hint, font, None);
                let x = self.suggestion_area.center().x - f64::from(width) * 0.5;
                let y = self.suggestion_area.y0
                    + (self.suggestion_area.height() - f64::from(height)) * 0.5;
                text_renderer.draw_text(
                    scene,
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
            let is_action = !matches!(&key.action, KeyAction::Char(_));
            let shift_active = matches!(&key.action, KeyAction::Shift) && shift.is_upper();
            let hovered = self.hovered_key == Some(*id);
            let pressed = self.pressed_key == Some(*id);
            // Letters get a larger glyph; multi-char action labels smaller.
            let font_scale = if Key::is_letter(key) { 0.42 } else { 0.30 };
            // The Shift key's icon fills solid while Caps-Lock is latched, so a
            // sticky upper-case reads differently from a one-shot shift.
            let icon_filled =
                matches!(&key.action, KeyAction::Shift) && shift == ShiftState::CapsLock;
            draw_cell(
                scene,
                transform,
                text_renderer,
                theme,
                *rect,
                key.icon,
                &label,
                CellState {
                    hovered,
                    pressed,
                    accent: is_action || shift_active,
                    icon_filled,
                },
                font_scale,
            );
        }
    }
}

/// Interaction/visual flags for one drawn cell, bundled so [`draw_cell`]
/// doesn't take four separate `bool` parameters.
#[derive(Clone, Copy, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "this struct IS the bool-bundling fix for draw_cell's argument count; \
              its four fields are independent interaction/visual flags for one \
              cell, and splitting them into sub-structs would not express any \
              real invariant between them"
)]
struct CellState {
    hovered: bool,
    pressed: bool,
    /// Accent styling (e.g. an active toggle key).
    accent: bool,
    /// Solid (vs. outline) icon glyph — used for a latched state like
    /// Caps-Lock's Shift key.
    icon_filled: bool,
}

/// Draw a rounded "button" cell (used for both keys and toolbar items).
#[allow(
    clippy::too_many_arguments,
    reason = "Keyboard cells combine render context with bounds, optional icon, label, interaction state, and key-specific font scaling."
)]
fn draw_cell(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    icon: Option<Icon>,
    label: &str,
    state: CellState,
    font_scale: f64,
) {
    let CellState {
        hovered,
        pressed,
        accent,
        icon_filled,
    } = state;
    let t = &theme.tokens;
    // Inset so adjacent cells have a visible gutter.
    let pad = (rect.height() * 0.06).clamp(2.0, 6.0);
    let cell = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad);
    let bg = if pressed {
        theme.bg_active
    } else if hovered {
        theme.bg_hover
    } else if accent {
        t.colors.surfaces.overlay
    } else {
        t.colors.surfaces.raised
    };
    let shape = RoundedRect::from_rect(cell, f64::from(t.radii.md));
    scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
    let border = if accent {
        theme.border_focus
    } else {
        t.colors.border_standard
    };
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        border,
        None,
        &shape,
    );

    let fg = if accent {
        theme.accent_primary
    } else {
        t.colors.text_primary
    };

    if let Some(icon) = icon {
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

/// Draw one suggestion chip: a pill with the word, themed by state. The `top`
/// (best / auto-inserted) chip is accented; the rest are neutral. All colours
/// come from theme tokens.
#[allow(
    clippy::too_many_arguments,
    reason = "Suggestion chips combine render context with bounds and word plus independent hover, press, and best-suggestion states."
)]
fn draw_suggestion_chip(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    word: &str,
    hovered: bool,
    pressed: bool,
    top: bool,
) {
    let t = &theme.tokens;
    let pad_x = (rect.width() * 0.06).clamp(3.0, 10.0);
    let pad_y = (rect.height() * 0.14).clamp(2.0, 8.0);
    let chip = Rect::new(
        rect.x0 + pad_x,
        rect.y0 + pad_y,
        rect.x1 - pad_x,
        rect.y1 - pad_y,
    );
    let bg = if pressed {
        theme.bg_active
    } else if hovered {
        theme.bg_hover
    } else if top {
        // The accent fill at low alpha reads as "selected" without shouting.
        theme.tokens.colors.surfaces.overlay
    } else {
        theme.tokens.colors.surfaces.base
    };
    let shape = RoundedRect::from_rect(chip, (chip.height() * 0.45).min(f64::from(t.radii.lg)));
    scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
    if top {
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            theme.border_focus,
            None,
            &shape,
        );
    }

    let fg = if top {
        theme.accent_primary
    } else {
        t.colors.text_primary
    };
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
