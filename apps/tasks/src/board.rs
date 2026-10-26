//! The board: a lane per status, each issue the sidebar's filter shows a
//! card in its lane.
//!
//! Dragging a card is one `place` action: its status and its place in the
//! lane, one undo step. A card opens its issue; its status changes there
//! too, so nothing needs a pointer.

use crate::model::{Issue, Status, Tracker, tracker};
use ui::prelude::*;
use ui::widget::{Badge, Board, BoardDrop, Card, ChipStyle, Tone};

use crate::{Filter, Nav, reference, tr};

pub(crate) fn view(cx: &ViewCx<Tracker>, nav: &Nav) -> Element {
    let chosen = nav.filter.clone();
    let me = cx.me().map(str::to_owned);
    let nav = nav.clone();
    let board = cx.switch(&chosen, move |cx, chosen| {
        let filter = chosen.as_deref().and_then(Filter::of).unwrap_or_default();
        let tracker = cx.value(Tracker::clone);
        let nav = nav.clone();
        let me = me.clone();
        cx.switch(&tracker, move |cx, tracker| {
            lanes(cx, tracker, &nav, &filter, me.as_deref())
        })
    });
    // The board's name is the window's location: the bar says where the
    // window is, so nothing here titles it a second time.
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .with_fill_remaining_height()
            .push(crate::new_issue(cx, None, "tasks-board-new"))
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
    tracker: &Tracker,
    nav: &Nav,
    filter: &Filter,
    me: Option<&str>,
) -> Element {
    let people = cx.people();
    let mut board = Board::new().with_id("tasks-board");
    for status in Status::ALL {
        let cards: Vec<(String, Element)> = tracker
            .issues
            .iter()
            .filter(|(_, issue)| issue.status == status && filter.matches(issue, me))
            .map(|(id, issue)| (id.to_string(), card(tracker, id, issue, &people, nav)))
            .collect();
        let header = Text::new(&ui::i18n::I18nManager::format(
            "tasks-lane-heading",
            &[
                ("status", tr(status.label_key()).into()),
                ("count", cards.len().into()),
            ],
        ))
        .with_role(TextRole::Subheading);
        board = board.lane(status.label_key(), Box::new(header), cards);
    }
    let dispatcher = cx.dispatcher();
    Box::new(board.on_drop(move |BoardDrop { card, lane, before }| {
        let Some(status) = status_of(&lane) else {
            return;
        };
        dispatcher.submit(Tracker::PLACE.with(tracker::Place {
            issue: Id::from_raw(card),
            status,
            before: before.map(Id::from_raw),
        }));
    }))
}

/// What the room calls the agent `agent` (`agent:<id>`), whichever way its
/// roster spells the id; the bare id where the room names it not.
fn agent_name(people: &ui::framework::People, agent: &str) -> String {
    let bare = agent.trim_start_matches("agent:");
    [agent, bare]
        .into_iter()
        .map(|id| people.name(id))
        .find(|name| name != agent && name != bare)
        .unwrap_or_else(|| bare.to_owned())
}

/// One issue's card: its reference and title, and what it is waiting on.
fn card(
    tracker: &Tracker,
    id: &Id,
    issue: &Issue,
    people: &ui::framework::People,
    nav: &Nav,
) -> Element {
    let mut facts = Row::new().with_rhythm(ui::tokens::Rhythm::Tight);
    facts = facts.push(Box::new(
        Text::new(&reference(issue.number))
            .with_role(TextRole::Caption)
            .dimmed(),
    ));
    if let Some(agent) = issue.assigned_agent() {
        facts = facts.push(Box::new(
            Badge::text(agent_name(people, agent)).style(ChipStyle::Tinted(Tone::Info)),
        ));
    } else if let Some(person) = issue.assigned_person() {
        facts = facts.push(Box::new(Badge::text(people.name(person))));
    }
    if let Some(run) = issue.active_run() {
        facts = facts.push(Box::new(
            Badge::text(tr(run.phase.label_key())).style(ChipStyle::Live),
        ));
    }
    let (closed, total) = tracker.progress(id);
    if total > 0 {
        facts = facts.push(Box::new(
            Text::new(&ui::i18n::I18nManager::format(
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
    let open = nav.clone();
    let issue_id = id.clone();
    Box::new(
        Card::new(Box::new(facts))
            .with_title(issue.title.clone())
            .with_accessible_name(format!("{} {}", reference(issue.number), issue.title))
            .on_activate(format!("tasks-card-{id}"), move || open.open(&issue_id)),
    )
}
