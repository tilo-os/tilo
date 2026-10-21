//! The sidebar's contents, the navigation of the view shown. In Tasks: the
//! smart lists and projects as navigation rows (a project led by its colour),
//! the shown list selected; its tasks are the content, never listed here
//! too. In the calendar views: each calendar account's calendars as checked
//! navigation rows in their colours that show or hide their events, and the
//! month navigator at the bottom.

use vello::kurbo::{Point, Rect};
use vello::peniko::Fill;

use ui::chrome::{
    self, NavCount, NavRowFace, NavSectionFace, SelectionState, nav_row_height, nav_section_height,
    nav_section_layout,
};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::widget::{ExternalDatePickerHit, Widget};

use super::{Action, CalendarApp, MiniMonthOutcome};
use crate::model::{ListSel, View};
use crate::sidebar::Action as SidebarAction;

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
        // The sidebar is the navigation of the view shown: in Tasks, the
        // task lists (their tasks are the content, never listed twice); in
        // the calendar views, the calendars and the month navigator.
        if self.view == View::Tasks {
            self.draw_sidebar_lists(&mut ctx, &mut column);
        } else {
            self.draw_sidebar_calendars(&mut ctx, &mut column);
            let picker = self.mini_month_rect(&chrome.theme);
            if column.y <= picker.y0 {
                self.draw_sidebar_month(&mut ctx, &mut column, picker);
            }
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
                    self.shown_list() == sel,
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
                    self.shown_list() == sel,
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
