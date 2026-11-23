//! The list: every issue the sidebar's filter shows, sub-issues nested under
//! their parent.
//!
//! Each row reads as the issue's reference and title, with its status
//! beside it. Choosing a row opens the issue; dragging one onto another
//! makes it a sub-issue there, the tracker's `move-under`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::issue_list::{IssueList, issue_list};
use crate::model::{Issue, Status, Tracker};
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

/// Rows read: each row's issue number, its parent's, and the blocks they
/// stand in ([`IssueRows::blocks`]).
type RowsRead = (Vec<u64>, Vec<Option<u64>>, Vec<(u64, usize)>);

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
    /// The rows in blocks, one for each top-level issue in the tracker's
    /// order: its number, and how many rows it and what shows under it hold
    /// (none when the filter shows nothing of it). A change reads again only
    /// the blocks of the top-level issues it touches.
    blocks: Vec<(u64, usize)>,
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
            blocks: Vec::new(),
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
        let touched = if before.0 == u64::MAX {
            None
        } else {
            issues.touched_since(before.1)
        };
        let (rows, parents, blocks) = touched
            .as_ref()
            .and_then(|touched| self.read_touched(&issues, touched))
            .unwrap_or_else(|| self.read_all(&issues));
        self.blocks = blocks;
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

    /// Every row, read block by block from the top-level issues.
    fn read_all(&self, issues: &Issues) -> RowsRead {
        let mut rows = Vec::with_capacity(self.rows.len().saturating_add(1));
        let mut parents = Vec::with_capacity(rows.capacity());
        let mut blocks = Vec::new();
        for (id, issue) in issues.children(None) {
            let start = rows.len();
            self.read_block(issues, id, issue, &mut rows, &mut parents);
            blocks.push((issue.number, rows.len().saturating_sub(start)));
        }
        (rows, parents, blocks)
    }

    /// The rows of the top-level issue `id`: it and what shows under it.
    fn read_block(
        &self,
        issues: &Issues,
        id: &Id,
        issue: &Issue,
        rows: &mut Vec<u64>,
        parents: &mut Vec<Option<u64>>,
    ) {
        let at = rows.len();
        rows.push(issue.number);
        parents.push(None);
        let below = walk(
            issues,
            &self.filter,
            self.me.as_ref(),
            Some(id),
            Some(issue.number),
            rows,
            parents,
        );
        if !(below
            || matches!(self.filter, Filter::All)
            || self.filter.matches(issue, self.me.as_ref()))
        {
            rows.truncate(at);
            parents.truncate(at);
        }
    }

    /// The rows held with only the blocks the `touched` issues stand in read
    /// again: the block each stood in, and the block it stands in now. The
    /// top-level issues are walked once in the tracker's order, each block
    /// kept or read again where it stands, so a change costs one pass over
    /// the blocks however many top-level issues it made: an import's list of
    /// hundreds lands as one change. `None` when the rows held cannot be
    /// sliced, and every row is read instead.
    fn read_touched(&self, issues: &Issues, touched: &[u64]) -> Option<RowsRead> {
        let mut held_at: HashMap<u64, (usize, usize)> = HashMap::with_capacity(self.blocks.len());
        let mut starts = Vec::with_capacity(self.blocks.len());
        let mut at = 0_usize;
        for (top, len) in &self.blocks {
            held_at.insert(*top, (at, *len));
            starts.push(at);
            at = at.saturating_add(*len);
        }
        let mut stale = std::collections::HashSet::new();
        for number in touched {
            if let Some(&row) = self.at.get(number)
                && let Some(block) = starts.partition_point(|start| *start <= row).checked_sub(1)
                && let Some((top, _)) = self.blocks.get(block)
            {
                stale.insert(*top);
            }
            stale.insert(*number);
            if let Some(top) = issues.top_of(*number) {
                stale.insert(top);
            }
        }
        let mut rows = Vec::with_capacity(self.rows.len().saturating_add(1));
        let mut parents = Vec::with_capacity(rows.capacity());
        let mut blocks = Vec::with_capacity(self.blocks.len().saturating_add(1));
        for (id, issue) in issues.children(None) {
            let start = rows.len();
            match held_at.get(&issue.number) {
                Some(&(from, len)) if !stale.contains(&issue.number) => {
                    let to = from.saturating_add(len);
                    rows.extend_from_slice(self.rows.get(from..to)?);
                    parents.extend_from_slice(self.parents.get(from..to)?);
                }
                _ => self.read_block(issues, id, issue, &mut rows, &mut parents),
            }
            blocks.push((issue.number, rows.len().saturating_sub(start)));
        }
        Some((rows, parents, blocks))
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
        let same_start = same_start.min(self.rows.len()).min(rows.len());
        let same_end = self
            .rows
            .get(same_start..)
            .unwrap_or_default()
            .iter()
            .rev()
            .zip(rows.get(same_start..).unwrap_or_default().iter().rev())
            .take_while(|(was, now)| was == now)
            .count();
        let removed = self
            .rows
            .len()
            .saturating_sub(same_start)
            .saturating_sub(same_end);
        let inserted = rows
            .len()
            .saturating_sub(same_start)
            .saturating_sub(same_end);
        // Rows that stayed but stand under another parent now, or show
        // something new, are drawn again.
        let mut changed: Vec<u64> = touched;
        let moved_under = |was: usize, now: usize| self.parents.get(was) != parents.get(now);
        changed.extend(
            (0..same_start)
                .filter(|&row| moved_under(row, row))
                .filter_map(|row| rows.get(row).copied()),
        );
        changed.extend((1..=same_end).filter_map(|back| {
            let was = self.rows.len().checked_sub(back)?;
            let now = rows.len().checked_sub(back)?;
            moved_under(was, now)
                .then(|| rows.get(now).copied())
                .flatten()
        }));
        if removed > 0 || inserted > 0 {
            // The rows before the first that moved keep their places.
            let gone = same_start.saturating_add(removed);
            for key in self.rows.get(same_start..gone).unwrap_or_default() {
                self.at.remove(key);
            }
            self.rows = rows;
            self.parents = parents;
            let at = &mut self.at;
            for (index, key) in self.rows.iter().enumerate().skip(same_start) {
                at.insert(*key, index);
            }
        } else {
            self.rows = rows;
            self.parents = parents;
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
        VirtualList::with_selection_delegate(
            model,
            ui::tokens::Metrics::BASE.panel_row_height,
            move |model, _, key, build| {
                row(model, *key, build.selection.selected, &depths, &checked)
            },
        )
        .with_id("tasks-list")
        .with_accessible_name(tr("tasks-place-list"))
        .with_tree(marks)
        .with_semantic_key(ToString::to_string)
        // The issue a press made is shown: the list scrolls to it and marks it chosen.
        .reveal_when(&nav.made)
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
                moving.submit(IssueList::MOVE_UNDER.with(issue_list::MoveUnder {
                    issue: source,
                    parent: parent.map(ui::tilo_resource::items::Ref::from),
                }));
                // The tracker answers with the moved issue; the list is
                // told which rows moved.
                false
            },
        )
        .on_activate(move |key| {
            if let Some(issue) = issue_of(&opening, key) {
                opener.open(&issue);
            }
        }),
    )
}

/// One row: the issue's reference and title, indented under its parent, with
/// the box that finishes it, its status beside them and the chosen row's face.
fn row(
    model: &IssueRows,
    key: u64,
    chosen: bool,
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
            .selected(chosen)
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
    window.submit(IssueList::PLACE.with(issue_list::Place {
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use ui::prelude::*;

    use super::IssueRows;
    use crate::issue_list::tests::Board;
    use crate::issue_list::{IssueList, Issues, issue_list};
    use crate::{Filter, Tasks};

    /// The window's state as `board`'s parts hold it now, read through `index`.
    fn tasks(board: &Board, index: &Rc<RefCell<Issues>>) -> Tasks {
        Tasks {
            tracker: Rc::new(board.tracker.state().unwrap()),
            parts: board.states().into(),
            index: Rc::clone(index),
        }
    }

    /// Read `model`'s issues again and check the rows it built block by block
    /// against the rows read from every issue.
    fn follow(model: &mut IssueRows, board: &Board, index: &Rc<RefCell<Issues>>, after: &str) {
        model.tasks.set(tasks(board, index));
        model.refresh();
        assert!(
            !model
                .changes
                .iter()
                .any(|change| matches!(change, ListModelChange::Reset)),
            "{after}: read from the issues it touched, not anew"
        );
        model.changes.clear();
        let state = model.tasks.get();
        let issues = state.issues();
        let (rows, parents, blocks) = model.read_all(&issues);
        assert_eq!(model.rows, rows, "{after}: the rows read whole");
        assert_eq!(
            model.parents, parents,
            "{after}: under the parents read whole"
        );
        assert_eq!(model.blocks, blocks, "{after}: in the blocks read whole");
        let places: std::collections::HashMap<u64, usize> = model
            .rows
            .iter()
            .enumerate()
            .map(|(index, key)| (*key, index))
            .collect();
        assert_eq!(model.at, places, "{after}: each row found where it stands");
    }

    #[test]
    fn the_rows_read_block_by_block_are_the_rows_read_whole() {
        for filter in [Filter::All, Filter::Todo] {
            let mut board = Board::new("ada");
            // Sixty issues, every tenth a parent of the nine after it.
            let mut ids = Vec::new();
            let mut parent = None;
            for number in 1..=60_u64 {
                let first = number % 10 == 1;
                let id = board.create(
                    &format!("Issue {number}"),
                    if first { None } else { parent.clone() },
                );
                if first {
                    parent = Some(id.clone());
                }
                ids.push(id);
            }
            let index = Rc::new(RefCell::new(Issues::default()));
            let mut model =
                IssueRows::new(LocalState::new(tasks(&board, &index)), filter.clone(), None);
            model.changes.clear();

            board.create("A new top", None);
            follow(&mut model, &board, &index, "a top-level issue made");
            board.create("A new sub-issue", Some(ids[20].clone()));
            follow(&mut model, &board, &index, "a sub-issue made");
            board
                .apply(IssueList::PLACE.with(issue_list::Place {
                    issue: ids[4].clone(),
                    status: crate::model::Status::Done,
                    before: None,
                    rank: None,
                }))
                .unwrap();
            follow(&mut model, &board, &index, "a sub-issue done");
            board
                .apply(IssueList::PLACE.with(issue_list::Place {
                    issue: ids[30].clone(),
                    status: crate::model::Status::Done,
                    before: Some(ui::tilo_resource::items::Ref::from(&ids[0])),
                    rank: None,
                }))
                .unwrap();
            follow(&mut model, &board, &index, "a parent done and moved first");
            board
                .apply(IssueList::MOVE_UNDER.with(issue_list::MoveUnder {
                    issue: ids[40].clone(),
                    parent: Some(ui::tilo_resource::items::Ref::from(&ids[50])),
                }))
                .unwrap();
            follow(&mut model, &board, &index, "a parent moved under another");
            board
                .apply(IssueList::DELETE.with(issue_list::Delete {
                    issue: ids[10].clone(),
                }))
                .unwrap();
            follow(&mut model, &board, &index, "a parent deleted");
            board
                .apply(IssueList::MOVE_UNDER.with(issue_list::MoveUnder {
                    issue: ids[45].clone(),
                    parent: None,
                }))
                .unwrap();
            follow(&mut model, &board, &index, "a sub-issue moved to the top");
        }
    }
}
