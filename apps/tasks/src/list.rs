//! The list: every issue the sidebar's filter shows, sub-issues nested under
//! their parent.
//!
//! Each row reads as the issue's reference and title, with its status
//! beside it. Choosing a row opens the issue; dragging one onto another
//! makes it a sub-issue there, the tracker's `move-under`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::model::{Issue, Status, Tracker};
use crate::part::{IssuePart, issue_part};
use ui::prelude::*;
use ui::reorder::{DropPlacement, ReorderDrop};
use ui::widget::context_menu::MenuItem;
use ui::widget::{ListRow, TreeMarks, VecListModel, VirtualList};

use crate::{Filter, Issues, Nav, Tasks, Window, reference, tr};

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

pub(crate) fn view(cx: &ViewCx<Tracker>, nav: &Nav, window: &Window) -> Element {
    let chosen = nav.filter.clone();
    let me = cx.me().cloned();
    let nav = nav.clone();
    let shown = window.clone();
    let list = cx.switch(&chosen, move |cx, chosen| {
        let filter = chosen.as_deref().and_then(Filter::of).unwrap_or_default();
        let nav = nav.clone();
        let me = me.clone();
        let window = shown.clone();
        cx.switch(&shown.tasks, move |_cx, tasks| {
            tree(tasks, &nav, &window, &filter, me.as_ref())
        })
    });
    // The list's name is the window's location: the bar says where the
    // window is, so nothing here titles it a second time.
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .with_fill_remaining_height()
            .push(crate::new_issue(cx, window, None, None, "tasks-list-new"))
            .push(list),
    )
}

fn label(issue: &Issue) -> String {
    format!("{}  {}", reference(issue.number), issue.title)
}

/// The rows under `parent`, each followed by its own: only what the filter
/// shows, and the issues above it so the tree stays whole.
fn collect(
    issues: &Issues<'_>,
    parent: Option<&Id>,
    parent_key: Option<u64>,
    ids: &mut Vec<Id>,
    out: &mut Vec<Row>,
    filter: &Filter,
    me: Option<&ui::tilo_resource::MemberId>,
) {
    for (id, issue) in issues
        .children(parent)
        .filter(|(id, issue)| filter.shows(issues, id, issue, me))
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
            progress: issues.progress(id),
        });
        collect(issues, Some(id), Some(key), ids, out, filter, me);
    }
}

fn tree(tasks: &Tasks, nav: &Nav, window: &Window, filter: &Filter, me: Option<&ui::tilo_resource::MemberId>) -> Element {
    let issues = tasks.issues();
    if issues.is_empty() {
        return Box::new(
            EmptyState::new("tasks-empty-title")
                .with_message("tasks-empty-message")
                .with_id("tasks-list-empty"),
        );
    }
    let mut ids = Vec::new();
    let mut rows = Vec::new();
    collect(&issues, None, None, &mut ids, &mut rows, filter, me);
    if rows.is_empty() {
        // The tracker holds issues; this filter shows none of them.
        return Box::new(
            EmptyState::new("tasks-empty-filter-title")
                .with_message("tasks-empty-filter-message")
                .with_id("tasks-list-filter-empty"),
        );
    }
    list(rows, Rc::new(ids), tasks, nav, window)
}

/// The list over `rows`: the flat rows stand as the tree their parents make,
/// and each opens its issue, ticks its box and moves under another.
fn list(rows: Vec<Row>, ids: Rc<Vec<Id>>, tasks: &Tasks, nav: &Nav, window: &Window) -> Element {
    let issue_of = move |key: u64| -> Option<Id> {
        usize::try_from(key)
            .ok()
            .and_then(|key| key.checked_sub(1))
            .and_then(|index| ids.get(index).cloned())
    };
    let open_issue = issue_of.clone();
    let opener = nav.clone();
    let check = tasks.clone();
    let validate_issue = issue_of.clone();
    let parents = tasks.clone();
    // The row's own controls: the box that finishes an issue and the menu a
    // right click, the menu key or its more button opens. Both reach the
    // tracker through the same actions the board and the issue page use.
    let boxes = Rc::new(RefCell::new(row_check(
        issue_of.clone(),
        tasks.clone(),
        window.clone(),
    )));
    let menu_chosen =
        row_menu_callback(issue_of.clone(), nav.clone(), tasks.clone(), window.clone());
    let moving = window.clone();
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
                !check.issues().is_under(&target, &source)
            },
            move |_, dropped: ReorderDrop<u64>| {
                let (Some(source), Some(target)) =
                    (issue_of(dropped.source), issue_of(dropped.target.key))
                else {
                    return false;
                };
                let parent = match dropped.target.placement {
                    DropPlacement::Inside => Some(target),
                    DropPlacement::Before | DropPlacement::After => {
                        let issues = parents.issues();
                        issues
                            .get(&target)
                            .and_then(|issue| issues.parent_of(issue).cloned())
                    }
                };
                moving.submit(IssuePart::MOVE_UNDER.with(issue_part::MoveUnder {
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
        }),
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
    tasks: Tasks,
    window: Window,
) -> impl FnMut(u64) + 'static {
    move |key| {
        let Some(issue) = issue_of(key) else {
            return;
        };
        let Some(status) = tasks.issues().get(&issue).map(|now| {
            if now.status.closed() {
                Status::Todo
            } else {
                Status::Done
            }
        }) else {
            return;
        };
        change_status(&tasks, &window, issue, status);
    }
}

/// What a row's menu reports, once the reader chooses an item.
fn row_menu_callback(
    issue_of: impl Fn(u64) -> Option<Id> + 'static,
    nav: Nav,
    tasks: Tasks,
    window: Window,
) -> impl FnMut(Option<u64>, usize) + 'static {
    move |target, index| {
        if let Some(issue) = target.and_then(&issue_of) {
            row_menu_chosen(&tasks, &nav, &window, issue, index);
        }
    }
}

/// Set `issue`'s status as the row's box and menu do: a place in the list
/// that keeps the issue where it is.
fn change_status(tasks: &Tasks, window: &Window, issue: Id, status: Status) {
    // Finishing an issue is not a move: it keeps the place it had in the
    // list, before the issue that follows it.
    let before = tasks
        .issues()
        .iter()
        .map(|(id, _)| id.clone())
        .skip_while(|id| id != &issue)
        .nth(1);
    window.submit(IssuePart::PLACE.with(issue_part::Place {
        issue,
        status,
        before,
        rank: None,
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
fn row_menu_chosen(tasks: &Tasks, nav: &Nav, window: &Window, issue: Id, index: usize) {
    let Some(closed) = tasks.issues().get(&issue).map(|now| now.status.closed()) else {
        return;
    };
    let status = match (index, closed) {
        (0, _) => {
            nav.open(&issue);
            return;
        }
        (1, false) => Status::Done,
        (1, true) => Status::Todo,
        _ => return,
    };
    change_status(tasks, window, issue, status);
}
