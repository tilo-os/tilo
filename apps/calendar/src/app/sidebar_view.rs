//! The sidebar's contents: the smart lists and projects as navigation rows
//! (a project led by its colour), each calendar account's calendars as
//! checked navigation rows in their colours that show or hide their events,
//! the selected list's open tasks, and the month navigator at the bottom.

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
        // A chosen task list takes the bottom of the sidebar; with none
        // chosen, the month navigator does.
        if let Some(list) = self.list_sel {
            let bottom = rect.y1 - f64::from(chrome.theme.tokens.spacing.s3);
            self.draw_sidebar_tasks(&mut ctx, &mut column, list, bottom);
        } else if column.y <= picker.y0 {
            self.draw_sidebar_month(&mut ctx, &mut column, picker);
        }
        ctx.scene.pop_layer();
        column.hits
    }

    /// The month navigator's rect at the bottom of the sidebar: as wide as
    /// the column and as tall as that width needs.
    fn mini_month_rect(&self, theme: &Theme) -> Rect {
        let r = self.sidebar.rect();
        let inset = f64::from(theme.tokens.spacing.s3);
        let height = self.mini_month.intrinsic_height(r.width() - 2.0 * inset);
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
                    self.list_sel == Some(sel),
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
            let mut face = NavRowFace::new(rect, &project.name)
                .swatch(project.color.color(theme))
                .state(SelectionState::new(
                    self.list_sel == Some(sel),
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
    /// its calendars are checked rows in their colours: the whole row shows
    /// or hides the calendar's events.
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
                let face = NavRowFace::new(rect, &calendar.name)
                    .checked(calendar.visible)
                    .swatch(calendar.palette().color(theme))
                    .hovered(rect.contains(column.pointer));
                chrome::paint_nav_row(ctx.scene, ctx.transform, ctx.text, theme, &face);
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

    /// The chosen list's tasks under its name, down to `limit`. The
    /// heading's trailing action puts the list away; a task's checkbox and
    /// label complete it; the rest of the row opens it.
    fn draw_sidebar_tasks(
        &self,
        ctx: &mut PaintCtx<'_>,
        column: &mut Column<'_>,
        list: ListSel,
        limit: f64,
    ) {
        let theme = column.theme;
        let row_h = nav_row_height(theme);
        let tasks = self.model.tasks_for(&list, self.today);
        let section_h = nav_section_height(theme, false);
        if column.y + section_h > limit {
            return;
        }
        let title = match &list {
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
        let face = NavSectionFace::new(band, title).action(Icon::X);
        let layout = nav_section_layout(theme, &face);
        let face = NavSectionFace {
            action_hover: layout.action.is_some_and(|a| a.contains(column.pointer)),
            ..face
        };
        chrome::paint_nav_section(ctx.scene, ctx.transform, ctx.text, theme, &face);
        if let Some(action) = layout.action {
            column.hits.push((action, Action::CloseList));
        }
        column.y = band.y1;
        if tasks.is_empty() {
            let line = column.row(row_h);
            let x = line.x0 + f64::from(theme.tokens.spacing.s4);
            text_in_band(
                ctx,
                "No tasks",
                &theme.text(TextRole::Body),
                theme.text_dim,
                x,
                line,
                line.x1 - x,
            );
            column.y = line.y1;
            return;
        }
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

    /// The month navigator: the shared date grid on the anchor date, a dot
    /// under each day with a shown event; its arrows move the anchor by a
    /// month.
    fn draw_sidebar_month(&mut self, ctx: &mut PaintCtx<'_>, column: &mut Column<'_>, rect: Rect) {
        self.mini_month.set_selected(self.anchor);
        self.mini_month.set_today(self.today);
        let (year, month) = self.mini_month.month_view();
        let hidden = self.hidden_calendar_ids();
        if let Some(first) = chrono::NaiveDate::from_ymd_opt(year, month, 1) {
            // The grid spills into the neighbouring months: seven weeks from
            // the week before the first cover every cell.
            let start = first - chrono::Duration::days(7);
            let marked: Vec<_> = (0..49)
                .map(|offset| start + chrono::Duration::days(offset))
                .filter(|day| {
                    self.model
                        .events_on(*day)
                        .iter()
                        .any(|event| event.visible_under(&hidden))
                })
                .collect();
            self.mini_month.set_marked(marked);
        }
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
