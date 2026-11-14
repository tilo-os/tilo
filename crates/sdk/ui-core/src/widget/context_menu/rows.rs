//! A menu's rows as the shared floating list draws them: a command with its
//! leading state and trailing shortcut or chevron, a caption, a rule between
//! groups, and a row of emoji.

use vello::kurbo::{Point, Rect, Vec2};

use super::emoji_row::{EmojiRow, ROW_SHARE};
use super::{CAPTION_ROW_SHARE, MenuItem, SEPARATOR_HEIGHT, label_inset};
use crate::icons::{AppImage, Icon, LUCIDE_DEFAULT_STROKE_PX, draw_app_image, draw_icon};
use crate::paint::PaintCtx;
use crate::tokens::TextRole;
use crate::widget::floating_list::{FloatingMetrics, FloatingRow, RowState};
use crate::widget::floating_surface;

/// One row of a menu, in the order the menu shows it.
#[derive(Clone)]
pub(super) struct MenuRow {
    pub(super) item: MenuItem,
    /// What the trailing column says on this platform: the shortcut, or the
    /// detail a command names.
    pub(super) trailing: Option<String>,
    /// The width the menu keeps for its trailing column, so a label elides
    /// before it.
    pub(super) trailing_width: f64,
}

/// What every row of a menu shares.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct MenuColumns {
    /// A leading lane for the state or icon of a row that has one.
    pub(super) gutter: bool,
}

impl FloatingRow for MenuRow {
    type Columns = MenuColumns;

    fn height(&self, m: &FloatingMetrics) -> f64 {
        match &self.item {
            MenuItem::Action { .. } => m.row,
            // A caption is a caption line with an `s1` of air either side,
            // tighter than a row: it is read with the rows above it.
            MenuItem::Caption(_) => (m.row * CAPTION_ROW_SHARE).ceil(),
            MenuItem::Emoji(_) => (m.row * ROW_SHARE).ceil(),
            MenuItem::Separator => SEPARATOR_HEIGHT,
        }
    }

    fn pickable(&self) -> bool {
        self.item.is_enabled()
    }

    fn alone_row(&self) -> bool {
        matches!(self.item, MenuItem::Action { .. })
    }

    fn cell_count(&self) -> usize {
        match &self.item {
            MenuItem::Emoji(row) => row.cells(),
            _ => 0,
        }
    }

    fn rows_inset(metrics: &FloatingMetrics, panel: Rect) -> Vec2 {
        // A menu's rows run the panel's full width, its armed row and emoji
        // cells inset from the edge on their own; down the sides they keep
        // the panel's inset, or a quarter of a panel too short for it.
        Vec2::new(0.0, metrics.inset.min(panel.height() / 4.0))
    }

    fn columns(rows: &[Self]) -> MenuColumns {
        MenuColumns {
            gutter: rows.iter().any(|row| {
                matches!(
                    row.item,
                    MenuItem::Action {
                        checked: Some(_),
                        ..
                    } | MenuItem::Action { icon: Some(_), .. }
                        | MenuItem::Action { image: Some(_), .. }
                )
            }),
        }
    }

    fn paint(
        &self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        state: RowState<'_, MenuColumns>,
    ) -> Option<f64> {
        let inset = label_inset(ctx.theme);
        let (content_x0, content_x1) = (row.x0 + inset, row.x1 - inset);
        match &self.item {
            MenuItem::Action {
                label,
                icon,
                image,
                checked,
                destructive,
                enabled,
                submenu,
                ..
            } => {
                let label = label.as_str();
                paint_action(
                    ctx,
                    row,
                    &state,
                    Action {
                        icon: *icon,
                        image: image.as_ref(),
                        checked: *checked,
                        destructive: *destructive,
                        enabled: *enabled,
                        submenu: submenu.is_some(),
                    },
                    label,
                    (self.trailing.as_deref(), self.trailing_width),
                    (content_x0, content_x1),
                );
            }
            MenuItem::Caption(text) => {
                paint_caption(ctx, row, &state, text, (content_x0, content_x1));
            }
            MenuItem::Emoji(emoji) => paint_emoji(ctx, row, &state, emoji),
            MenuItem::Separator => {
                crate::surface::separator(
                    ctx.scene,
                    ctx.transform,
                    row.x0,
                    row.x1,
                    row.center().y,
                    inset,
                    crate::chrome::panel_ground(ctx.theme),
                );
            }
        }
        None
    }
}

/// What a command row says about itself.
pub(super) struct Action<'a> {
    icon: Option<Icon>,
    image: Option<&'a AppImage>,
    checked: Option<bool>,
    destructive: bool,
    enabled: bool,
    submenu: bool,
}

/// One command row: the armed lift, its leading state, its label and the
/// trailing column.
fn paint_action(
    ctx: &mut PaintCtx<'_>,
    row: Rect,
    state: &RowState<'_, MenuColumns>,
    action: Action<'_>,
    label: &str,
    (trailing, trailing_width): (Option<&str>, f64),
    (content_x0, content_x1): (f64, f64),
) {
    let Action {
        icon,
        image,
        checked,
        destructive,
        enabled,
        submenu,
    } = action;
    let theme = ctx.theme;
    let ground = crate::chrome::panel_ground(theme);
    let colors = &theme.tokens.colors;
    // The row a press or Enter takes, and the row whose nested menu is open,
    // lift as every menu paints it (`floating_surface::armed_row`).
    let armed = enabled && (state.armed || state.marked);
    let armed_ink = armed.then(|| {
        // Concentric with the panel it is inset in, as in every menu.
        let inset = floating_surface::menu_inset(theme);
        let row = Rect::new(row.x0 + inset, row.y0, row.x1 - inset, row.y1);
        floating_surface::paint_armed_row_in(
            ctx.scene,
            ctx.transform,
            row,
            state.radius,
            inset,
            theme,
        )
    });
    let ink = if !enabled {
        colors
            .on_surface_at(ground, crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST)
            .ink_dim
    } else if let Some(ink) = armed_ink {
        ink
    } else if destructive {
        colors.ink_on_any(
            &[ground],
            colors.semantic.danger,
            crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST,
        )
    } else {
        colors.text_primary
    };
    let mut label_x = content_x0;
    if state.columns.gutter {
        let icon_size = f64::from(theme.tokens.metrics.icon_sm);
        // A state row shows its state, not its icon: the check is what says
        // a toggle is on.
        let glyph = match checked {
            Some(on) => on.then_some(Icon::Check),
            None => icon,
        };
        let slot = Rect::from_origin_size(
            (label_x, row.y0 + (row.height() - icon_size) / 2.0),
            (icon_size, icon_size),
        );
        // An app's own artwork stands in its glyph's place once it has pixels.
        let drawn = checked.is_none()
            && image.is_some_and(|image| draw_app_image(ctx.scene, ctx.transform, slot, image));
        if !drawn && let Some(icon) = glyph {
            draw_icon(
                ctx.scene,
                ctx.transform,
                slot,
                icon,
                ink,
                LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        label_x += icon_size + f64::from(theme.tokens.spacing.s2);
    }
    let style = theme.text(TextRole::Body);
    let shown = ctx.text.elide_to_width_style(
        label,
        crate::num::f32_of_f64((content_x1 - trailing_width - label_x).max(1.0)),
        &style,
    );
    let (_, label_height) = ctx.text.measure_style(&shown, None, &style);
    let label_y = row.y0 + (row.height() - f64::from(label_height)) / 2.0;
    ctx.text.draw_style(
        ctx.scene,
        &shown,
        ink,
        ctx.transform.pre_translate(Vec2::new(label_x, label_y)),
        None,
        &style,
    );
    if submenu {
        let size = f64::from(theme.tokens.metrics.icon_sm);
        draw_icon(
            ctx.scene,
            ctx.transform,
            Rect::from_origin_size(
                (content_x1 - size, row.y0 + (row.height() - size) / 2.0),
                (size, size),
            ),
            Icon::ChevronRight,
            ink,
            LUCIDE_DEFAULT_STROKE_PX,
        );
    } else if let Some(text) = trailing {
        let (w, _) = ctx.text.measure_style(text, None, &style);
        let dim = if armed {
            ink
        } else {
            colors
                .on_surface_at(ground, crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST)
                .ink_dim
        };
        ctx.text.draw_style(
            ctx.scene,
            text,
            dim,
            ctx.transform
                .pre_translate(Vec2::new(content_x1 - f64::from(w), label_y)),
            None,
            &style,
        );
    }
}

/// A caption: a dim line read with the rows above it, never a target.
fn paint_caption(
    ctx: &mut PaintCtx<'_>,
    row: Rect,
    state: &RowState<'_, MenuColumns>,
    text: &str,
    (content_x0, content_x1): (f64, f64),
) {
    let theme = ctx.theme;
    let ground = crate::chrome::panel_ground(theme);
    let ink = theme
        .tokens
        .colors
        .on_surface_at(ground, crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST)
        .ink_dim;
    let text_x = if state.columns.gutter {
        content_x0 + f64::from(theme.tokens.metrics.icon_sm) + f64::from(theme.tokens.spacing.s2)
    } else {
        content_x0
    };
    let caption = theme.text(TextRole::Caption);
    let shown = ctx.text.elide_to_width_style(
        text,
        crate::num::f32_of_f64((content_x1 - text_x).max(1.0)),
        &caption,
    );
    let (_, height) = ctx.text.measure_style(&shown, None, &caption);
    ctx.text.draw_style(
        ctx.scene,
        &shown,
        ink,
        ctx.transform.pre_translate(Vec2::new(
            text_x,
            row.y0 + (row.height() - f64::from(height)) / 2.0,
        )),
        None,
        &caption,
    );
}

/// The cells of an emoji row, side by side across the panel's width, each as
/// wide as it is tall or as the row allows: they are emoji, not a label, and
/// keep the menu's inset from the panel's edge so a cell's lift stays off it.
pub(super) fn emoji_cells(row: Rect, inset: f64, cells: usize) -> Vec<Rect> {
    let inner = Rect::new(
        row.x0 + inset,
        row.y0 + inset,
        row.x1 - inset,
        row.y1 - inset,
    );
    let count = cells.max(1);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a menu row holds a handful of emoji"
    )]
    let share = inner.width() / count as f64;
    let side = share.min(inner.height());
    (0..count)
        .map(|cell| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a menu row holds a handful of emoji"
            )]
            let center_x = inner.x0 + share * (cell as f64 + 0.5);
            Rect::from_center_size(Point::new(center_x, inner.center().y), (side, side))
        })
        .collect()
}

/// A row of emoji to react with: the armed cell lifts, the rest are drawn
/// larger than a label to be told apart at a glance.
fn paint_emoji(
    ctx: &mut PaintCtx<'_>,
    row: Rect,
    state: &RowState<'_, MenuColumns>,
    emoji: &EmojiRow,
) {
    let theme = ctx.theme;
    let style = theme.text(TextRole::Title);
    let cells = emoji_cells(row, state.metrics.inset, emoji.cells());
    let radius = f64::from(theme.tokens.radii.item);
    for (cell, rect) in cells.iter().enumerate() {
        let armed = state.armed && state.cell == cell;
        let ink = if armed {
            floating_surface::paint_armed_row_in(
                ctx.scene,
                ctx.transform,
                *rect,
                radius,
                0.0,
                theme,
            )
        } else {
            theme.tokens.colors.text_dim
        };
        if let Some(glyph) = emoji.emoji.get(cell) {
            let glyph_rect = rect.inset(-f64::from(theme.tokens.spacing.s1));
            crate::widget::reaction_strip::paint_emoji(ctx, glyph_rect, glyph, &style, ink);
        } else {
            let size = f64::from(theme.tokens.metrics.icon_md);
            draw_icon(
                ctx.scene,
                ctx.transform,
                Rect::from_center_size(rect.center(), (size, size)),
                Icon::SmilePlus,
                ink,
                LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }
}
