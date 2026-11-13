//! The list: every issue the sidebar's filter shows, sub-issues nested under
//! their parent.
//!
//! Each row reads as the issue's reference and title, with its status
//! beside it. Choosing a row opens the issue; dragging one onto another
//! makes it a sub-issue there, the tracker's `move-under`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::model::{Issue, Status, Tracker, tracker};
use ui::prelude::*;
use ui::reorder::{DropPlacement, ReorderDrop};
use ui::widget::context_menu::MenuItem;
use ui::widget::{ListRow, TreeMarks, VecListModel, VirtualList};

use crate::{Filter, Nav, reference, tr};

/// What a row shows besides its label.
#[derive(Clone)]
struct Row {
    /// Its place in the list, which is what the rows are addressed by.
    key: u64,
    /// The issue it shows, which is what actions naming the issue are drawn on.
    issue: Id,
    label: String,
    /// The row it stands under, where it is a sub-issue.
    parent: Option<u64>,
    number: u64,
    status: crate::model::Status,
    progress: (usize, usize),
}

pub(crate) fn view(cx: &ViewCx<Tracker>, nav: &Nav) -> Element {
    let chosen = nav.filter.clone();
    let me = cx.me().map(str::to_owned);
    let nav = nav.clone();
    let list = cx.switch(&chosen, move |cx, chosen| {
        let filter = chosen.as_deref().and_then(Filter::of).unwrap_or_default();
        let tracker = cx.value(Tracker::clone);
        let nav = nav.clone();
        let me = me.clone();
        cx.switch(&tracker, move |cx, tracker| {
            tree(cx, tracker, &nav, &filter, me.as_deref())
        })
    });
    // The list's name is the window's location: the bar says where the
    // window is, so nothing here titles it a second time.
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .with_fill_remaining_height()
            .push(crate::new_issue(cx, None, None, "tasks-list-new"))
            .push(list),
    )
}

fn label(issue: &Issue) -> String {
    format!("{}  {}", reference(issue.number), issue.title)
}

/// The rows under `parent`, each followed by its own: only what the filter
/// shows, and the issues above it so the tree stays whole.
fn collect(
    tracker: &Tracker,
    parent: Option<&Id>,
    parent_key: Option<u64>,
    ids: &mut Vec<Id>,
    out: &mut Vec<Row>,
    filter: &Filter,
    me: Option<&str>,
) {
    for (id, issue) in tracker
        .children(parent)
        .filter(|(id, issue)| filter.shows(tracker, id, issue, me))
    {
        ids.push(id.clone());
        let key = u64::try_from(ids.len()).unwrap_or(u64::MAX);
        out.push(Row {
            key,
            issue: id.clone(),
            label: label(issue),
            parent: parent_key,
            number: issue.number,
            status: issue.status,
            progress: tracker.progress(id),
        });
        collect(tracker, Some(id), Some(key), ids, out, filter, me);
    }
}

fn tree(
    cx: &ViewCx<Tracker>,
    tracker: &Tracker,
    nav: &Nav,
    filter: &Filter,
    me: Option<&str>,
) -> Element {
    if tracker.issues.is_empty() {
        return Box::new(
            EmptyState::new("tasks-empty-title")
                .with_message("tasks-empty-message")
                .with_id("tasks-list-empty"),
        );
    }
    let mut ids = Vec::new();
    let mut rows = Vec::new();
    collect(tracker, None, None, &mut ids, &mut rows, filter, me);
    if rows.is_empty() {
        // The tracker holds issues; this filter shows none of them.
        return Box::new(
            EmptyState::new("tasks-empty-filter-title")
                .with_message("tasks-empty-filter-message")
                .with_id("tasks-list-filter-empty"),
        );
    }
    list(rows, Rc::new(ids), tracker, nav, cx.dispatcher())
}

/// The list over `rows`: the flat rows stand as the tree their parents make,
/// and each opens its issue, ticks its box and moves under another.
/// What a row is about: the issue itself, the item of the tracker its row publishes, named as
/// the row reads ("#1 Write the launch notes").
fn issue_subject(resource: &str, label: &str) -> ui::shared_selection::SharedSelection {
    ui::shared_selection::SharedSelection::of_items([(resource, label)]).in_resource(
        ui::tilo_actions::ActionResource {
            id: resource.to_owned(),
            kind: ui::tilo_resource::items::item_kind(resource)
                .unwrap_or_else(|| crate::model::KIND.to_owned()),
            label: label.to_owned(),
        },
    )
}

fn list(
    rows: Vec<Row>,
    ids: Rc<Vec<Id>>,
    tracker: &Tracker,
    nav: &Nav,
    dispatcher: ui::prelude::Dispatcher<Tracker>,
) -> Element {
    let issue_of = move |key: u64| -> Option<Id> {
        usize::try_from(key)
            .ok()
            .and_then(|key| key.checked_sub(1))
            .and_then(|index| ids.get(index).cloned())
    };
    let open_issue = issue_of.clone();
    let opener = nav.clone();
    let check = tracker.clone();
    let validate_issue = issue_of.clone();
    let parents = tracker.clone();
    // The row's own controls: the box that finishes an issue and the menu a
    // right click, the menu key or its more button opens. Both reach the
    // tracker through the same actions the board and the issue page use.
    let boxes = Rc::new(RefCell::new(row_check(
        issue_of.clone(),
        tracker.clone(),
        dispatcher.clone(),
    )));
    let menu_chosen = row_menu_callback(
        issue_of.clone(),
        nav.clone(),
        tracker.clone(),
        dispatcher.clone(),
    );
    // What each row is about, by its key: the issue itself, the item of the tracker its row
    // publishes, named as the row reads ("#1 Write the launch notes"), so the menu opened on a row
    // carries that issue as its subject and what it sends names it.
    let subjects: Rc<Vec<(String, String)>> = Rc::new(
        rows.iter()
            .map(|row| (crate::model::issue_resource(&row.issue), row.label.clone()))
            .collect(),
    );
    let row_about = {
        let subjects = Rc::clone(&subjects);
        move |key: &u64| {
            let index = usize::try_from(*key).ok()?.checked_sub(1)?;
            let (resource, label) = subjects.get(index)?;
            Some(issue_subject(resource, label))
        }
    };
    let Ok(model) =
        VecListModel::new(rows, |row| row.key).map(|model| model.with_parents(|row| row.parent))
    else {
        return Box::new(Spacer::token(ui::tokens::SpacingStep::S0));
    };
    // Every issue shows its sub-issues: the list starts unfolded, as the
    // tree it replaced did.
    let marks = Rc::new(TreeMarks::default());
    let depths = Rc::clone(&marks);
    let checked = Rc::clone(&boxes);
    Box::new(
        VirtualList::new(
            model,
            ui::tokens::Metrics::BASE.panel_row_height,
            move |model, index, key| row(model, index, *key, &depths, &checked),
        )
        .with_id("tasks-list")
        .with_accessible_name(tr("tasks-place-list"))
        .with_tree(marks)
        .with_semantic_key(ToString::to_string)
        .filling()
        .open_on_press()
        .selection_follows_focus()
        .with_context_menu(row_menu, menu_chosen)
        .with_reorder(
            move |_, source, target| {
                let (Some(source), Some(target)) =
                    (validate_issue(*source), validate_issue(target.key))
                else {
                    return false;
                };
                !check.is_under(&target, &source)
            },
            move |_, dropped: ReorderDrop<u64>| {
                let (Some(source), Some(target)) =
                    (issue_of(dropped.source), issue_of(dropped.target.key))
                else {
                    return false;
                };
                let parent = match dropped.target.placement {
                    DropPlacement::Inside => Some(target),
                    DropPlacement::Before | DropPlacement::After => parents
                        .issues
                        .get(&target)
                        .and_then(|issue| issue.parent.clone()),
                };
                dispatcher.submit(Tracker::MOVE_UNDER.with(tracker::MoveUnder {
                    issue: source,
                    parent,
                }));
                // The tracker answers with the moved issue; the list is
                // built again from it.
                false
            },
        )
        .on_activate_view("tasks-list", move |key| {
            if let Some(issue) = open_issue(key) {
                opener.open(&issue);
            }
        })
        .with_row_about(row_about),
    )
}

/// One row: the issue's reference and title, indented under its parent, with
/// the box that finishes it and its status beside them.
fn row(
    model: &VecListModel<Row, u64>,
    index: usize,
    key: u64,
    depths: &TreeMarks<u64>,
    boxes: &Rc<RefCell<impl FnMut(u64) + 'static>>,
) -> Element {
    let Some(row) = model.item(index) else {
        return Box::new(ListRow::new(format!("key:{key}"), ""));
    };
    let toggle = {
        let boxes = Rc::clone(boxes);
        move |_checked: bool| (boxes.borrow_mut())(key)
    };
    let status = status_text(row);
    let closed = row.status.closed();
    Box::new(
        ListRow::new(format!("key:{key}"), row.label.as_str())
            .item(row.issue.as_str())
            .depth(depths.depth(&key))
            .checkbox(closed, toggle)
            .checked(closed)
            .value(status.as_str())
            .description(format!("{}, {status}", reference(row.number))),
    )
}

/// A row's status beside its label, as a reader hears it too.
fn status_text(row: &Row) -> String {
    let mut status = tr(row.status.label_key());
    if row.progress.1 > 0 {
        status = crate::locales::CATALOG.t_args(
            "tasks-status-with-progress",
            &[
                ("status", status.into()),
                ("closed", row.progress.0.into()),
                ("total", row.progress.1.into()),
            ],
        );
    }
    status
}

/// The row's box: ticking it finishes the issue, unticking reopens it.
fn row_check(
    issue_of: impl Fn(u64) -> Option<Id> + 'static,
    tracker: Tracker,
    dispatcher: ui::prelude::Dispatcher<Tracker>,
) -> impl FnMut(u64) + 'static {
    move |key| {
        let Some(issue) = issue_of(key) else {
            return;
        };
        let Some(status) = tracker.issues.get(&issue).map(|now| {
            if now.status.closed() {
                Status::Todo
            } else {
                Status::Done
            }
        }) else {
            return;
        };
        change_status(&tracker, &dispatcher, issue, status);
    }
}

/// What a row's menu reports, once the reader chooses an item.
fn row_menu_callback(
    issue_of: impl Fn(u64) -> Option<Id> + 'static,
    nav: Nav,
    tracker: Tracker,
    dispatcher: ui::prelude::Dispatcher<Tracker>,
) -> impl FnMut(Option<u64>, usize) + 'static {
    move |target, index| {
        if let Some(issue) = target.and_then(&issue_of) {
            row_menu_chosen(&tracker, &nav, &dispatcher, issue, index);
        }
    }
}

/// Set `issue`'s status as the row's box and menu do: a place in the list
/// that keeps the issue where it is.
fn change_status(
    tracker: &Tracker,
    dispatcher: &ui::prelude::Dispatcher<Tracker>,
    issue: Id,
    status: Status,
) {
    // Finishing an issue is not a move: it keeps the place it had in the
    // list, before the issue that follows it.
    let before = tracker
        .issues
        .iter()
        .map(|(id, _)| id.clone())
        .skip_while(|id| id != &issue)
        .nth(1);
    dispatcher.submit(Tracker::PLACE.with(tracker::Place {
        issue,
        status,
        before,
    }));
}

/// A row's menu: open it, and finish or reopen it.
fn row_menu(model: &VecListModel<Row, u64>, target: Option<&u64>) -> Vec<MenuItem> {
    let Some(row) = target
        .and_then(|key| model.index_of(key))
        .and_then(|index| model.item(index))
    else {
        return Vec::new();
    };
    vec![
        MenuItem::icon_action(ui::icons::Icon::ArrowUpRight, tr("tasks-row-open"), || {}),
        if row.status.closed() {
            MenuItem::icon_action(ui::icons::Icon::RotateCcw, tr("tasks-row-reopen"), || {})
        } else {
            MenuItem::icon_action(
                ui::icons::Icon::CircleCheck,
                tr("tasks-row-complete"),
                || {},
            )
        },
    ]
}

/// What a row's menu item does, by the order [`row_menu`] lists them.
fn row_menu_chosen(
    tracker: &Tracker,
    nav: &Nav,
    dispatcher: &ui::prelude::Dispatcher<Tracker>,
    issue: Id,
    index: usize,
) {
    let Some(now) = tracker.issues.get(&issue) else {
        return;
    };
    let status = match (index, now.status.closed()) {
        (0, _) => {
            nav.open(&issue);
            return;
        }
        (1, false) => Status::Done,
        (1, true) => Status::Todo,
        _ => return,
    };
    change_status(tracker, dispatcher, issue, status);
}
