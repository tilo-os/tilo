//! Library shelf sorting and filtering controls.
//!
//! The component owns dropdown visibility, drawing, and its topmost hit stack.
//! Persisted preference changes are emitted as semantic actions for the app.

use vello::kurbo::{Affine, Point, Rect};

use ui::paint::PaintCtx;
use ui::text_render::TextWeight;
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

    pub(super) fn draw_bar(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        viewport: Rect,
        open_book_x: f64,
        cursor: Point,
        prefs: LibraryPrefs,
    ) -> Rect {
        use crate::chrome::rhythm;
        let (transform, theme) = (ctx.transform, ctx.theme);
        let PaintCtx { scene, text, .. } = ctx;
        let colors = &theme.tokens.colors;
        let y = viewport.y0 + 13.0;
        let compact = viewport.width() < 560.0;
        // Bar controls are siblings: one `s3` between every neighbour, on
        // every width.
        let mut right = open_book_x - rhythm::SIBLING;

        let label = if compact {
            "Sort".to_string()
        } else {
            format!("Sort · {}", prefs.sort.label())
        };
        let label_width = text
            .measure_weighted(&label, 13.0, None, TextWeight::Medium)
            .0;
        let width = f64::from(label_width) + if compact { 38.0 } else { 46.0 };
        right -= width;
        let sort = Rect::new(right, y, right + width, y + 30.0);
        // A dropdown field rests like a Neutral button — raised face, derived
        // edge (HIG §9.5) — and wears the focus ring while its menu is open.
        crate::chrome::quiet_pill(
            scene,
            transform,
            theme,
            sort,
            15.0,
            colors.surfaces.raised,
            self.sort_menu_open || sort.contains(cursor),
        );
        if self.sort_menu_open {
            paint::stroke_round(scene, transform, sort, 15.0, theme.border_focus, 2.0);
        }
        text.draw_text_weighted(
            scene,
            &label,
            13.0,
            colors.text_primary,
            transform * Affine::translate((right + 13.0, y + 8.0)),
            None,
            TextWeight::Medium,
        );
        paint::chevron_down(
            scene,
            transform,
            Rect::new(right + width - 26.0, y + 3.0, right + width - 4.0, y + 27.0),
            colors.text_dim,
            1.7,
        );
        self.hits.push((sort, ShelfControlAction::ToggleSortMenu));

        right -= rhythm::SIBLING;
        let toggle = Rect::new(right - 40.0, y + 4.0, right, y + 26.0);
        Toggle::paint(
            scene,
            transform,
            theme,
            &ToggleFace::new(toggle, prefs.hide_read).hovered(toggle.contains(cursor)),
        );
        self.hits.push((
            crate::chrome::pointer_target(toggle, theme),
            ShelfControlAction::ToggleHideRead,
        ));
        if !compact {
            let hide = "Hide read";
            let hide_width = text
                .measure_weighted(hide, 13.0, None, TextWeight::Medium)
                .0;
            text.draw_text_weighted(
                scene,
                hide,
                13.0,
                colors.text_dim,
                transform
                    * Affine::translate((
                        toggle.x0 - rhythm::RELATED - f64::from(hide_width),
                        y + 8.0,
                    )),
                None,
                TextWeight::Medium,
            );
        }
        sort
    }

    pub(super) fn draw_menu(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        viewport: Rect,
        anchor: Rect,
        cursor: Point,
        selected: LibrarySort,
    ) {
        use crate::chrome::{MENU_ROW_H, rhythm};
        if !self.sort_menu_open {
            return;
        }
        let (transform, theme) = (ctx.transform, ctx.theme);
        let PaintCtx { scene, text, .. } = ctx;
        let colors = &theme.tokens.colors;
        let width = 190.0_f64.max(anchor.width());
        // Menu rows share the settings menu's pitch: a `MENU_ROW_H` face plus
        // a related step, with a related step of panel padding around them.
        let row_height = MENU_ROW_H + rhythm::RELATED;
        let x = (anchor.x1 - width).max(viewport.x0 + rhythm::RELATED);
        let top = anchor.y1 + rhythm::RELATED;
        let menu = Rect::new(
            x,
            top,
            x + width,
            top + LibrarySort::ALL.len() as f64 * row_height + rhythm::RELATED,
        );
        crate::chrome::panel(scene, transform, theme, menu);
        self.hits.push((menu, ShelfControlAction::Swallow));

        let mut row_y = top + rhythm::RELATED;
        for choice in LibrarySort::ALL {
            let row = Rect::new(
                x + rhythm::RELATED,
                row_y,
                x + width - rhythm::RELATED,
                row_y + MENU_ROW_H,
            );
            let active = selected == choice;
            // The current sort is a lift (HIG §9.3), not an accent wash.
            crate::chrome::row(
                scene,
                transform,
                theme,
                row,
                f64::from(theme.tokens.radii.md),
                ui::chrome::SelectionState::new(active, row.contains(cursor), false),
                false,
            );
            let (_, label_h) = text.measure_weighted(
                choice.label(),
                13.0,
                None,
                if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Medium
                },
            );
            text.draw_text_weighted(
                scene,
                choice.label(),
                13.0,
                colors.text_primary,
                transform
                    * Affine::translate((
                        row.x0 + rhythm::RELATED,
                        row.y0 + (row.height() - f64::from(label_h)) / 2.0,
                    )),
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
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::Scene;

    fn draw(controls: &mut ShelfControls, prefs: LibraryPrefs) -> Rect {
        controls.begin_frame();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let viewport = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let anchor = controls.draw_bar(&mut ctx, viewport, 1120.0, Point::ZERO, prefs);
        controls.draw_menu(&mut ctx, viewport, anchor, Point::ZERO, prefs.sort);
        anchor
    }

    /// The bar exposes sort and filter actions and an open menu owns its rows and swallows the gap.
    #[test]
    fn shelf_controls_hit_testing_and_menu_ownership() {
        // --- phase: bar_hit_testing_exposes_sort_and_filter_actions ---
        {
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

        // --- phase: open_menu_owns_rows_and_gap_swallowing ---
        {
            use crate::chrome::rhythm;
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
            // The menu opens a related step under its anchor and pads its rows
            // by another related step: a point inside that top padding is the
            // panel's own (swallowed), a point in the gap above it is nobody's.
            assert_eq!(
                controls.hit_test(Point::new(
                    anchor.x0,
                    anchor.y1 + rhythm::RELATED + rhythm::TIGHT
                )),
                Some(ShelfControlAction::Swallow)
            );
            assert_eq!(
                controls.hit_test(Point::new(anchor.x0, anchor.y1 + rhythm::TIGHT)),
                None
            );
            controls.close_sort_menu();
            assert!(!controls.sort_menu_open());
        }
    }
}
