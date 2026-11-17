//! The list: every issue the sidebar's filter shows, sub-issues nested under
//! their parent.
//!
//! Each row reads as the issue's reference and title, with its status
//! beside it. Choosing a row opens the issue; dragging one onto another
//! makes it a sub-issue there, the tracker's `move-under`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::model::{Issue, Status, Tracker};
use crate::part::{IssuePart, issue_part};
use ui::prelude::*;
use ui::reorder::{DropPlacement, ReorderDrop};
use ui::tilo_resource::MemberId;
use ui::widget::context_menu::MenuItem;
use ui::widget::virtual_list::{ListModel, ListModelChange};
use ui::widget::{ListRow, TreeMarks, TreeRow, VirtualList};

use crate::{Filter, Issues, Nav, Tasks, Window, reference, tr};

/// What the list shows: the tracker's issues, or why it shows none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Shape {
    /// The tracker holds no issue.
    #[default]
    Empty,
    /// The tracker holds issues; the filter shows none of them.
    FilteredOut,
    Rows,
}

pub(crate) fn view(cx: &ViewCx<Tracker>, nav: &Nav, window: &Window) -> Element {
    let chosen = nav.filter.clone();
    let me = cx.me().cloned();
    let nav = nav.clone();
    let shown = window.clone();
    let list = cx.switch(&chosen, move |cx, chosen| {
        let filter = chosen.as_deref().and_then(Filter::of).unwrap_or_default();
        let shape = shape(cx, &shown, &filter, me.clone());
        let nav = nav.clone();
        let me = me.clone();
        let window = shown.clone();
        // The list is built once for the filter: as issues come and go it is
        // told which rows moved, and builds only those it shows.
        cx.switch(&shape, move |_cx, shape| match shape {
            Shape::Empty => Box::new(
                EmptyState::new("tasks-empty-title")
                    .with_message("tasks-empty-message")
                    .with_id("tasks-list-empty"),
            ),
            Shape::FilteredOut => Box::new(
                EmptyState::new("tasks-empty-filter-title")
                    .with_message("tasks-empty-filter-message")
                    .with_id("tasks-list-filter-empty"),
            ),
            Shape::Rows => list(&window, &nav, filter.clone(), me.clone()),
        })
    });
    // The list's name is the window's location: the bar says where the
    // window is, so nothing here titles it a second time.
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .with_fill_remaining_height()
            .push(crate::new_issue(
                &|| cx.local(String::new()),
                window,
                None,
                None,
                "tasks-list-new",
            ))
            .push(list),
    )
}

/// Whether the list shows rows under `filter`, read from what the issues tally.
fn shape(
    cx: &ViewCx<Tracker>,
    window: &Window,
    filter: &Filter,
    me: Option<MemberId>,
) -> LocalState<Shape> {
    let filter = filter.clone();
    let read = move |issues: &Issues| {
        if issues.is_empty() {
            Shape::Empty
        } else if filter.count(issues, me.as_ref()) == 0 {
            Shape::FilteredOut
        } else {
            Shape::Rows
        }
    };
    match &window.chain {
        Some(chain) => {
            let index = Rc::clone(&window.index);
            cx.chain_value(chain, move |_, parts| {
                read(&crate::read_issues(&index, parts))
            })
        }
        // A past version shows the tracker's projects and no issues.
        None => cx.value(|_| Shape::Empty),
    }
}

/// The rows the list shows, by issue number, in the order the tree reads:
/// each issue followed by its sub-issues, only what the filter shows and the
/// issues above it so the tree stays whole. Read again from the window's
/// issues as they change, and told to the list as the rows that moved.
struct IssueRows {
    tasks: LocalState<Tasks>,
    filter: Filter,
    me: Option<MemberId>,
    rows: Vec<u64>,
    parents: Vec<Option<u64>>,
    at: HashMap<u64, usize>,
    /// The window state's revision and the issues' revision last read.
    read: (u64, u64),
    changes: Vec<ListModelChange<u64>>,
}

impl IssueRows {
    fn new(tasks: LocalState<Tasks>, filter: Filter, me: Option<MemberId>) -> Self {
        let mut rows = Self {
            tasks,
            filter,
            me,
            rows: Vec::new(),
            parents: Vec::new(),
            at: HashMap::new(),
            read: (u64::MAX, u64::MAX),
            changes: Vec::new(),
        };
        rows.refresh();
        rows.changes.clear();
        rows
    }

    /// Read the issues again if they moved, and note which rows did.
    fn refresh(&mut self) {
        let state = self.tasks.revision();
        if state == self.read.0 {
            return;
        }
        let _profile = ui::tilo_resource::profiling::Phase::start("tasks.list_rows");
        let tasks = self.tasks.get();
        let issues = tasks.issues();
        let revision = issues.revision();
        let before = std::mem::replace(&mut self.read, (state, revision));
        if before.1 == revision && !self.rows.is_empty() {
            return;
        }
        let mut rows = Vec::with_capacity(self.rows.len().saturating_add(1));
        let mut parents = Vec::with_capacity(rows.capacity());
        walk(
            &issues,
            &self.filter,
            self.me.as_ref(),
            None,
            None,
            &mut rows,
            &mut parents,
        );
        let touched = if before.0 == u64::MAX {
            None
        } else {
            issues.touched_since(before.1)
        };
        drop(issues);
        let Some(touched) = touched else {
            self.rows = rows;
            self.parents = parents;
            self.index_rows();
            self.changes.push(ListModelChange::Reset);
            return;
        };
        self.take_rows(rows, parents, touched);
    }

    /// Hold `rows` under `parents` in place of the rows held, and note the
    /// rows that went, came, or show something new (`touched` and those that
    /// stand under another parent now).
    fn take_rows(&mut self, rows: Vec<u64>, parents: Vec<Option<u64>>, touched: Vec<u64>) {
        // What stayed in place before and after the rows that moved.
        let same_start = self
            .rows
            .iter()
            .zip(&rows)
            .take_while(|(was, now)| was == now)
            .count();
        let (was_head, was_tail) = self.rows.split_at(same_start.min(self.rows.len()));
        let (now_head, now_tail) = rows.split_at(same_start.min(rows.len()));
        let same_end = was_tail
            .iter()
            .rev()
            .zip(now_tail.iter().rev())
            .take_while(|(was, now)| was == now)
            .count();
        let removed = was_tail.len().saturating_sub(same_end);
        let inserted = now_tail.len().saturating_sub(same_end);
        // Rows that stayed but stand under another parent now, or show
        // something new, are drawn again.
        let mut changed: Vec<u64> = touched;
        let kept = |head: &[u64], tail: &[u64], parents: &[Option<u64>]| {
            let (head_parents, rest) = parents
                .split_at_checked(head.len())
                .unwrap_or((parents, &[]));
            let tail_parents = rest
                .get(rest.len().saturating_sub(same_end)..)
                .unwrap_or_default();
            let tail_rows = tail
                .get(tail.len().saturating_sub(same_end)..)
                .unwrap_or_default();
            head.iter()
                .zip(head_parents)
                .chain(tail_rows.iter().zip(tail_parents))
                .map(|(key, parent)| (*key, *parent))
                .collect::<Vec<_>>()
        };
        let kept_before = kept(was_head, was_tail, &self.parents);
        let kept_now = kept(now_head, now_tail, &parents);
        changed.extend(
            kept_before
                .into_iter()
                .zip(kept_now)
                .filter(|((_, was), (_, now))| was != now)
                .map(|(_, (key, _))| key),
        );
        self.rows = rows;
        self.parents = parents;
        if removed > 0 || inserted > 0 {
            self.index_rows();
        }
        if removed > 0 {
            self.changes.push(ListModelChange::RowsRemoved {
                index: same_start,
                count: removed,
            });
        }
        if inserted > 0 {
            self.changes.push(ListModelChange::RowsInserted {
                index: same_start,
                count: inserted,
            });
        }
        changed.retain(|key| self.at.contains_key(key));
        changed.sort_unstable();
        changed.dedup();
        if !changed.is_empty() {
            self.changes
                .push(ListModelChange::RowsChanged { keys: changed });
        }
    }

    fn index_rows(&mut self) {
        self.at = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, key)| (*key, index))
            .collect();
    }

    /// The issue numbered `key`, as the window reads it now.
    fn issue<T>(&self, key: u64, read: impl FnOnce(&Issues, &Id, &Issue) -> T) -> Option<T> {
        self.tasks.with(|tasks| {
            let issues = tasks.issues();
            let (id, issue) = issues.by_number(key)?;
            Some(read(&issues, id, issue))
        })
    }
}

/// The rows under `parent`, each followed by its own, as [`IssueRows`] reads
/// them; whether any of them shows. An issue shows when it matches the filter
/// or something under it does.
fn walk(
    issues: &Issues,
    filter: &Filter,
    me: Option<&MemberId>,
    parent: Option<&Id>,
    parent_key: Option<u64>,
    rows: &mut Vec<u64>,
    parents: &mut Vec<Option<u64>>,
) -> bool {
    let mut any = false;
    for (id, issue) in issues.children(parent) {
        let at = rows.len();
        rows.push(issue.number);
        parents.push(parent_key);
        let below = walk(
            issues,
            filter,
            me,
            Some(id),
            Some(issue.number),
            rows,
            parents,
        );
        if below || matches!(filter, Filter::All) || filter.matches(issue, me) {
            any = true;
        } else {
            rows.truncate(at);
            parents.truncate(at);
        }
    }
    any
}

impl ListModel for IssueRows {
    type Key = u64;

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        self.rows.get(index).copied()
    }

    fn index_of(&self, key: &u64) -> Option<usize> {
        self.at.get(key).copied()
    }

    fn tree_at(&self, index: usize) -> Option<TreeRow<u64>> {
        Some(TreeRow {
            parent: self.parents.get(index).copied().flatten(),
            depth: None,
            above: None,
        })
    }

    fn poll_changes(&mut self) -> Vec<ListModelChange<u64>> {
        self.refresh();
        std::mem::take(&mut self.changes)
    }

    fn needs_poll(&self) -> bool {
        !self.changes.is_empty() || self.tasks.revision() != self.read.0
    }
}

/// The issue numbered `key`, as the window reads it now.
fn issue_of(window: &Window, key: u64) -> Option<Id> {
    window
        .tasks
        .with(|tasks| tasks.issues().by_number(key).map(|(id, _)| id.clone()))
}

/// The list of the issues `filter` shows: the rows stand as the tree their
/// parents make, and each opens its issue, ticks its box and moves under
/// another.
fn list(window: &Window, nav: &Nav, filter: Filter, me: Option<MemberId>) -> Element {
    let model = IssueRows::new(window.tasks.clone(), filter, me);
    let opener = nav.clone();
    let opening = window.clone();
    let checking = window.clone();
    let moving = window.clone();
    // The row's own controls: the box that finishes an issue and the menu a
    // right click, the menu key or its more button opens. Both reach the
    // tracker through the same actions the board and the issue page use.
    let boxes = Rc::new(RefCell::new(row_check(window.clone())));
    let menu_chosen = row_menu_callback(nav.clone(), window.clone());
    // Every issue shows its sub-issues: the list starts unfolded, as the
    // tree it replaced did.
    let marks = Rc::new(TreeMarks::default());
    let depths = Rc::clone(&marks);
    let checked = Rc::clone(&boxes);
    Box::new(
        VirtualList::new(
            model,
            ui::tokens::Metrics::BASE.panel_row_height,
            move |model, _, key| row(model, *key, &depths, &checked),
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
                let (Some(source), Some(target)) = (
                    issue_of(&checking, *source),
                    issue_of(&checking, target.key),
                ) else {
                    return false;
                };
                !checking
                    .tasks
                    .with(|tasks| tasks.issues().is_under(&target, &source))
            },
            move |_, dropped: ReorderDrop<u64>| {
                let (Some(source), Some(target)) = (
                    issue_of(&moving, dropped.source),
                    issue_of(&moving, dropped.target.key),
                ) else {
                    return false;
                };
                let parent = match dropped.target.placement {
                    DropPlacement::Inside => Some(target),
                    DropPlacement::Before | DropPlacement::After => moving.tasks.with(|tasks| {
                        let issues = tasks.issues();
                        issues
                            .get(&target)
                            .and_then(|issue| issues.parent_of(issue).cloned())
                    }),
                };
                moving.submit(IssuePart::MOVE_UNDER.with(issue_part::MoveUnder {
                    issue: source,
                    parent: parent.map(ui::tilo_resource::items::Ref::from),
                }));
                // The tracker answers with the moved issue; the list is
                // told which rows moved.
                false
            },
        )
        .on_activate_view("tasks-list", move |key| {
            if let Some(issue) = issue_of(&opening, key) {
                opener.open(&issue);
            }
        }),
    )
}

/// One row: the issue's reference and title, indented under its parent, with
/// the box that finishes it and its status beside them.
fn row(
    model: &IssueRows,
    key: u64,
    depths: &TreeMarks<u64>,
    boxes: &Rc<RefCell<impl FnMut(u64) + 'static>>,
) -> Element {
    let shown = model.issue(key, |issues, id, issue| {
        (
            id.clone(),
            issue.title.clone(),
            issue.status,
            issues.progress(id),
        )
    });
    let Some((id, title, status, progress)) = shown else {
        return Box::new(ListRow::new(format!("key:{key}"), ""));
    };
    let toggle = {
        let boxes = Rc::clone(boxes);
        move |_checked: bool| (boxes.borrow_mut())(key)
    };
    let said = status_text(status, progress);
    let closed = status.closed();
    Box::new(
        // The reference stands before the title, apart from it: the title is the issue's own
        // `title`, where an agent's rename is drawn as it lands.
        ListRow::new(format!("key:{key}"), title.as_str())
            .prefix(reference(key))
            .item(id.as_str())
            .edits("title")
            .depth(depths.depth(&key))
            .checkbox(closed, toggle)
            .checked(closed)
            .value(said.as_str())
            .description(format!("{}, {said}", reference(key))),
    )
}

/// A row's status beside its label, as a reader hears it too.
fn status_text(status: Status, progress: (usize, usize)) -> String {
    let mut said = tr(status.label_key());
    if progress.1 > 0 {
        said = crate::locales::CATALOG.t_args(
            "tasks-status-with-progress",
            &[
                ("status", said.into()),
                ("closed", progress.0.into()),
                ("total", progress.1.into()),
            ],
        );
    }
    said
}

/// The row's box: ticking it finishes the issue, unticking reopens it.
fn row_check(window: Window) -> impl FnMut(u64) + 'static {
    move |key| {
        let Some(issue) = issue_of(&window, key) else {
            return;
        };
        let Some(status) = window.tasks.with(|tasks| {
            tasks.issues().get(&issue).map(|now| {
                if now.status.closed() {
                    Status::Todo
                } else {
                    Status::Done
                }
            })
        }) else {
            return;
        };
        change_status(&window, issue, status);
    }
}

/// What a row's menu reports, once the reader chooses an item.
fn row_menu_callback(nav: Nav, window: Window) -> impl FnMut(Option<u64>, usize) + 'static {
    move |target, index| {
        if let Some(issue) = target.and_then(|key| issue_of(&window, key)) {
            row_menu_chosen(&nav, &window, issue, index);
        }
    }
}

/// Set `issue`'s status as the row's box and menu do: a place in the list
/// that keeps the issue where it is.
fn change_status(window: &Window, issue: Id, status: Status) {
    // Finishing an issue is not a move: it keeps the place it had in the
    // list, before the issue that follows it.
    let before = window
        .tasks
        .with(|tasks| tasks.issues().after(&issue).cloned());
    window.submit(IssuePart::PLACE.with(issue_part::Place {
        issue,
        status,
        before: before.map(ui::tilo_resource::items::Ref::from),
        rank: None,
    }));
}

/// A row's menu: open it, and finish or reopen it.
fn row_menu(model: &IssueRows, target: Option<&u64>) -> Vec<MenuItem> {
    let Some(closed) =
        target.and_then(|key| model.issue(*key, |_, _, issue| issue.status.closed()))
    else {
        return Vec::new();
    };
    vec![
        MenuItem::icon_action(ui::icons::Icon::ArrowUpRight, tr("tasks-row-open"), || {}),
        if closed {
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
fn row_menu_chosen(nav: &Nav, window: &Window, issue: Id, index: usize) {
    let Some(closed) = window
        .tasks
        .with(|tasks| tasks.issues().get(&issue).map(|now| now.status.closed()))
    else {
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
    change_status(window, issue, status);
}
