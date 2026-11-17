//! The board: a lane per status, each issue the sidebar's filter shows a
//! card in its lane.
//!
//! Dragging a card is one `place` action: its status and its place in the
//! lane, one undo step. A card opens its issue; its status changes there
//! too, so nothing needs a pointer.

use crate::model::{Issue, Status, Tracker};
use crate::part::{IssuePart, Issues, issue_part};
use ui::prelude::*;
use ui::widget::{Badge, Board, BoardDrop, Card, ChipStyle, Tone};

use crate::{Filter, Nav, TasksEffect, Window, reference, tr};

pub(crate) fn view(cx: &ViewCx<Tracker>, nav: &Nav, window: &Window) -> Element {
    let chosen = nav.filter.clone();
    let me = cx.me().cloned();
    let window = window.clone();
    let board = cx.switch(&chosen, move |cx, chosen| {
        let filter = chosen.as_deref().and_then(Filter::of).unwrap_or_default();
        let me = me.clone();
        let shown = window.clone();
        cx.switch(&window.tasks, move |cx, tasks| {
            lanes(cx, &shown, &tasks.issues(), &filter, me.as_ref())
        })
    });
    // The board's name is the window's location: the bar says where the
    // window is, so nothing here titles it a second time. Each lane adds at
    // its foot, in its own place: what is typed there lands in that lane.
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .with_fill_remaining_height()
            .push(Box::new(
                Scrollable::new_horizontal()
                    .fill_parent_height()
                    .with_child(board),
            )),
    )
}

/// The status a lane stands for, by its key.
fn status_of(lane: &str) -> Option<Status> {
    Status::ALL
        .into_iter()
        .find(|status| status.label_key() == lane)
}

fn lanes(
    cx: &ViewCx<Tracker>,
    window: &Window,
    issues: &Issues<'_>,
    filter: &Filter,
    me: Option<&ui::tilo_resource::MemberId>,
) -> Element {
    let people = cx.people();
    let mut board = Board::new().with_id("tasks-board");
    for status in Status::ALL {
        let cards: Vec<(String, Element)> = issues
            .iter()
            .filter(|(_, issue)| issue.status == status && filter.matches(issue, me))
            .map(|(id, issue)| (id.to_string(), card(window, issues, id, issue, &people)))
            .collect();
        let header = Text::new(&crate::locales::CATALOG.t_args(
            "tasks-lane-heading",
            &[
                ("status", tr(status.label_key()).into()),
                ("count", cards.len().into()),
            ],
        ))
        .with_role(TextRole::Subheading);
        board = board.lane(
            status.label_key(),
            Some(Box::new(header) as Element),
            cards,
            Some(crate::new_issue(
                cx,
                window,
                None,
                Some(status),
                &format!("tasks-board-new-{}", status.label_key()),
            )),
        );
    }
    // A drop is the `place` action: one gesture, one undo step, confirmed
    // and permitted like the same move made anywhere else.
    let place = IssuePart::PLACE.try_map(move |drop: BoardDrop| {
        Some(issue_part::Place {
            issue: Id::from_raw(drop.card),
            status: status_of(&drop.lane)?,
            before: drop.before.map(Id::from_raw),
            rank: None,
        })
    });
    let board = match window
        .actions
        .as_ref()
        .and_then(|actions| actions.binding(place))
    {
        Some(binding) => board.on_drop_binding(binding),
        None => board,
    };
    Box::new(board)
}

/// One issue's card: its reference and title, and what it is waiting on.
fn card(
    window: &Window,
    issues: &Issues<'_>,
    id: &Id,
    issue: &Issue,
    people: &ui::framework::People,
) -> Element {
    let mut facts = Row::new().with_rhythm(ui::tokens::Rhythm::Tight);
    facts = facts.push(Box::new(
        Text::new(&reference(issue.number))
            .with_role(TextRole::Caption)
            .dimmed(),
    ));
    if let Some(who) = assignee(people, issue) {
        facts = facts.push(Box::new(if who.agent() {
            Badge::text(who.name).style(ChipStyle::Tinted(Tone::Info))
        } else {
            Badge::text(who.name)
        }));
    }
    let (closed, total) = issues.progress(id);
    if total > 0 {
        facts = facts.push(Box::new(
            Text::new(&crate::locales::CATALOG.t_args(
                "tasks-progress",
                &[("closed", closed.into()), ("total", total.into())],
            ))
            .with_role(TextRole::Caption)
            .dimmed(),
        ));
    }
    for label in issue.labels.iter() {
        facts = facts.push(Box::new(Badge::text(label.clone())));
    }
    let card = Card::new(Box::new(facts))
        .with_title(issue.title.clone())
        .with_accessible_name(format!("{} {}", reference(issue.number), issue.title));
    // The card opens its issue through the app's published effect, the one
    // the list, a script and an agent invoke.
    let open = TasksEffect::OpenIssue { issue: id.clone() };
    Box::new(
        match window
            .actions
            .as_ref()
            .and_then(|actions| actions.effect(&open))
        {
            Some(binding) => card.on_action(format!("tasks-card-{id}"), binding),
            None => card,
        },
    )
}

/// Whom `issue` is assigned to, as the views show them: a member of the workspace, or someone
/// outside it by their name.
pub(crate) fn assignee(
    people: &ui::framework::People,
    issue: &Issue,
) -> Option<ui::framework::Actor> {
    match issue.assigned_member() {
        Some(member) => Some(people.actor_of(&member)),
        None => issue.assigned_outsider().map(ui::framework::Actor::outside),
    }
}
