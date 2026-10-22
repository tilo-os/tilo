//! The board: a lane per status, each issue a card in its lane.
//!
//! Dragging a card is one `place` action: its status and its place in the
//! lane, one undo step, in the tracker that keeps the issue. A card opens
//! its issue; its status changes there too, so nothing needs a pointer.

use crate::Shown;
use crate::model::{Issue, Status, Tracker, tracker};
use ui::prelude::*;
use ui::widget::{Badge, Board, BoardDrop, Card, ChipStyle, Tone};

use crate::{Nav, reference, tr};

pub(crate) fn view(cx: &ViewCx<Tracker>, nav: &Nav) -> Element {
    let shown = nav.shown.clone();
    let nav = nav.clone();
    let board = cx.switch(&shown, move |cx, shown| lanes(cx, shown, &nav));
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .with_fill_remaining_height()
            .push(Box::new(
                Text::new("tasks-place-board").with_role(TextRole::Title),
            ))
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

fn lanes(cx: &ViewCx<Tracker>, shown: &Shown, nav: &Nav) -> Element {
    let tracker = &shown.tracker;
    let mut board = Board::new().with_id("tasks-board");
    for status in Status::ALL {
        let cards: Vec<(String, Element)> = tracker
            .issues
            .iter()
            .filter(|(_, issue)| issue.status == status)
            .map(|(id, issue)| {
                let private = shown.private.contains_key(id);
                (id.to_string(), card(tracker, id, issue, private, nav))
            })
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
    let (dispatcher, nav) = (cx.dispatcher(), nav.clone());
    Box::new(board.on_drop(move |BoardDrop { card, lane, before }| {
        let Some(status) = status_of(&lane) else {
            return;
        };
        let issue = Id::from_raw(card);
        // A card placed before one another tracker keeps lands at the end.
        let before = before
            .map(Id::from_raw)
            .filter(|before| nav.home(before) == nav.home(&issue));
        let place = Tracker::PLACE.with(tracker::Place {
            issue: issue.clone(),
            status,
            before,
        });
        nav.submit(&dispatcher, &issue, place);
    }))
}

/// One issue's card: its reference and title, whether it is private, and
/// what it is waiting on.
fn card(tracker: &Tracker, id: &Id, issue: &Issue, private: bool, nav: &Nav) -> Element {
    let mut facts = Row::new().with_rhythm(ui::tokens::Rhythm::Tight);
    facts = facts.push(Box::new(
        Text::new(&reference(issue.number))
            .with_role(TextRole::Caption)
            .dimmed(),
    ));
    if private {
        facts = facts.push(Box::new(
            Badge::text(tr("tasks-private")).style(ChipStyle::Tinted(Tone::Warning)),
        ));
    }
    if let Some(agent) = issue.assigned_agent() {
        facts = facts.push(Box::new(
            Badge::text(agent.trim_start_matches("agent:")).style(ChipStyle::Tinted(Tone::Info)),
        ));
    } else if let Some(person) = issue.assigned_person() {
        facts = facts.push(Box::new(Badge::text(person)));
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
