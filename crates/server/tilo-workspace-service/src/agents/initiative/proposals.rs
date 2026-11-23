//! What a person's assistant proposed, and what its person made of each
//! kind of work ([`Book`]).
//!
//! A proposal waits for its person's nod. Two things a person says about a
//! kind of work are kept with it and decide what the next proposal of that
//! kind does: turned down [`workspace_initiative::QUIET_AFTER`] times, the
//! kind is quiet and the server refuses it when the assistant proposes it
//! again; said always to, a piece of work of that kind starts without a
//! nod. Both are the person's to take back. The rule is kept here, in what
//! the server answers, and not in what a model was told.

use crate::agents::events::{Definition, told_as_of};
use serde::{Deserialize, Serialize};
use tilo_actions::remote::workspace_events::SetUp;
use tilo_actions::remote::workspace_initiative::{
    self, Proposal, Proposals, Propose, ProposedTask, State,
};

/// The most proposals kept for a person. Past it the oldest that were
/// decided go; what still waits is never dropped.
const MAX_KEPT: usize = 200;

/// What a task a proposal stands as says about it in the tracker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InTracker {
    /// Nobody gave it to the assistant and it is still open.
    Waits,
    /// Its person gave it to the assistant, or work on it began.
    Given,
    /// It was cancelled, or is gone.
    Dropped,
}

/// A person's proposals, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Book {
    proposals: Vec<Proposal>,
}

/// `text` on one line: its words, a single blank between them.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What `wanted` proposes, held to its bounds and written the one way it is
/// kept: the kind as its word, the title and the why each on one line, and
/// an event as the events would keep it.
///
/// # Errors
/// What is wrong with it, in words for whoever proposed it.
pub(super) fn checked(wanted: Propose) -> Result<Propose, String> {
    let kind = workspace_initiative::kind_word(&wanted.kind).ok_or_else(|| {
        format!(
            "a kind is your own short word for this sort of work, at most {} bytes",
            workspace_initiative::MAX_KIND_BYTES
        )
    })?;
    let title = one_line(&wanted.title);
    if title.is_empty() || title.chars().count() > workspace_initiative::MAX_TITLE_CHARS {
        return Err(format!(
            "a proposal has a title of at most {} characters",
            workspace_initiative::MAX_TITLE_CHARS
        ));
    }
    let why = one_line(&wanted.why);
    if why.is_empty() || why.chars().count() > workspace_initiative::MAX_WHY_CHARS {
        return Err(format!(
            "a proposal says why in one line of at most {} characters",
            workspace_initiative::MAX_WHY_CHARS
        ));
    }
    let event = wanted.event.map(event_checked).transpose()?;
    Ok(Propose {
        kind,
        title,
        why,
        event,
    })
}

/// `event` as the events would keep it, or why they would refuse it: said
/// when it is proposed, not when its person nods.
fn event_checked(event: SetUp) -> Result<SetUp, String> {
    let checked = Definition {
        id: String::new(),
        name: event.name.clone(),
        when: event.when.clone(),
        only_if: event.only_if.clone(),
        every: event.every,
        instruction: event.instruction.clone(),
        told_as: told_as_of(event.told_as.as_deref().unwrap_or_default())?,
        set_up_at_ms: 0,
    }
    .checked()?;
    Ok(SetUp {
        name: checked.name,
        when: checked.when,
        only_if: checked.only_if,
        every: checked.every,
        instruction: checked.instruction,
        ..event
    })
}

impl Book {
    /// How many proposals wait for their person.
    fn waiting(&self) -> usize {
        self.proposals
            .iter()
            .filter(|proposal| proposal.state.waits())
            .count()
    }

    /// Keep what `wanted` proposes, already [`checked`], under `id`.
    ///
    /// The same thing proposed again while it waits is the proposal that
    /// waits.
    ///
    /// # Errors
    /// Too much waits already: in words for the assistant.
    pub(super) fn propose(
        &mut self,
        wanted: Propose,
        id: String,
        look: Option<String>,
        now_ms: i64,
    ) -> Result<Proposal, String> {
        if let Some(waits) = self.proposals.iter().find(|proposal| {
            proposal.state.waits() && proposal.kind == wanted.kind && proposal.title == wanted.title
        }) {
            return Ok(waits.clone());
        }
        if self.waiting() >= workspace_initiative::MAX_WAITING {
            return Err(format!(
                "{} proposals wait for your person already: propose more once they decided some",
                workspace_initiative::MAX_WAITING
            ));
        }
        let proposal = Proposal {
            id,
            kind: wanted.kind,
            title: wanted.title,
            why: wanted.why,
            task: None,
            event: wanted.event,
            state: State::Proposed,
            proposed_at_ms: now_ms,
            decided_at_ms: None,
            look,
            set_up: None,
        };
        self.proposals.push(proposal.clone());
        self.trim();
        Ok(proposal)
    }

    /// The proposal `id`, as it is kept.
    ///
    /// # Errors
    /// The person keeps no such proposal.
    pub(super) fn get(&self, id: &str) -> Result<&Proposal, String> {
        self.proposals
            .iter()
            .find(|proposal| proposal.id == id)
            .ok_or_else(|| "you keep no such proposal here".to_owned())
    }

    fn get_mut(&mut self, id: &str) -> Result<&mut Proposal, String> {
        self.proposals
            .iter_mut()
            .find(|proposal| proposal.id == id)
            .ok_or_else(|| "you keep no such proposal here".to_owned())
    }

    /// The person's nod to `id`. A nod given again changes nothing.
    ///
    /// # Errors
    /// There is no such proposal, or it was turned down.
    pub(super) fn accept(&mut self, id: &str, now_ms: i64) -> Result<Proposal, String> {
        let proposal = self.get_mut(id)?;
        match proposal.state {
            State::Declined => {
                return Err("it was turned down: it has to be proposed again".to_owned());
            }
            State::Proposed => {
                proposal.state = State::Accepted;
                proposal.decided_at_ms = Some(now_ms);
            }
            State::Accepted | State::Started => {}
        }
        Ok(proposal.clone())
    }

    /// `id` turned down.
    ///
    /// # Errors
    /// There is no such proposal, or its work was begun already.
    pub(super) fn decline(&mut self, id: &str, now_ms: i64) -> Result<Proposal, String> {
        let proposal = self.get_mut(id)?;
        match proposal.state {
            State::Declined => return Ok(proposal.clone()),
            State::Accepted | State::Started => {
                return Err("its work was begun already: stop it where it is worked on".to_owned());
            }
            State::Proposed => {}
        }
        proposal.state = State::Declined;
        proposal.decided_at_ms = Some(now_ms);
        Ok(proposal.clone())
    }

    /// Have `id` wait again: its person's nod could not be carried out.
    /// What was turned down is left as it is.
    pub(super) fn waits_again(&mut self, id: &str) {
        if let Ok(proposal) = self.get_mut(id)
            && proposal.state == State::Accepted
        {
            proposal.state = State::Proposed;
            proposal.decided_at_ms = None;
            proposal.set_up = None;
        }
    }

    /// The task `id` stands as in the tracker.
    pub(super) fn stands_as(&mut self, id: &str, task: ProposedTask) {
        if let Ok(proposal) = self.get_mut(id) {
            proposal.task = Some(task);
        }
    }

    /// The event that was set up when `id` was accepted.
    pub(super) fn set_up_as(&mut self, id: &str, event: String) {
        if let Ok(proposal) = self.get_mut(id) {
            proposal.set_up = Some(event);
        }
    }

    /// Take `id` back as if it was never proposed: what it should have
    /// stood as could not be written.
    pub(super) fn withdraw(&mut self, id: &str) {
        self.proposals.retain(|proposal| proposal.id != id);
    }

    /// Whether anything that waits stands as a task in the tracker.
    pub(super) fn waits_in_tracker(&self) -> bool {
        self.proposals
            .iter()
            .any(|proposal| proposal.state.waits() && proposal.task.is_some())
    }

    /// Hold every proposal that waits against what its task says in the
    /// tracker, as `in_tracker` answers for an issue's address: given to the
    /// assistant there it is accepted, cancelled or gone there it is
    /// declined. So a person decides where they look, and the server keeps
    /// score from the tracker. Whether anything changed.
    pub(super) fn reconcile(
        &mut self,
        in_tracker: impl Fn(&str) -> InTracker,
        now_ms: i64,
    ) -> bool {
        let decided: Vec<(String, InTracker)> = self
            .proposals
            .iter()
            .filter(|proposal| proposal.state.waits())
            .filter_map(|proposal| {
                let task = proposal.task.as_ref()?;
                Some((proposal.id.clone(), in_tracker(&task.issue)))
            })
            .filter(|(_, stands)| *stands != InTracker::Waits)
            .collect();
        for (id, stands) in &decided {
            // Each was waiting a moment ago, so neither is refused.
            let _ = match stands {
                InTracker::Given => self.accept(id, now_ms),
                InTracker::Dropped => self.decline(id, now_ms),
                InTracker::Waits => continue,
            };
        }
        !decided.is_empty()
    }

    /// Everything, as its person reads it: the proposals newest first.
    pub(super) fn listed(&self) -> Proposals {
        Proposals {
            proposals: self.proposals.iter().rev().cloned().collect(),
        }
    }

    /// Keep at most [`MAX_KEPT`]: the oldest decided ones go first.
    fn trim(&mut self) {
        while self.proposals.len() > MAX_KEPT {
            let Some(oldest) = self
                .proposals
                .iter()
                .position(|proposal| !proposal.state.waits())
            else {
                return;
            };
            self.proposals.remove(oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(kind: &str, title: &str) -> Propose {
        checked(Propose {
            kind: kind.to_owned(),
            title: title.to_owned(),
            why: "Three of them changed since Monday.".to_owned(),
            event: None,
        })
        .unwrap_or_else(|error| panic!("{error}"))
    }

    fn proposed(book: &mut Book, kind: &str, title: &str, id: &str) -> Result<Proposal, String> {
        book.propose(wanted(kind, title), id.to_owned(), None, 1)
    }

    #[test]
    fn turning_one_down_stays_declined_and_begun_work_is_not_turned_down() {
        let mut book = Book::default();
        proposed(&mut book, "tidy", "Tidy the notes", "a").unwrap();
        book.decline("a", 2).unwrap();
        book.decline("a", 3).unwrap();
        assert!(book.accept("a", 4).is_err(), "turned down stays so");
        proposed(&mut book, "tidy", "Tidy the files", "b").unwrap();
        assert_eq!(
            book.accept("b", 5).map(|accepted| accepted.state),
            Ok(State::Accepted)
        );
        assert_eq!(
            book.accept("b", 6).map(|accepted| accepted.decided_at_ms),
            Ok(Some(5)),
            "a nod given again changes nothing"
        );
        assert!(book.decline("b", 7).is_err());
        assert!(book.decline("nobody", 8).is_err());
    }

    #[test]
    fn the_same_thing_proposed_again_is_the_one_that_waits_and_too_many_are_refused() {
        let mut book = Book::default();
        let first = proposed(&mut book, "tidy", "Tidy the notes", "a").unwrap();
        let again = proposed(&mut book, "tidy", "  Tidy   the notes ", "b").unwrap();
        assert_eq!(again, first);
        for n in 1..workspace_initiative::MAX_WAITING {
            proposed(&mut book, "tidy", &format!("Tidy {n}"), &n.to_string()).unwrap();
        }
        let refused = proposed(&mut book, "tidy", "One more", "more");
        assert!(
            matches!(&refused, Err(why) if why.contains("wait for your person already")),
            "{refused:?}"
        );
        book.accept("a", 2).unwrap();
        assert!(proposed(&mut book, "tidy", "One more", "more").is_ok());
    }

    #[test]
    fn what_its_person_did_with_the_task_counts_as_their_answer() {
        let mut book = Book::default();
        for (id, title) in [("a", "One"), ("b", "Two"), ("c", "Three"), ("d", "Four")] {
            proposed(&mut book, "tidy", title, id).unwrap();
            book.stands_as(
                id,
                ProposedTask {
                    issue: format!("issues/{id}"),
                    number: None,
                },
            );
        }
        proposed(&mut book, "tidy", "No task yet", "e").unwrap();
        let changed = book.reconcile(
            |issue| match issue {
                "issues/a" => InTracker::Given,
                "issues/b" => InTracker::Dropped,
                _ => InTracker::Waits,
            },
            9,
        );
        assert!(changed);
        let state = |id: &str| book.get(id).map(|proposal| proposal.state);
        assert_eq!(state("a"), Ok(State::Accepted));
        assert_eq!(state("b"), Ok(State::Declined));
        assert_eq!(state("c"), Ok(State::Proposed));
        assert_eq!(state("e"), Ok(State::Proposed), "no task says nothing");
        // Read again, nothing is counted twice.
        let mut again = book.clone();
        assert!(!again.reconcile(|_| InTracker::Waits, 10));
        assert!(!again.reconcile(
            |issue| if issue == "issues/b" {
                InTracker::Dropped
            } else {
                InTracker::Waits
            },
            11
        ));
        assert_eq!(again, book);
    }

    #[test]
    fn a_proposal_past_its_bounds_is_refused_in_words() {
        let refused = |wanted: Propose| checked(wanted).err().unwrap_or_default();
        let base = || Propose {
            kind: "tidy".into(),
            title: "Tidy the notes".into(),
            why: "They are\nall over the place.".into(),
            event: None,
        };
        assert_eq!(
            checked(base()).map(|kept| kept.why).ok().as_deref(),
            Some("They are all over the place."),
            "why is one line"
        );
        assert!(
            refused(Propose {
                kind: " - ".into(),
                ..base()
            })
            .contains("short word")
        );
        assert!(
            refused(Propose {
                title: " ".into(),
                ..base()
            })
            .contains("title")
        );
        assert!(
            refused(Propose {
                why: "x".repeat(workspace_initiative::MAX_WHY_CHARS + 1),
                ..base()
            })
            .contains("one line")
        );
        // An event no server can set up is refused when it is proposed.
        assert!(
            refused(Propose {
                event: Some(SetUp {
                    name: "Soon".into(),
                    when: "in two minutes".into(),
                    instruction: "Say hello.".into(),
                    ..SetUp::default()
                }),
                ..base()
            })
            .contains("once at")
        );
    }

    #[test]
    fn a_nod_that_could_not_be_carried_out_leaves_the_proposal_waiting() {
        let mut book = Book::default();
        proposed(&mut book, "tidy", "Tidy the notes", "a").unwrap();
        book.accept("a", 2).unwrap();
        book.set_up_as("a", "an-event".into());
        book.waits_again("a");
        let waits = book.get("a").unwrap();
        assert_eq!(
            (waits.state, waits.decided_at_ms, waits.set_up.clone()),
            (State::Proposed, None, None)
        );
        // What was turned down is not made to wait again.
        book.decline("a", 3).unwrap();
        book.waits_again("a");
        assert_eq!(book.get("a").map(|kept| kept.state), Ok(State::Declined));
    }

    #[test]
    fn decided_proposals_go_before_any_that_waits() {
        let mut book = Book::default();
        proposed(&mut book, "tidy", "Kept waiting", "waits").unwrap();
        for n in 0..MAX_KEPT + 3 {
            let id = n.to_string();
            proposed(&mut book, "tidy", &format!("Tidy {n}"), &id).unwrap();
            book.accept(&id, 2).unwrap();
        }
        assert_eq!(book.proposals.len(), MAX_KEPT);
        assert_eq!(book.proposals[0].id, "waits");
        assert_eq!(book.proposals[1].id, "4");
    }
}
