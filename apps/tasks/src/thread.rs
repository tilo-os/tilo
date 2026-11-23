//! The thread view: the tasks whose conversation has something in it, as
//! conversations, the newest task first.
//!
//! A task is its conversation, so this is a view of the same issues the list
//! and the board show, not a copy of them: opening a row opens the same
//! issue every other view opens. Each row is the task's title, who last
//! spoke in it and a line of what they said, read from the issue's own
//! thread.

use crate::model::{Issue, Tracker};
use ui::prelude::*;
use ui::widget::ListRow;

use crate::{Filter, Issues, Nav, Window, reference};

/// One task as the thread view reads it: the issue and what its thread last
/// said.
struct Row {
    number: u64,
    title: String,
    subtitle: String,
    /// The last message, in a line.
    said: String,
}

impl Row {
    fn of(issue: &Issue, people: &ui::framework::People) -> Option<Self> {
        let (_, last) = issue.comments.iter().last()?;
        let speaker = people
            .provenance(
                last.author.as_ref(),
                &last.author_name,
                last.at_ms,
                last.origin.as_ref(),
            )
            .name;
        Some(Self {
            number: issue.number,
            title: format!("{}  {}", reference(issue.number), issue.title),
            subtitle: crate::locales::CATALOG
                .t_args("tasks-thread-last-from", &[("speaker", speaker.into())]),
            said: one_line(&last.text),
        })
    }
}

/// A message as a row reads it: its first line, kept short.
fn one_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim();
    if line.chars().count() <= 120 {
        return line.to_owned();
    }
    let kept: String = line.chars().take(119).collect();
    format!("{kept}…")
}

/// The tasks showing a conversation, the newest task first: a task that was
/// asked later reads above one asked before it.
fn rows(issues: &Issues, people: &ui::framework::People) -> Vec<(Id, Row)> {
    let mut shown: Vec<(Id, Row)> = issues
        .iter()
        .filter(|(_, issue)| !issue.comments.is_empty())
        .filter_map(|(id, issue)| Row::of(issue, people).map(|row| (id.clone(), row)))
        .collect();
    shown.sort_by_key(|(_, row)| std::cmp::Reverse(row.number));
    shown
}

pub(crate) fn view(cx: &ViewCx<Tracker>, nav: &Nav, window: &Window) -> Element {
    let chosen = nav.filter.clone();
    let me = cx.me().cloned();
    let people = cx.people();
    let nav = nav.clone();
    let tasks = window.tasks.clone();
    let chain = window.chain.clone();
    let list = cx.switch(&chosen, move |cx, chosen| {
        let filter = chosen.as_deref().and_then(Filter::of).unwrap_or_default();
        let nav = nav.clone();
        let me = me.clone();
        let people = people.clone();
        let chain = chain.clone();
        cx.switch(&tasks, move |_, tasks| {
            // Every thread is shown here, which the window's reading of the
            // issues leaves unread until a view asks for it.
            if let Some(chain) = &chain {
                for (index, part) in tasks.parts.iter() {
                    let ids = part.issues.iter().map(|(id, _)| id.as_str().to_owned());
                    chain.demand(*index, ui::tilo_resource::Demand::none().with_keys(ids));
                }
            }
            list_of(&tasks.issues(), &nav, &filter, me.as_ref(), &people)
        })
    });
    // The view's name is the window's location: the bar says where the
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
                "tasks-thread-new",
            ))
            .push(list),
    )
}

fn list_of(
    issues: &Issues,
    nav: &Nav,
    filter: &Filter,
    me: Option<&ui::tilo_resource::MemberId>,
    people: &ui::framework::People,
) -> Element {
    if issues.is_empty() {
        return Box::new(
            EmptyState::new("tasks-empty-title")
                .with_message("tasks-empty-message")
                .with_id("tasks-thread-empty"),
        );
    }
    let shown: Vec<(Id, Row)> = rows(issues, people)
        .into_iter()
        .filter(|(id, _)| {
            issues
                .get(id)
                .is_some_and(|issue| filter.shows(issues, id, issue, me))
        })
        .collect();
    if shown.is_empty() {
        // The tracker holds issues; none of them has said anything yet, or
        // the sidebar's filter shows none of the ones that have.
        return Box::new(
            EmptyState::new("tasks-thread-empty-title")
                .with_message("tasks-thread-empty-message")
                .with_id("tasks-thread-nothing"),
        );
    }
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Tight);
    for (id, row) in shown {
        column = column.push(row_of(nav, &id, &row));
    }
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(column)),
    )
}

/// One task's row, opening the same issue every other view opens.
fn row_of(nav: &Nav, id: &Id, row: &Row) -> Element {
    let open = nav.clone();
    let issue_id = id.clone();
    Box::new(
        ListRow::new(format!("tasks-thread-{id}"), &row.title)
            .subtitle(&row.subtitle)
            .meta(&row.said)
            .icon(ui::icons::Icon::MessageCircle)
            .on_open(move || open.open(&issue_id)),
    )
}
