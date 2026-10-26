//! The calendar in the shell's frame: the sidebar is the shell's (its
//! width, its drawer in a narrow window, its toggle and keys), and the
//! calendar says what it holds. The smart lists and projects are places;
//! each account's calendars are checked rows in their colours that show or
//! hide their events; the chosen list's tasks are checked rows that complete
//! them; the month navigator stands at the sidebar's foot.
//!
//! The calendar's model stays the one truth: every sync declares the places
//! and the month again ([`NavFeed`], [`DateFeed`]), and what the person does
//! in them comes back as the calendar's own actions.

use chrono::{Datelike, NaiveDate};
use ui::icons::Icon;
use ui::widget::{
    DateFeed, DatePicker, DateShown, Element, Manual, NavAction, NavFeed, NavItem, NavList,
    NavSection, Tone, When, WindowBarLink,
};
use vello::kurbo::Rect;

use super::{Action, CalendarApp};
use crate::model::{Id, ListSel, Priority, View};

/// The smart lists, in the order the sidebar shows them, by the Fluent key
/// each is named with.
const SMART_LISTS: [(&str, ListSel, Icon); 4] = [
    ("calendar-tasks-inbox", ListSel::Inbox, Icon::Inbox),
    ("calendar-tasks-today", ListSel::Today, Icon::Star),
    ("calendar-tasks-upcoming", ListSel::Upcoming, Icon::Calendar),
    ("calendar-tasks-completed", ListSel::Completed, Icon::Check),
];

const TASKS: &str = "tasks";
const CLOSE: &str = "close";
const SYNC: &str = "sync";
/// A task row's action: open the task.
const OPEN: &str = "open";
const ACCOUNT: &str = "account:";
const CALENDAR: &str = "calendar:";
const TASK: &str = "task:";

/// The place id of a smart list or project.
pub(crate) fn list_place(list: &ListSel) -> String {
    match list {
        ListSel::Inbox => "list:inbox".into(),
        ListSel::Today => "list:today".into(),
        ListSel::Upcoming => "list:upcoming".into(),
        ListSel::Completed => "list:completed".into(),
        ListSel::Project(id) => format!("project:{id}"),
    }
}

/// The list a place id names.
fn place_list(place: &str) -> Option<ListSel> {
    if let Some(id) = place.strip_prefix("project:") {
        return id.parse().ok().map(ListSel::Project);
    }
    SMART_LISTS
        .iter()
        .map(|(_, list, _)| *list)
        .find(|list| list_place(list) == place)
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The sidebar's column: the places above, the month at the foot, as
/// wide as the column and as tall as that width needs.
pub(crate) fn sidebar(places: &NavFeed, month: &DateFeed) -> Element {
    let list = NavList::new(Vec::new())
        .with_id("calendar.places")
        .with_accessible_name(crate::resource::t("calendar-navigation"))
        .fed_by(places)
        .fed_actions(
            places,
            vec![NavAction::new(
                OPEN,
                Icon::ChevronRight,
                crate::resource::t("calendar-open-task"),
            )],
        );
    let picker = DatePicker::new(ui::clock::today())
        .navigator()
        .with_accessible_name(crate::resource::t("calendar-month"))
        .fed_by(month);
    // A chosen task list takes the foot of the sidebar; else the month.
    let shown = month.clone();
    let picker = When::new(Box::new(picker), move || shown.is_shown());
    Box::new(
        Manual::new(|ctx| {
            let size = ctx.available();
            let (w, h) = (f64::from(size.width), f64::from(size.height));
            let inset = f64::from(ui::tokens::Spacing::BASE.s3);
            let inner = (w - 2.0 * inset).max(0.0);
            let constraint = taffy::Size {
                width: ui::widget::MeasureConstraint::Exact(ui::num::f32_of_f64(inner)),
                height: ui::widget::MeasureConstraint::Intrinsic,
            };
            let measured = ctx.measure(1, constraint)?;
            let picker_h = f64::from(measured.size.height).min((h - 2.0 * inset).max(0.0));
            if picker_h <= 0.0 {
                ctx.place(0, Rect::new(0.0, 0.0, w, h))?;
                ctx.place(1, Rect::new(inset, h, inset + inner, h))?;
                return Ok(size);
            }
            let foot = (h - inset - picker_h).max(0.0);
            ctx.place(0, Rect::new(0.0, 0.0, w, (foot - inset).max(0.0)))?;
            ctx.place(1, Rect::new(inset, foot, inset + inner, foot + picker_h))?;
            Ok(size)
        })
        .child(Box::new(list))
        .child(Box::new(picker)),
    )
}

impl CalendarApp {
    /// Stand in the shell: show the places in `places`, the month in
    /// `month`, and read the shell's sidebar through `bar`.
    #[must_use]
    pub(crate) fn in_shell(
        mut self,
        places: &NavFeed,
        month: &DateFeed,
        bar: WindowBarLink,
    ) -> Self {
        self.places = places.clone();
        self.month = month.clone();
        self.bar = Some(bar);
        self.show_shell();
        self
    }

    /// Show the shell's sidebar or put it away (a capture's staged state,
    /// the narrow window's drawer).
    pub(crate) fn stage_sidebar(&self, open: bool) {
        if let Some(bar) = &self.bar {
            bar.stage_sidebar(open);
        }
    }

    /// Put the sidebar away if it floats over the calendar.
    pub(super) fn put_drawer_away(&self) {
        if let Some(bar) = &self.bar {
            bar.put_drawer_away();
        }
    }

    /// Whether the shell shows the sidebar.
    pub(crate) fn sidebar_open(&self) -> bool {
        self.bar
            .as_ref()
            .is_some_and(WindowBarLink::is_sidebar_open)
    }

    /// Declare the places and the month as the model has them now.
    pub(super) fn show_shell(&self) {
        let mut sections = vec![self.lists_section()];
        sections.extend(self.projects_section());
        sections.extend(self.account_sections());
        let tasks = self.tasks_section();
        let month = tasks.is_none();
        sections.extend(tasks);
        self.places
            .show(sections, self.list_sel.as_ref().map(list_place));
        if month {
            self.show_month();
        } else {
            self.month.withdraw();
        }
    }

    fn lists_section(&self) -> NavSection {
        let items = SMART_LISTS
            .iter()
            .map(|(key, list, icon)| {
                let item = NavItem::new(list_place(list), crate::resource::t(key))
                    .icon(*icon)
                    .without_actions();
                let open = self.model.open_count(list, self.today);
                if open > 0 && *list != ListSel::Completed {
                    item.count(count(open))
                } else {
                    item
                }
            })
            .collect();
        NavSection::untitled("lists", items)
    }

    fn projects_section(&self) -> Option<NavSection> {
        if self.model.projects.is_empty() {
            return None;
        }
        let theme = &self.layout_theme;
        let items = self
            .model
            .projects
            .iter()
            .map(|project| {
                let item = NavItem::new(list_place(&ListSel::Project(project.id)), &project.name)
                    .swatch(project.color.color(theme))
                    .without_actions();
                let open = self
                    .model
                    .tasks
                    .iter()
                    .filter(|task| {
                        task.project == Some(project.id) && !task.done && task.parent.is_none()
                    })
                    .count();
                if open > 0 {
                    item.count(count(open))
                } else {
                    item
                }
            })
            .collect();
        Some(NavSection::new(
            "projects",
            crate::resource::t("calendar-projects"),
            items,
        ))
    }

    /// Each account folds; its action syncs; its calendars are checked rows.
    fn account_sections(&self) -> Vec<NavSection> {
        let theme = &self.layout_theme;
        self.accounts
            .accounts
            .iter()
            .map(|account| {
                let items = account
                    .calendars
                    .iter()
                    .map(|calendar| {
                        NavItem::new(format!("{CALENDAR}{}", calendar.id), &calendar.name)
                            .checked(calendar.visible)
                            .swatch(calendar.palette().color(theme))
                            .without_actions()
                    })
                    .collect();
                let section =
                    NavSection::new(format!("{ACCOUNT}{}", account.id), account.label(), items)
                        .trailing_action(NavAction::new(
                            SYNC,
                            Icon::Refresh,
                            crate::resource::t("calendar-sync"),
                        ));
                if account.collapsed {
                    section.collapsed()
                } else {
                    section.collapsible()
                }
            })
            .collect()
    }

    /// The chosen list's tasks under its name: checking one completes it,
    /// its Open action opens it; the header's action puts the list away.
    /// The Tasks screen shows the list itself, so the sidebar doesn't.
    fn tasks_section(&self) -> Option<NavSection> {
        let list = self.list_sel.filter(|_| self.view != View::Tasks)?;
        let items = self
            .model
            .tasks_for(&list, self.today)
            .into_iter()
            .map(|task| {
                let item =
                    NavItem::new(format!("{TASK}{}", task.id), &task.title).checked(task.done);
                match task.priority {
                    Priority::None => item,
                    Priority::Low => item.status_label(task.priority.flag(), Tone::Info),
                    Priority::Medium => item.status_label(task.priority.flag(), Tone::Warning),
                    Priority::High => item.status_label(task.priority.flag(), Tone::Danger),
                }
            })
            .collect();
        Some(
            NavSection::new(TASKS, self.model.list_title(&list), items)
                .trailing_action(NavAction::new(
                    CLOSE,
                    Icon::X,
                    crate::resource::t("calendar-close-list"),
                ))
                .empty_caption(crate::resource::t("calendar-list-empty")),
        )
    }

    /// The month at the sidebar's foot: the anchor, today, and a dot under
    /// each day with a shown event.
    fn show_month(&self) {
        let hidden = self.hidden_calendar_ids();
        // The grid spills into the neighbouring months: seven weeks from
        // the week before the anchor's month cover every cell.
        let first = self.anchor.with_day(1).unwrap_or(self.anchor);
        let start = first - chrono::Duration::days(7);
        let marked: Vec<NaiveDate> = (0..49)
            .map(|offset| start + chrono::Duration::days(offset))
            .filter(|day| {
                self.model
                    .events_on(*day)
                    .iter()
                    .any(|event| event.visible_under(&hidden))
            })
            .collect();
        self.month.show(DateShown {
            selected: self.anchor,
            today: self.today,
            marked,
        });
    }

    /// Run what the person did in the sidebar as the calendar's own
    /// actions, then show the result.
    pub(super) fn take_shell_events(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let mut actions = Vec::new();
        for place in self.places.take_chosen() {
            if let Some(list) = place_list(&place) {
                // Choosing a list shows it; it stays chosen.
                if self.list_sel != Some(list) {
                    actions.push(Action::List(list));
                }
            }
        }
        for (place, checked) in self.places.take_checks() {
            if let Some(id) = place.strip_prefix(CALENDAR) {
                actions.push(Action::SetCalendarVisible {
                    id: id.to_owned(),
                    visible: checked,
                });
            } else if let Some(id) = place
                .strip_prefix(TASK)
                .and_then(|id| id.parse::<Id>().ok())
                && self.model.task(id).is_some_and(|task| task.done != checked)
            {
                actions.push(Action::ToggleTask(id));
            }
        }
        for (place, action) in self.places.take_item_actions() {
            if action == OPEN
                && let Some(id) = place
                    .strip_prefix(TASK)
                    .and_then(|id| id.parse::<Id>().ok())
            {
                actions.push(Action::OpenTask(id));
            }
        }
        for (section, open) in self.places.take_expands() {
            if let Some(id) = section.strip_prefix(ACCOUNT)
                && self
                    .accounts
                    .accounts
                    .iter()
                    .any(|account| account.id == id && account.collapsed == open)
            {
                actions.push(Action::ToggleAccountCollapsed(id.to_owned()));
            }
        }
        for (section, action) in self.places.take_section_actions() {
            match (section.as_str(), action.as_str()) {
                (TASKS, CLOSE) => actions.push(Action::CloseList),
                (section, SYNC) if section.starts_with(ACCOUNT) => actions.push(Action::Sync),
                _ => {}
            }
        }
        actions.extend(self.month.take_chosen().into_iter().map(Action::PickDate));
        if actions.is_empty() {
            return;
        }
        for action in actions {
            self.apply_action(action, doc);
        }
        self.dirty = true;
        self.show_shell();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_list_place_names_its_list() {
        let project = ListSel::Project(Id::new_v4());
        for list in SMART_LISTS
            .iter()
            .map(|(_, list, _)| *list)
            .chain([project])
        {
            assert_eq!(place_list(&list_place(&list)), Some(list));
        }
        assert_eq!(place_list("calendar:work"), None);
    }
}
