//! The list: every issue, sub-issues nested under their parent.
//!
//! Each row reads as the issue's reference and title, with its status
//! beside it. Choosing a row opens the issue; dragging one onto another
//! makes it a sub-issue there, the tracker's `move-under`.

use std::rc::Rc;

use crate::model::{Issue, Tracker, tracker};
use ui::prelude::*;
use ui::reorder::{DropPlacement, ReorderDrop};
use ui::widget::{TreeClickActivation, TreeRowDecoration, TreeRowTone};

use crate::{Nav, reference, tr};

/// The root node's id; issues take ids from 1, by their place in the
/// tracker.
const ROOT: u64 = 0;

/// What a row shows besides its label.
#[derive(Clone)]
struct Row {
    number: u64,
    status: crate::model::Status,
    progress: (usize, usize),
}

pub(crate) fn view(cx: &ViewCx<Tracker>, nav: &Nav) -> Element {
    let tracker = cx.value(Tracker::clone);
    let nav = nav.clone();
    let list = cx.switch(&tracker, move |cx, tracker| tree(cx, tracker, &nav));
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .with_fill_remaining_height()
            .push(Box::new(
                Text::new("tasks-place-list").with_role(TextRole::Title),
            ))
            .push(crate::new_issue(cx, None, "tasks-list-new"))
            .push(list),
    )
}

fn label(issue: &Issue) -> String {
    format!("{}  {}", reference(issue.number), issue.title)
}

fn node(tracker: &Tracker, parent: Option<&Id>, ids: &mut Vec<Id>) -> Vec<TreeNode<Option<Row>>> {
    tracker
        .children(parent)
        .map(|(id, issue)| {
            ids.push(id.clone());
            let key = u64::try_from(ids.len()).unwrap_or(u64::MAX);
            let data = Some(Row {
                number: issue.number,
                status: issue.status,
                progress: tracker.progress(id),
            });
            let children = node(tracker, Some(id), ids);
            if children.is_empty() {
                TreeNode::leaf(key, label(issue), data)
            } else {
                TreeNode::branch(key, label(issue), data, children)
            }
        })
        .collect()
}

fn tree(cx: &ViewCx<Tracker>, tracker: &Tracker, nav: &Nav) -> Element {
    if tracker.issues.is_empty() {
        return Box::new(
            EmptyState::new("tasks-empty-title")
                .with_message("tasks-empty-message")
                .with_id("tasks-list-empty"),
        );
    }
    let mut ids = Vec::new();
    let children = node(tracker, None, &mut ids);
    let expanded: Vec<u64> = (ROOT..=u64::try_from(ids.len()).unwrap_or(u64::MAX)).collect();
    let ids = Rc::new(ids);
    let issue_of = {
        let ids = Rc::clone(&ids);
        move |key: u64| -> Option<Id> {
            usize::try_from(key)
                .ok()
                .and_then(|key| key.checked_sub(1))
                .and_then(|index| ids.get(index).cloned())
        }
    };
    let open_issue = issue_of.clone();
    let opener = nav.clone();
    let check = tracker.clone();
    let validate_issue = issue_of.clone();
    let dispatcher = cx.dispatcher();
    let parents = tracker.clone();
    Box::new(
        TreeView::new(TreeNode::branch(
            ROOT,
            tr("tasks-place-list"),
            None,
            children,
        ))
        .with_id("tasks-list")
        .with_accessible_name(tr("tasks-place-list"))
        .hide_root()
        .fill()
        .with_expanded(expanded)
        .click_activation(TreeClickActivation::SingleClick)
        .on_activate(move |key| {
            if let Some(issue) = open_issue(key) {
                opener.open(&issue);
            }
        })
        .with_row_decoration(decorate)
        .with_reorder(
            move |_, source, target| {
                let (Some(source), Some(target)) =
                    (validate_issue(source), validate_issue(target.key))
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
        ),
    )
}

/// A row's status beside its label, and its spoken description.
fn decorate(
    model: &TreeNode<Option<Row>>,
    row: ui::widget::TreeRowInfo,
    theme: &ui::theme::Theme,
) -> TreeRowDecoration {
    let Some(Some(data)) = model.find(row.id).map(|node| node.data.clone()) else {
        return TreeRowDecoration::new();
    };
    let mut status = tr(data.status.label_key());
    if data.progress.1 > 0 {
        status = ui::i18n::I18nManager::format(
            "tasks-status-with-progress",
            &[
                ("status", status.into()),
                ("closed", data.progress.0.into()),
                ("total", data.progress.1.into()),
            ],
        );
    }
    let decoration = TreeRowDecoration::new()
        .trailing(status.clone())
        .description(format!("{}, {status}", reference(data.number)));
    if data.status.closed() {
        decoration
            .tone(TreeRowTone::Secondary)
            .icon(ui::icons::Icon::CircleCheck, theme.tokens.colors.text_dim)
    } else {
        decoration.icon(
            ui::icons::Icon::CircleDot,
            theme.tokens.colors.intent.accent,
        )
    }
}
