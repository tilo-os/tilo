//! Library shelf sorting and filtering controls.
//!
//! The component owns dropdown visibility, drawing, and its topmost hit stack.
//! Persisted preference changes are emitted as semantic actions for the app.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Toggle, ToggleFace};

use crate::model::{LibraryPrefs, LibrarySort};
use crate::paint;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShelfControlAction {
    ToggleSortMenu,
    SetSort(LibrarySort),
    ToggleHideRead,
    Swallow,
}

pub(super) struct ShelfPaintCtx<'a> {
    pub scene: &'a mut Scene,
    pub transform: Affine,
    pub text: &'a mut TextRenderer,
    pub theme: &'a Theme,
}

#[derive(Clone, Copy)]
pub(super) struct ShelfBarView {
    pub viewport: Rect,
    pub open_book_x: f64,
    pub cursor: Point,
    pub preferences: LibraryPrefs,
}

#[derive(Clone, Copy)]
pub(super) struct ShelfMenuView {
    pub viewport: Rect,
    pub anchor: Rect,
    pub cursor: Point,
    pub selected: LibrarySort,
}

#[derive(Default)]
pub(super) struct ShelfControls {
    sort_menu_open: bool,
    hits: Vec<(Rect, ShelfControlAction)>,
}

impl ShelfControls {
    pub(super) fn accessibility_nodes(
        &self,
        prefs: LibraryPrefs,
        focused: Option<&str>,
    ) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        self.hits
            .iter()
            .filter_map(|(rect, action)| {
                let (target, name, role, state) = match action {
                    ShelfControlAction::ToggleSortMenu => (
                        "books.library.sort",
                        format!("Sort books, currently {}", prefs.sort.label()),
                        Role::ComboBox,
                        AccessState::new().expanded(self.sort_menu_open),
                    ),
                    ShelfControlAction::SetSort(sort) => (
                        match sort {
                            LibrarySort::Recent => "books.library.sort.recent",
                            LibrarySort::Title => "books.library.sort.title",
                            LibrarySort::Author => "books.library.sort.author",
                            LibrarySort::Progress => "books.library.sort.progress",
                        },
                        format!("Sort by {}", sort.label()),
                        Role::RadioButton,
                        AccessState::new().checked(prefs.sort == *sort),
                    ),
                    ShelfControlAction::ToggleHideRead => (
                        "books.library.hide-read",
                        "Hide finished books".into(),
                        Role::Switch,
                        AccessState::new().checked(prefs.hide_read),
                    ),
                    ShelfControlAction::Swallow => return None,
                };
                Some(
                    AccessNode::new(role)
                        .with_name(name)
                        .with_bounds(*rect)
                        .with_state(state.focusable(true).focused(focused == Some(target)))
                        .with_action_target(target),
                )
            })
            .collect()
    }

    pub(super) fn semantic_action(target: &str) -> Option<ShelfControlAction> {
        match target {
            "books.library.sort" => Some(ShelfControlAction::ToggleSortMenu),
            "books.library.sort.recent" => Some(ShelfControlAction::SetSort(LibrarySort::Recent)),
            "books.library.sort.title" => Some(ShelfControlAction::SetSort(LibrarySort::Title)),
            "books.library.sort.author" => Some(ShelfControlAction::SetSort(LibrarySort::Author)),
            "books.library.sort.progress" => {
                Some(ShelfControlAction::SetSort(LibrarySort::Progress))
            }
            "books.library.hide-read" => Some(ShelfControlAction::ToggleHideRead),
            _ => None,
        }
    }

    pub(super) fn begin_frame(&mut self) {
        self.hits.clear();
    }

    pub(super) fn toggle_sort_menu(&mut self) {
        self.sort_menu_open = !self.sort_menu_open;
    }

    pub(super) fn close_sort_menu(&mut self) {
        self.sort_menu_open = false;
    }

    #[cfg(test)]
    pub(super) fn sort_menu_open(&self) -> bool {
        self.sort_menu_open
    }

    #[cfg(test)]
    pub(super) fn has_action(&self, wanted: ShelfControlAction) -> bool {
        self.hits.iter().any(|(_, action)| *action == wanted)
    }

    pub(super) fn hit_test(&self, point: Point) -> Option<ShelfControlAction> {
        self.hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, action)| *action)
    }

    pub(super) fn draw_bar(&mut self, ctx: &mut ShelfPaintCtx<'_>, view: ShelfBarView) -> Rect {
        let colors = &ctx.theme.tokens.colors;
        let y = view.viewport.y0 + 13.0;
        let compact = view.viewport.width() < 560.0;
        let mut right = view.open_book_x - if compact { 12.0 } else { 16.0 };

        let label = if compact {
            "Sort".to_string()
        } else {
            format!("Sort · {}", view.preferences.sort.label())
        };
        let label_width = ctx
            .text
            .measure_weighted(&label, 13.0, None, TextWeight::Medium)
            .0;
        let width = f64::from(label_width) + if compact { 38.0 } else { 46.0 };
        right -= width;
        let sort = Rect::new(right, y, right + width, y + 30.0);
        paint::fill_round(
            ctx.scene,
            ctx.transform,
            sort,
            15.0,
            if self.sort_menu_open || sort.contains(view.cursor) {
                ctx.theme.bg_hover
            } else {
                colors.surfaces.overlay
            },
        );
        paint::stroke_round(
            ctx.scene,
            ctx.transform,
            sort,
            15.0,
            if self.sort_menu_open {
                ctx.theme.border_focus
            } else {
                colors.border_standard
            },
            1.0,
        );
        ctx.text.draw_text_weighted(
            ctx.scene,
            &label,
            13.0,
            colors.text_primary,
            ctx.transform * Affine::translate((right + 13.0, y + 8.0)),
            None,
            TextWeight::Medium,
        );
        paint::chevron_down(
            ctx.scene,
            ctx.transform,
            Rect::new(right + width - 26.0, y + 3.0, right + width - 4.0, y + 27.0),
            colors.text_dim,
            1.7,
        );
        self.hits.push((sort, ShelfControlAction::ToggleSortMenu));

        right -= if compact { 10.0 } else { 18.0 };
        let toggle = Rect::new(right - 40.0, y + 4.0, right, y + 26.0);
        Toggle::paint(
            ctx.scene,
            ctx.transform,
            ctx.theme,
            &ToggleFace::new(toggle, view.preferences.hide_read)
                .hovered(toggle.contains(view.cursor)),
        );
        self.hits.push((toggle, ShelfControlAction::ToggleHideRead));
        if !compact {
            let hide = "Hide read";
            let hide_width = ctx
                .text
                .measure_weighted(hide, 13.0, None, TextWeight::Medium)
                .0;
            ctx.text.draw_text_weighted(
                ctx.scene,
                hide,
                13.0,
                colors.text_dim,
                ctx.transform
                    * Affine::translate((toggle.x0 - 8.0 - f64::from(hide_width), y + 8.0)),
                None,
                TextWeight::Medium,
            );
        }
        sort
    }

    pub(super) fn draw_menu(&mut self, ctx: &mut ShelfPaintCtx<'_>, view: ShelfMenuView) {
        if !self.sort_menu_open {
            return;
        }
        let colors = &ctx.theme.tokens.colors;
        let width = 190.0_f64.max(view.anchor.width());
        let row_height = 34.0;
        let x = (view.anchor.x1 - width).max(view.viewport.x0 + 8.0);
        let top = view.anchor.y1 + 6.0;
        let menu = Rect::new(
            x,
            top,
            x + width,
            top + LibrarySort::ALL.len() as f64 * row_height + 8.0,
        );
        paint::fill_round(
            ctx.scene,
            ctx.transform,
            Rect::new(menu.x0 - 1.0, menu.y0 + 3.0, menu.x1 + 1.0, menu.y1 + 6.0),
            12.0,
            Color::from_rgba8(0, 0, 0, 80),
        );
        paint::fill_round(
            ctx.scene,
            ctx.transform,
            menu,
            f64::from(ctx.theme.tokens.radii.lg),
            colors.surfaces.raised,
        );
        paint::stroke_round(
            ctx.scene,
            ctx.transform,
            menu,
            f64::from(ctx.theme.tokens.radii.lg),
            colors.border_standard,
            1.0,
        );
        self.hits.push((menu, ShelfControlAction::Swallow));

        let mut row_y = top + 4.0;
        for choice in LibrarySort::ALL {
            let row = Rect::new(x + 6.0, row_y, x + width - 6.0, row_y + row_height - 4.0);
            let active = view.selected == choice;
            if active {
                paint::fill_round(
                    ctx.scene,
                    ctx.transform,
                    row,
                    8.0,
                    ctx.theme.accent_primary.with_alpha_factor(0.16),
                );
            } else if row.contains(view.cursor) {
                paint::fill_round(ctx.scene, ctx.transform, row, 8.0, ctx.theme.bg_hover);
            }
            ctx.text.draw_text_weighted(
                ctx.scene,
                choice.label(),
                13.0,
                if active {
                    ctx.theme.accent_primary
                } else {
                    colors.text_primary
                },
                ctx.transform * Affine::translate((x + 16.0, row_y + 8.0)),
                None,
                if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Medium
                },
            );
            self.hits.push((row, ShelfControlAction::SetSort(choice)));
            row_y += row_height;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(controls: &mut ShelfControls, prefs: LibraryPrefs) -> Rect {
        controls.begin_frame();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let viewport = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let anchor = controls.draw_bar(
            &mut ShelfPaintCtx {
                scene: &mut scene,
                transform: Affine::IDENTITY,
                text: &mut text,
                theme: &theme,
            },
            ShelfBarView {
                viewport,
                open_book_x: 1120.0,
                cursor: Point::ZERO,
                preferences: prefs,
            },
        );
        controls.draw_menu(
            &mut ShelfPaintCtx {
                scene: &mut scene,
                transform: Affine::IDENTITY,
                text: &mut text,
                theme: &theme,
            },
            ShelfMenuView {
                viewport,
                anchor,
                cursor: Point::ZERO,
                selected: prefs.sort,
            },
        );
        anchor
    }

    #[test]
    fn bar_hit_testing_exposes_sort_and_filter_actions() {
        let mut controls = ShelfControls::default();
        let anchor = draw(&mut controls, LibraryPrefs::default());
        assert_eq!(
            controls.hit_test(anchor.center()),
            Some(ShelfControlAction::ToggleSortMenu)
        );
        let toggle = controls
            .hits
            .iter()
            .find(|(_, action)| *action == ShelfControlAction::ToggleHideRead)
            .map(|(rect, _)| rect.center())
            .expect("hide-read toggle");
        assert_eq!(
            controls.hit_test(toggle),
            Some(ShelfControlAction::ToggleHideRead)
        );
    }

    #[test]
    fn open_menu_owns_rows_and_gap_swallowing() {
        let mut controls = ShelfControls::default();
        controls.toggle_sort_menu();
        let anchor = draw(&mut controls, LibraryPrefs::default());
        let progress = controls
            .hits
            .iter()
            .find(|(_, action)| *action == ShelfControlAction::SetSort(LibrarySort::Progress))
            .map(|(rect, _)| rect.center())
            .expect("progress row");
        assert_eq!(
            controls.hit_test(progress),
            Some(ShelfControlAction::SetSort(LibrarySort::Progress))
        );
        assert_eq!(
            controls.hit_test(Point::new(anchor.x0, anchor.y1 + 7.0)),
            Some(ShelfControlAction::Swallow)
        );
        controls.close_sort_menu();
        assert!(!controls.sort_menu_open());
    }
}
