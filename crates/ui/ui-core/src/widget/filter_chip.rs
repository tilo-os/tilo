//! One pill of a filter band and the widths a row of them shares: the search card's kind chips
//! and the autocomplete's filter row draw the same chip.

use tilo_ui_base::num::f64_of_usize;
use tilo_ui_base::text_render::{TextRenderer, TextStyle, TextWeight};
use tilo_ui_base::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

/// The narrowest a chip is drawn: a pointer target.
pub const CHIP_MIN_W_PX: f64 = tilo_ui_base::tokens::Metrics::BASE.min_pointer_target as f64;

/// Widths for one row of chips that must all fit inside `available`.
pub fn share_row(natural: &[f64], available: f64, gap: f64) -> Vec<f64> {
    if natural.is_empty() {
        return Vec::new();
    }
    let n = f64_of_usize(natural.len());
    let gaps = gap * (n - 1.0);
    let total: f64 = natural.iter().sum();
    if total + gaps <= available {
        return natural.iter().map(|w| w.max(CHIP_MIN_W_PX)).collect();
    }
    let base = ((available - gaps) / n).max(CHIP_MIN_W_PX);
    let spare: f64 = natural
        .iter()
        .filter(|w| **w < base)
        .map(|w| base - w)
        .sum();
    let over = natural.iter().filter(|w| **w >= base).count();
    let share_over = if over > 0 {
        base + spare / f64_of_usize(over)
    } else {
        base
    };
    natural
        .iter()
        .map(|w| {
            if *w < base {
                w.max(CHIP_MIN_W_PX)
            } else {
                share_over.min(*w).max(CHIP_MIN_W_PX)
            }
        })
        .collect()
}

/// One pill in the search card's filter band, shared by the kind chips and the "All"
/// reset chip.
pub fn draw_filter_chip(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    text_style: &TextStyle,
    state: tilo_ui_base::chrome::SelectionState,
) {
    let colors = &theme.tokens.colors;
    let radius = tilo_ui_base::chrome::ChromeRole::Selector.radius(theme, rect.height());
    let tier = tilo_ui_base::chrome::selection_tier(theme, state);
    tier.paint(scene, transform, rect, radius);
    let lifted = state.active || state.pressed || state.hover;
    let under = if lifted {
        tier.fill
    } else {
        tilo_ui_base::chrome::shell::panel_ground(theme)
    };
    let tone = if state.active {
        colors.text_primary
    } else {
        colors.text_dim
    };
    let text_color = colors.ink_on(
        under,
        tone,
        tilo_ui_base::tokens::colors::ON_FILL_MIN_CONTRAST,
    );
    // Control labels are medium weight (HIG §9.6); measured and drawn at the same weight
    // so the centring holds.
    let (lw, lh) = text_renderer.measure_style(
        label,
        None,
        &text_style.clone().with_weight(TextWeight::Medium),
    );
    text_renderer.draw_style(
        scene,
        label,
        text_color,
        transform.pre_translate(
            (
                rect.x0 + (rect.width() - f64::from(lw)) / 2.0,
                rect.y0 + (rect.height() - f64::from(lh)) / 2.0,
            )
                .into(),
        ),
        None,
        &text_style.clone().with_weight(TextWeight::Medium),
    );
}
