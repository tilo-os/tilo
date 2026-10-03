//! The sidebar's contents: the smart lists and projects as navigation rows,
//! each calendar account's calendars as visibility checkboxes, the selected
//! list's open tasks, and the month navigator at the bottom.

use vello::kurbo::{Point, Rect};
use vello::peniko::Fill;

use ui::chrome::{
    self, NavCount, NavRowFace, NavSectionFace, SelectionState, nav_row_height, nav_section_height,
    nav_section_layout,
};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Checkbox, CheckboxFace, ExternalDatePickerHit, Widget};

use super::{Action, CalendarApp, MiniMonthOutcome};
use crate::draw_util::text_in_band;
use crate::model::{ListSel, Priority};
use crate::sidebar::Action as SidebarAction;

/// One checkbox row: its label, state and an optional trailing flag (a
/// task's priority) in its own ink.
struct CheckRow<'a> {
    label: &'a str,
    checked: bool,
    hovered: bool,
    /// Quiet ink for the label (a completed task).
    dim: bool,
    trailing: Option<(&'a str, vello::peniko::Color)>,
}

/// One sidebar paint pass: where the next row goes, the sidebar's own theme
/// and the hit rects the rows push.
struct Column<'a> {
    rect: Rect,
    y: f64,
    theme: &'a Theme,
    pointer: Point,
    hits: Vec<(Rect, Action)>,
}

impl Column<'_> {
    fn row(&self, height: f64) -> Rect {
        Rect::new(self.rect.x0, self.y, self.rect.x1, self.y + height)
    }
}

impl CalendarApp {
    pub(super) fn draw_sidebar(&mut self, ctx: &mut PaintCtx<'_>) -> Vec<(Rect, Action)> {
        let switcher = (!self.topbar.switcher_visible()).then_some(self.view);
        let Some(chrome) = self.sidebar.draw_chrome(ctx, switcher, self.last_mouse) else {
            return Vec::new();
        };
        let hits = chrome
            .hits
            .into_iter()
            .map(|(rect, action)| {
                let action = match action {
                    SidebarAction::Dismiss => Action::DismissSidebar,
                    SidebarAction::Guard => Action::Noop,
                    SidebarAction::Toggle => Action::ToggleSidebar,
                    SidebarAction::Menu => Action::OpenSidebarMenu,
                    SidebarAction::View(view) => Action::View(view),
                };
                (rect, action)
            })
            .collect();
        let rect = self.sidebar.rect();
        let mut column = Column {
            rect,
            y: chrome.body_y,
            theme: &chrome.theme,
            pointer: self.last_mouse,
            hits,
        };
        let mut ctx = PaintCtx::new(ctx.scene, ctx.transform, ctx.text, &chrome.theme);
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &rect,
        );
        let picker = self.mini_month_rect(&chrome.theme);
        self.draw_sidebar_lists(&mut ctx, &mut column);
        self.draw_sidebar_calendars(&mut ctx, &mut column);
        self.draw_sidebar_tasks(&mut ctx, &mut column, picker.y0);
        if column.y <= picker.y0 {
            self.draw_sidebar_month(&mut ctx, &mut column, picker);
        }
        ctx.scene.pop_layer();
        column.hits
    }

    /// The month navigator's rect at the bottom of the sidebar: a header
    /// row and a weekday row above six week rows.
    fn mini_month_rect(&self, theme: &Theme) -> Rect {
        let r = self.sidebar.rect();
        let m = &theme.tokens.metrics;
        let inset = f64::from(theme.tokens.spacing.s3);
        let height =
            f64::from(m.min_pointer_target + m.icon_md) + 6.0 * f64::from(m.bar_control_height);
        Rect::new(
            r.x0 + inset,
            r.y1 - inset - height,
            r.x1 - inset,
            r.y1 - inset,
        )
    }

    fn draw_sidebar_lists(&self, ctx: &mut PaintCtx<'_>, column: &mut Column<'_>) {
        let theme = column.theme;
        let row_h = nav_row_height(theme);
        let smart = [
            ("Inbox", ListSel::Inbox, Icon::Inbox),
            ("Today", ListSel::Today, Icon::Star),
            ("Upcoming", ListSel::Upcoming, Icon::Calendar),
            ("Completed", ListSel::Completed, Icon::Check),
        ];
        for (label, sel, icon) in smart {
            let rect = column.row(row_h);
            let count = self.model.open_count(&sel, self.today);
            let mut face = NavRowFace::new(rect, label)
                .icon(icon)
                .state(SelectionState::new(
                    self.list_sel == sel,
                    rect.contains(column.pointer),
                    false,
                ));
            if count > 0 && sel != ListSel::Completed {
                face = face.count(NavCount::Neutral(u32::try_from(count).unwrap_or(u32::MAX)));
            }
            chrome::paint_nav_row(ctx.scene, ctx.transform, ctx.text, theme, &face);
            column.hits.push((rect, Action::List(sel)));
            column.y = rect.y1;
        }

        if self.model.projects.is_empty() {
            return;
        }
        let band = column.row(nav_section_height(theme, false));
        chrome::paint_nav_section(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &NavSectionFace::new(band, "Projects"),
        );
        column.y = band.y1;
        for project in &self.model.projects {
            let rect = column.row(row_h);
            let sel = ListSel::Project(project.id);
            let count = self
                .model
                .tasks
                .iter()
                .filter(|task| {
                    task.project == Some(project.id) && !task.done && task.parent.is_none()
                })
                .count();
            let mut face = NavRowFace::new(rect, &project.name).state(SelectionState::new(
                self.list_sel == sel,
                rect.contains(column.pointer),
                false,
            ));
            if count > 0 {
                face = face.count(NavCount::Neutral(u32::try_from(count).unwrap_or(u32::MAX)));
            }
            chrome::paint_nav_row(ctx.scene, ctx.transform, ctx.text, theme, &face);
            column.hits.push((rect, Action::List(sel)));
            column.y = rect.y1;
        }
    }

    /// Each account is a collapsible section whose trailing action syncs;
    /// its calendars are checkboxes that show or hide their events.
    fn draw_sidebar_calendars(&self, ctx: &mut PaintCtx<'_>, column: &mut Column<'_>) {
        let theme = column.theme;
        let row_h = nav_row_height(theme);
        for account in &self.accounts.accounts {
            let band = column.row(nav_section_height(theme, false));
            let face = NavSectionFace::new(band, account.label())
                .collapsible(!account.collapsed)
                .action(Icon::Refresh)
                .hovered(band.contains(column.pointer));
            let layout = nav_section_layout(theme, &face);
            let face = NavSectionFace {
                action_hover: layout.action.is_some_and(|a| a.contains(column.pointer)),
                ..face
            };
            chrome::paint_nav_section(ctx.scene, ctx.transform, ctx.text, theme, &face);
            column.hits.push((
                layout.band,
                Action::ToggleAccountCollapsed(account.id.clone()),
            ));
            if let Some(action) = layout.action {
                column.hits.push((action, Action::Sync));
            }
            column.y = band.y1;
            if account.collapsed {
                continue;
            }
            for calendar in &account.calendars {
                let rect = column.row(row_h);
                Self::checkbox_row(
                    ctx,
                    theme,
                    rect,
                    &CheckRow {
                        label: &calendar.name,
                        checked: calendar.visible,
                        hovered: rect.contains(column.pointer),
                        dim: false,
                        trailing: None,
                    },
                );
                column.hits.push((
                    rect,
                    Action::SetCalendarVisible {
                        id: calendar.id.clone(),
                        visible: !calendar.visible,
                    },
                ));
                column.y = rect.y1;
            }
        }
    }

    /// The selected list's open tasks, when there is room above the month
    /// navigator. The checkbox and the label complete a task; the rest of
    /// the row opens it.
    fn draw_sidebar_tasks(&self, ctx: &mut PaintCtx<'_>, column: &mut Column<'_>, limit: f64) {
        let theme = column.theme;
        let row_h = nav_row_height(theme);
        let tasks = self.model.tasks_for(&self.list_sel, self.today);
        let section_h = nav_section_height(theme, false);
        if tasks.is_empty() || column.y + section_h + row_h > limit {
            return;
        }
        let title = match &self.list_sel {
            ListSel::Inbox => "Inbox",
            ListSel::Today => "Today",
            ListSel::Upcoming => "Upcoming",
            ListSel::Completed => "Completed",
            ListSel::Project(id) => self
                .model
                .project(*id)
                .map_or("Tasks", |project| project.name.as_str()),
        };
        let band = column.row(section_h);
        chrome::paint_nav_section(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &NavSectionFace::new(band, title),
        );
        column.y = band.y1;
        for task in tasks {
            let rect = column.row(row_h);
            if rect.y1 > limit {
                break;
            }
            let flag = (task.priority != Priority::None).then(|| task.priority.flag());
            let complete = Self::checkbox_row(
                ctx,
                theme,
                rect,
                &CheckRow {
                    label: &task.title,
                    checked: task.done,
                    hovered: rect.contains(column.pointer),
                    dim: task.done,
                    trailing: flag.map(|flag| (flag, task.priority.color(theme))),
                },
            );
            // The wide "open" target first, then the narrower "complete"
            // target over it, so the reverse hit scan lets the checkbox and
            // label win where they overlap.
            column.hits.push((rect, Action::OpenTask(task.id)));
            column.hits.push((complete, Action::ToggleTask(task.id)));
            column.y = rect.y1;
        }
    }

    /// A checkbox and its label on one sidebar row, inset like a navigation
    /// row. Returns the part of the row that toggles the box: the box and
    /// its label.
    fn checkbox_row(ctx: &mut PaintCtx<'_>, theme: &Theme, rect: Rect, row: &CheckRow<'_>) -> Rect {
        let label = row.label;
        let sp = &theme.tokens.spacing;
        let side = f64::from(theme.tokens.metrics.icon_md);
        let x0 = rect.x0 + f64::from(sp.s4);
        let mid = rect.center().y;
        let check = Rect::new(x0, mid - side / 2.0, x0 + side, mid + side / 2.0);
        Checkbox::paint(
            ctx.scene,
            ctx.transform,
            theme,
            &CheckboxFace::new(check, row.checked).hovered(row.hovered),
        );
        let label_x = check.x1 + f64::from(theme.tokens.metrics.icon_label_gap);
        let mut label_x1 = rect.x1 - f64::from(sp.s4);
        if let Some((text, ink)) = row.trailing {
            let style = theme.text(TextRole::Caption).strong();
            let width = f64::from(ctx.text.measure_style(text, None, &style).0);
            text_in_band(ctx, text, &style, ink, label_x1 - width, rect, width);
            label_x1 -= width + f64::from(sp.s2);
        }
        let style = theme.text(TextRole::Body);
        let ink = if row.dim {
            theme.text_dim
        } else {
            theme.text_primary
        };
        text_in_band(ctx, label, &style, ink, label_x, rect, label_x1 - label_x);
        let shown = ctx.text.elide_to_width_style(
            label,
            ui::num::f32_of_f64((label_x1 - label_x).max(0.0)),
            &style,
        );
        let label_w = f64::from(ctx.text.measure_style(&shown, None, &style).0);
        Rect::new(
            rect.x0,
            rect.y0,
            (label_x + label_w + f64::from(sp.s2)).min(rect.x1),
            rect.y1,
        )
    }

    /// The month navigator: the shared date grid on the anchor date; its
    /// arrows move the anchor by a month.
    fn draw_sidebar_month(&mut self, ctx: &mut PaintCtx<'_>, column: &mut Column<'_>, rect: Rect) {
        self.mini_month.set_selected(self.anchor);
        self.mini_month.apply_layout_external(rect);
        self.mini_month.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            column.theme,
            &ui::presence::PresenceCtx::empty(),
        );
        for (hit, rect) in self.mini_month.hit_regions_external() {
            let outcome = match hit {
                ExternalDatePickerHit::PreviousMonth => MiniMonthOutcome::ShiftMonth(-1),
                ExternalDatePickerHit::NextMonth => MiniMonthOutcome::ShiftMonth(1),
                ExternalDatePickerHit::Date(day) => MiniMonthOutcome::SelectDate(day),
            };
            column.hits.push((rect, Action::MiniMonth(outcome)));
        }
    }
}
