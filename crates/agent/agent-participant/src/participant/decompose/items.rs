//! A coordinated task's items on their own: the list the coordinator keeps,
//! the order the items run in, the waits it holds while other items run,
//! what each item's agent is told of the items before it, and what a turn
//! of the coordinator changes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use tilo_actions::ActionOutput;
use tilo_browser_peer::DocumentRoute;

use super::super::running::{Running, step_brief};
use super::super::waits::{Arrival, holds_wanted, short_app, wait_words};
use crate::model::ROLE_INTRO;
use crate::planner::{
    MESSAGE_APPS, MOST_OBLIGATIONS, MOST_WAITS, Obligations, PlanRole, Step, StepAction, Wait,
    message_app,
};

/// The most characters of the request an item's agent is shown: the
/// request is there for reference, its item is what it plans.
const MOST_REQUEST_CHARS: usize = 2_000;

/// The most characters of what one item did, in a line, as the items after
/// it and the coordinator are told it.
const MOST_RESULT_CHARS: usize = 600;

/// The most characters of what one item read and its steps answered, as
/// the items after it are told it: the values they work with, not a line
/// about them.
const MOST_FOUND_CHARS: usize = 2_400;

/// The most characters of what all the items before one did and found:
/// the oldest findings are shortened first.
const MOST_RESULTS_CHARS: usize = 8_000;

/// The most messages others sent during a task that are kept for the
/// waits of its items.
const MOST_KEPT_ARRIVALS: usize = 32;

/// Words that join the words of an item's text rather than say what it
/// does: two items whose other words match are one item.
const ITEM_FILLER: &[&str] = &[
    "a", "an", "and", "as", "at", "by", "for", "from", "her", "his", "if", "in", "into", "is",
    "it", "its", "of", "on", "or", "our", "s", "the", "their", "then", "to", "with", "your",
];

/// Where an item is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum State {
    Pending,
    Running,
    /// A wait the coordinator holds while other items run: no model call
    /// until what it waits for arrives or it runs out.
    Waiting,
    Done,
    SetAside(String),
    Failed(String),
}

/// A wait the coordinator holds: when it runs out, and how many times it
/// waited.
#[derive(Clone, Copy, Debug)]
pub(super) struct Armed {
    pub(super) until: Instant,
    pub(super) count: u32,
}

/// One item of a coordinated task.
#[derive(Debug)]
pub(super) struct Item {
    pub(super) text: String,
    /// The app ids it needs; empty for any the agent may use.
    pub(super) apps: Vec<String>,
    /// The items, numbered from 1, that must be done before it.
    pub(super) after: Vec<usize>,
    /// What it waits for: the coordinator holds it, with no model call,
    /// while the items that do not need it run.
    pub(super) wait: Option<Wait>,
    /// While it waits, until when.
    pub(super) armed: Option<Armed>,
    /// What ended its wait.
    pub(super) arrived: Option<Arrival>,
    pub(super) state: State,
    /// What it did, in a line.
    pub(super) result: Option<String>,
    /// What it read and its steps answered: the values the items after it
    /// work with.
    pub(super) found: Option<String>,
    /// What the coordinator told it: a hint for news, or why it is done
    /// again.
    pub(super) note: Option<String>,
    /// It was marked to be done again once already.
    pub(super) redone: bool,
    /// It could not be done, and the coordinator let the items after it go
    /// on without it.
    pub(super) waived: bool,
    /// Its agent said "cannot" before trying anything and was asked again,
    /// told this.
    pub(super) retry: Option<String>,
    /// The check added it.
    pub(super) fixup: bool,
}

impl Item {
    pub(super) fn new(text: String, apps: Vec<String>, after: Vec<usize>) -> Self {
        Self {
            text,
            apps,
            after,
            wait: None,
            armed: None,
            arrived: None,
            state: State::Pending,
            result: None,
            found: None,
            note: None,
            redone: false,
            waived: false,
            retry: None,
            fixup: false,
        }
    }

    /// Whether the items after it may run: it is done, set aside, or could
    /// not be done and the coordinator went on without it.
    fn lets_on(&self) -> bool {
        match self.state {
            State::Done | State::SetAside(_) => true,
            State::Failed(_) => self.waived,
            State::Pending | State::Running | State::Waiting => false,
        }
    }
}

/// A coordinator's turn, while it is being planned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::participant) enum Turn {
    /// The first plan: the items.
    List,
    /// News arrived while the task ran.
    News,
    /// An item could not be done: redo it, adjust the items after it, or
    /// go on without it.
    Failure,
    /// Every item ended: the whole is checked once.
    Check,
}

/// What a coordinated task does next.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Next {
    /// Start the item at this index.
    Item(usize),
    /// Only waits the coordinator holds are left, or items that need them:
    /// sleep until a message arrives or one runs out.
    Hold,
    Check,
    Finish,
}

/// The coordinator of a running coordinated task.
pub(in crate::participant) struct Coordinator {
    pub(in crate::participant) task: String,
    /// The task as it was given.
    pub(super) request: String,
    pub(super) items: Vec<Item>,
    /// The index of the item whose agent runs.
    pub(super) current: Option<usize>,
    /// The coordinator's turn being planned.
    pub(in crate::participant) turn: Option<Turn>,
    /// The check was taken.
    pub(super) checked: bool,
    /// The one round of fixes was taken.
    pub(super) fixed: bool,
    pub(super) news_turns: u32,
    pub(super) failure_turns: u32,
    /// The items that could not be done that the coordinator was not asked
    /// about yet, and those its failure turn is being planned for.
    pub(super) failures: Vec<usize>,
    pub(super) asked_failures: Vec<usize>,
    /// The news the coordinator's turn is being planned for, and what it
    /// added: what the running item's agent is told with a note.
    pub(super) news: (Vec<DocumentRoute>, Vec<Arrival>),
    /// Messages others sent during the task: what the waits of its items
    /// look for, those that started after them included.
    pub(super) arrivals: Vec<Arrival>,
    /// What the message records others changed held before they did, since
    /// that was last taken in ([`Self::arrivals`]).
    pub(in crate::participant) before: BTreeMap<DocumentRoute, Arc<str>>,
    /// Nothing but its waits is left to do for now: it sleeps.
    pub(super) holding: bool,
    /// What the coordinator read while it listed the items.
    pub(super) read: Option<String>,
    /// The records the items made and acted on, the content they produced
    /// (with the app of the step that made each) and the latest step's
    /// output that produced content: what each new item's agent starts
    /// with, so it acts on the records the items before it made.
    pub(in crate::participant) made: Vec<DocumentRoute>,
    pub(in crate::participant) touched: Vec<DocumentRoute>,
    pub(super) produced: Vec<String>,
    pub(super) produced_by: Vec<String>,
    pub(super) last: Option<ActionOutput>,
    pub(super) performed: usize,
    /// What the running item's agent last said in reply.
    pub(super) reply: Option<String>,
    /// The running item's task, as [`super::super::AgentParticipant::end_running`]
    /// took it, until what it did is taken in.
    pub(in crate::participant) ended: Option<Running>,
}

impl Coordinator {
    pub(in crate::participant) fn new(task: String, request: String) -> Self {
        Self {
            task,
            request,
            items: Vec::new(),
            current: None,
            turn: Some(Turn::List),
            checked: false,
            fixed: false,
            news_turns: 0,
            failure_turns: 0,
            failures: Vec::new(),
            asked_failures: Vec::new(),
            news: (Vec::new(), Vec::new()),
            arrivals: Vec::new(),
            before: BTreeMap::new(),
            holding: false,
            read: None,
            made: Vec::new(),
            touched: Vec::new(),
            produced: Vec::new(),
            produced_by: Vec::new(),
            last: None,
            performed: 0,
            reply: None,
            ended: None,
        }
    }

    /// Take up the items the first plan listed, their apps resolved among
    /// `granted` (each app id with its title), at most
    /// [`MOST_OBLIGATIONS`]. A wait the plan set for itself rather than on
    /// an item becomes an item after those its steps were for. Returns how
    /// many it listed.
    pub(super) fn adopt_list(
        &mut self,
        declared: Obligations,
        granted: &[(String, String)],
        steps: &[Step],
    ) -> usize {
        let mut plans = declared.plans.into_iter();
        for text in declared.items.into_iter().take(MOST_OBLIGATIONS) {
            let plan = plans.next().unwrap_or_default();
            let mut item = Item::new(text, resolve_apps(&plan.apps, granted), plan.after);
            item.wait = plan.wait.and_then(|wait| Wait::planned(wait, None).ok());
            self.items.push(item);
        }
        if self.items.is_empty() {
            return 0;
        }
        let waits_listed = self.items.iter().any(|item| item.wait.is_some());
        if let Some(at) = steps
            .iter()
            .position(|step| matches!(step.action, StepAction::Wait(_)))
            && !waits_listed
            && self.items.len() < MOST_OBLIGATIONS
            && let Some(Step {
                action: StepAction::Wait(wait),
                ..
            }) = steps.get(at)
        {
            let before = steps.get(..at).unwrap_or_default();
            let after = before
                .iter()
                .filter_map(|step| step.serves)
                .max()
                .unwrap_or_else(|| usize::from(!before.is_empty()))
                .min(self.items.len());
            let mut wait = wait.clone();
            wait.rest = None;
            let text = format!(
                "wait for {}",
                wait.contains.as_deref().map_or_else(
                    || "what is to be sent".to_owned(),
                    |wanted| format!("{wanted:?}")
                )
            );
            let mut item = Item::new(
                text,
                Vec::new(),
                (after > 0).then_some(after).into_iter().collect(),
            );
            item.wait = Some(wait);
            self.items.insert(after, item);
            // The items after it moved one along, and need it: the plan
            // waited before their steps.
            let number = after.saturating_add(1);
            for (index, item) in self.items.iter_mut().enumerate() {
                if index == after {
                    continue;
                }
                for needed in &mut item.after {
                    if *needed > after {
                        *needed = needed.saturating_add(1);
                    }
                }
                if index > after && !item.after.contains(&number) {
                    item.after.push(number);
                }
            }
        }
        self.turn = None;
        self.items.len()
    }

    /// The next thing to do, at `now`: the first pending item, in the order
    /// listed, whose `after` items let it on. A wait among them is held
    /// from then on and the items after it go on; while only held waits and
    /// the items that need them are left, the task sleeps. Then the check,
    /// once; then the end.
    pub(super) fn next(&mut self, now: Instant) -> Next {
        let mut first_pending = None;
        let mut waiting = false;
        for at in 0..self.items.len() {
            let Some(item) = self.items.get(at) else {
                continue;
            };
            match item.state {
                State::Waiting => {
                    waiting = true;
                    continue;
                }
                State::Pending => {}
                _ => continue,
            }
            let held = item.after.iter().any(|number| {
                number
                    .checked_sub(1)
                    .filter(|index| *index != at)
                    .and_then(|index| self.items.get(index))
                    .is_some_and(|needed| !needed.lets_on())
            });
            if held {
                first_pending.get_or_insert(at);
                continue;
            }
            if item.wait.is_some() {
                self.arm(at, now);
                waiting |= self
                    .items
                    .get(at)
                    .is_some_and(|item| item.state == State::Waiting);
                continue;
            }
            return Next::Item(at);
        }
        if waiting {
            return Next::Hold;
        }
        // Items that only wait on each other (an `after` naming a later
        // item both ways) run in the order listed.
        if let Some(at) = first_pending {
            if self.items.get(at).is_some_and(|item| item.wait.is_some()) {
                self.arm(at, now);
                return Next::Hold;
            }
            return Next::Item(at);
        }
        if self.checked {
            Next::Finish
        } else {
            Next::Check
        }
    }

    /// Hold the wait of the item at `at` from `now`: over at once when a
    /// message others sent during the task already answers it.
    fn arm(&mut self, at: usize, now: Instant) {
        let Some(item) = self.items.get_mut(at) else {
            return;
        };
        let Some(within) = item.wait.as_ref().map(|wait| wait.within) else {
            return;
        };
        item.state = State::Waiting;
        item.armed = Some(Armed {
            until: now.checked_add(within).unwrap_or(now),
            count: 1,
        });
        self.take_arrivals();
    }

    /// End every held wait that a kept message answers, each with its own
    /// message, in the order listed. Returns whether one ended.
    pub(super) fn take_arrivals(&mut self) -> bool {
        let mut ended = false;
        for at in 0..self.items.len() {
            let Some(wait) = self
                .items
                .get(at)
                .filter(|item| item.state == State::Waiting)
                .and_then(|item| item.wait.clone())
            else {
                continue;
            };
            let Some(index) = self
                .arrivals
                .iter()
                .position(|arrival| answers(&wait, arrival))
            else {
                continue;
            };
            let arrival = self.arrivals.remove(index);
            if let Some(item) = self.items.get_mut(at) {
                tracing::debug!(
                    item = at.saturating_add(1),
                    "agent: a held wait is answered"
                );
                item.state = State::Done;
                item.armed = None;
                item.result = Some(bounded(
                    &format!("arrived: {}", arrived_text(std::slice::from_ref(&arrival))),
                    MOST_RESULT_CHARS,
                ));
                item.arrived = Some(arrival);
                ended = true;
            }
        }
        ended
    }

    /// The held waits that ran out by `now` wait again while the task may;
    /// one that waited [`MOST_WAITS`] times could not be done. Returns
    /// whether one ended.
    pub(super) fn run_out(&mut self, now: Instant) -> bool {
        let mut ended = false;
        for (at, item) in self.items.iter_mut().enumerate() {
            let (Some(armed), Some(within)) = (
                item.armed.filter(|_| item.state == State::Waiting),
                item.wait.as_ref().map(|wait| wait.within),
            ) else {
                continue;
            };
            if now < armed.until {
                continue;
            }
            if armed.count < MOST_WAITS {
                item.armed = Some(Armed {
                    until: now.checked_add(within).unwrap_or(now),
                    count: armed.count.saturating_add(1),
                });
                continue;
            }
            item.armed = None;
            item.state = State::Failed(format!(
                "nothing it waited for arrived ({MOST_WAITS} waits)"
            ));
            item.result = Some("nothing arrived.".to_owned());
            self.failures.push(at);
            ended = true;
        }
        ended
    }

    /// When the first held wait runs out, if one is held.
    pub(super) fn held_until(&self) -> Option<Instant> {
        self.items
            .iter()
            .filter(|item| item.state == State::Waiting)
            .filter_map(|item| item.armed.map(|armed| armed.until))
            .min()
    }

    /// What the first held wait waits for, as a waiting agent shows it.
    pub(super) fn held_activity(&self) -> Option<String> {
        let wait = self
            .items
            .iter()
            .find(|item| item.state == State::Waiting)?
            .wait
            .as_ref()?;
        let place = match wait.apps.as_slice() {
            [] => String::new(),
            apps => format!(
                " in {}",
                apps.iter()
                    .map(|app| short_app(app))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
        };
        let holding = wait
            .contains
            .as_ref()
            .map(|wanted| format!(" containing {wanted:?}"))
            .unwrap_or_default();
        Some(format!("Waiting for a message{place}{holding}"))
    }

    /// The item at `at` starts.
    pub(super) fn start(&mut self, at: usize) {
        if let Some(item) = self.items.get_mut(at) {
            item.state = State::Running;
            item.retry = None;
        }
        self.current = Some(at);
        self.reply = None;
    }

    /// Who plans now, as the logs name it: the coordinator, on its turns;
    /// else the running item's agent, under the task id with the item's own
    /// suffix.
    pub(in crate::participant) fn role(&self) -> PlanRole {
        let coordinator = |item| PlanRole {
            role: "coordinator",
            item,
            task: self.task.clone(),
        };
        let Some(at) = self.current.filter(|_| self.turn.is_none()) else {
            return coordinator(None);
        };
        let number = at.saturating_add(1);
        match self.items.get(at) {
            Some(item) => PlanRole {
                role: "item",
                item: Some(number),
                task: format!(
                    "{}/item-{number}{}",
                    self.task,
                    if item.redone { "-redo" } else { "" }
                ),
            },
            None => coordinator(Some(number)),
        }
    }

    /// The checklist as a plan is shown it, each item with where it is;
    /// `mine` is the running item's own.
    pub(super) fn checklist(&self, mine: Option<usize>) -> String {
        self.items
            .iter()
            .enumerate()
            .map(|(at, item)| {
                let number = at.saturating_add(1);
                let state = match &item.state {
                    State::Pending => "to do".to_owned(),
                    State::Running if Some(at) == mine => "yours".to_owned(),
                    State::Running => "running".to_owned(),
                    State::Waiting => "waiting".to_owned(),
                    State::Done => "done".to_owned(),
                    State::SetAside(why) => format!("set aside: {why}"),
                    State::Failed(why) if item.waived => {
                        format!("not done, the items after it go on: {why}")
                    }
                    State::Failed(why) => format!("not done: {why}"),
                };
                format!("{number}. {} ({state})", item.text)
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// What the items that ended did, a line each, and with `found` what
    /// each read and its steps answered, within [`MOST_RESULTS_CHARS`]
    /// (the oldest findings shortened first).
    pub(super) fn results(&self, found: bool) -> String {
        let mut entries: Vec<(String, String)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(at, item)| {
                let result = item.result.as_ref()?;
                let line = format!("\n- {}. {}: {result}", at.saturating_add(1), item.text);
                let finding = item
                    .found
                    .as_ref()
                    .filter(|_| found)
                    .map(|found| format!("\n  What it read and its steps answered: {found}"))
                    .unwrap_or_default();
                Some((line, finding))
            })
            .collect();
        let length = |entries: &[(String, String)]| -> usize {
            entries
                .iter()
                .map(|(line, finding)| line.chars().count().saturating_add(finding.chars().count()))
                .sum()
        };
        for at in 0..entries.len() {
            let over = length(&entries).saturating_sub(MOST_RESULTS_CHARS);
            if over == 0 {
                break;
            }
            if let Some((_, finding)) = entries.get_mut(at) {
                // One character more for the ellipsis.
                let keep = finding
                    .chars()
                    .count()
                    .saturating_sub(over.saturating_add(1));
                *finding = if keep < 80 {
                    String::new()
                } else {
                    format!("{}…", finding.chars().take(keep).collect::<String>())
                };
            }
        }
        entries
            .into_iter()
            .map(|(line, finding)| line + &finding)
            .collect()
    }

    /// What the agent of the item at `at` is told after its item: the
    /// request, the checklist, what the items before it did and found, the
    /// coordinator's note, and how it ends.
    pub(super) fn item_note(&self, at: usize) -> String {
        let Some(item) = self.items.get(at) else {
            return String::new();
        };
        let number = at.saturating_add(1);
        let mut note = format!(
            "{ROLE_INTRO}, each item by a fresh agent of its own; you are the agent of item \
             {number} only: {}.\nDo your item yourself, with whatever it needs: read the records \
             it needs (call `read_view` for what a view answers), open a record that is not open \
             yet with its open, and look at the pictures and frames the records and steps show. \
             What the items before you found is below; the records hold the rest.\nThe original \
             request, for reference: {}\nThe items: {}.",
            item.text,
            bounded(&self.request, MOST_REQUEST_CHARS),
            self.checklist(Some(at)),
        );
        if let Some(read) = &self.read {
            let _ = write!(
                note,
                "\nWhat the coordinator read while it listed the items: {read}"
            );
        }
        let results = self.results(true);
        if !results.is_empty() {
            let _ = write!(
                note,
                "\nWhat the items that ended did and found (use these values):{results}"
            );
        }
        if let Some(told) = &item.note {
            let _ = write!(note, "\nThe coordinator's note for this item: {told}");
        }
        if let Some(retry) = &item.retry {
            let _ = write!(note, "\n{retry}");
        }
        note.push_str(
            "\nPlan only the steps of your item; list no `obligations`. When it is done, or the \
             records show it was done already, plan no steps and say so in `reply`. Reply \
             \"cannot: <why>\" only when, after trying (reading or opening what it needs), it \
             cannot be done.",
        );
        note
    }

    /// What a coordinator's turn after the first is told after its own
    /// text: the checklist with what each item did, and the steps the
    /// running item's agent is about to take.
    pub(super) fn state_note(&self, pending: &[String]) -> String {
        let mut note = format!(
            "{ROLE_INTRO}, each item by a fresh agent of its own, and you coordinate them.\nThe \
             items: {}.",
            self.checklist(None)
        );
        let results = self.results(false);
        if !results.is_empty() {
            let _ = write!(note, "\nWhat the items that ended did:{results}");
        }
        if let (Some(at), false) = (self.current, pending.is_empty()) {
            let _ = write!(
                note,
                "\nItem {}'s agent is about to: {}.",
                at.saturating_add(1),
                pending.join("; ")
            );
        }
        note
    }

    /// The item listed already that `text` repeats, if one does
    /// ([`same_item`]): open, running, waiting, done or not done. A set
    /// aside item may be listed again.
    fn listed_already(&self, text: &str) -> Option<usize> {
        self.items.iter().position(|item| {
            !matches!(item.state, State::SetAside(_)) && same_item(&item.text, text)
        })
    }

    /// Take up what a coordinator's news, failure or check turn declared:
    /// notes on items (a hint to the running one, a start for a pending
    /// one, the running again of an ended one, once each), items added (at
    /// most [`MOST_OBLIGATIONS`] in all, marked fixes on the check; one
    /// that repeats an item listed already is not added again, and one that
    /// repeats an item not done is that item done again) and pending items
    /// set aside. Returns the hint for the running item's agent.
    pub(super) fn adjust(
        &mut self,
        declared: Obligations,
        granted: &[(String, String)],
        check: bool,
    ) -> Option<String> {
        let mut hint = None;
        for (number, told) in declared.adjust {
            if number == 0 {
                continue;
            }
            if let Some(running) = self.note(number.saturating_sub(1), told) {
                hint = Some(running);
            }
        }
        let mut plans = declared.plans.into_iter();
        for text in declared.items {
            let plan = plans.next().unwrap_or_default();
            if let Some(at) = self.listed_already(&text) {
                let failed = self
                    .items
                    .get(at)
                    .is_some_and(|item| matches!(item.state, State::Failed(_)));
                tracing::debug!(
                    item = at.saturating_add(1),
                    failed,
                    "agent: an item listed again is not added"
                );
                if failed {
                    self.note(at, "it was listed again".to_owned());
                }
                continue;
            }
            if self.items.len() >= MOST_OBLIGATIONS {
                break;
            }
            let mut item = Item::new(text, resolve_apps(&plan.apps, granted), plan.after);
            item.wait = plan.wait.and_then(|wait| Wait::planned(wait, None).ok());
            item.fixup = check;
            self.items.push(item);
        }
        for (number, why) in declared.set_aside {
            if let Some(item) = number
                .checked_sub(1)
                .and_then(|at| self.items.get_mut(at))
                .filter(|item| matches!(item.state, State::Pending | State::Waiting))
            {
                item.state = State::SetAside(why);
                item.armed = None;
            }
        }
        if check {
            self.fixed = true;
        }
        hint
    }

    /// The coordinator's note `told` on the item at `at`: returned when it
    /// is the running item's, kept for a pending one, and an item that
    /// ended is done again by a fresh agent with it, once.
    fn note(&mut self, at: usize, told: String) -> Option<String> {
        let running = self.current == Some(at);
        let item = self.items.get_mut(at)?;
        match item.state {
            State::Running if running => return Some(told),
            State::Pending => item.note = Some(told),
            State::Done | State::Failed(_) | State::SetAside(_) | State::Waiting
                if !item.redone =>
            {
                let before = item
                    .result
                    .take()
                    .map(|result| format!(" What it did before: {result}"))
                    .unwrap_or_default();
                item.note = Some(format!("Do this item again: {}{before}", sentence(&told)));
                item.state = State::Pending;
                item.armed = None;
                item.waived = false;
                item.redone = true;
            }
            State::Running
            | State::Waiting
            | State::Done
            | State::Failed(_)
            | State::SetAside(_) => {
                tracing::debug!(
                    item = at.saturating_add(1),
                    "agent: a note on an item done again already is not taken"
                );
            }
        }
        None
    }

    /// The items the failure turn being planned is about went on without
    /// whichever of them is not done again: the items after them run.
    pub(super) fn waive_asked(&mut self) {
        for at in std::mem::take(&mut self.asked_failures) {
            if let Some(item) = self
                .items
                .get_mut(at)
                .filter(|item| matches!(item.state, State::Failed(_)))
            {
                item.waived = true;
            }
        }
    }

    /// What the coordinator's failure turn is told: each item that could not
    /// be done, and why. The items are taken as asked about.
    pub(super) fn ask_failures(&mut self) -> String {
        let failures = std::mem::take(&mut self.failures);
        let mut text = String::new();
        for at in &failures {
            if let Some(item) = self.items.get(*at)
                && let State::Failed(why) = &item.state
            {
                let again = if item.redone {
                    " It was done again already."
                } else {
                    ""
                };
                let _ = writeln!(
                    text,
                    "Item {} ({}) could not be done: {}{again}",
                    at.saturating_add(1),
                    item.text,
                    sentence(why)
                );
            }
        }
        self.asked_failures = failures;
        text
    }

    /// The running item ended with `outcome`, its task `running` when it
    /// had one: what it did and found is taken in, for the items after it.
    /// An item that could not be done goes to the coordinator.
    pub(super) fn item_ended(
        &mut self,
        outcome: &Result<String, String>,
        running: Option<Running>,
    ) {
        let Some(at) = self.current.take() else {
            return;
        };
        let reply = self.reply.take();
        let mut wrote = String::new();
        let mut found = None;
        if let Some(running) = running {
            for route in &running.made {
                if !self.made.contains(route) {
                    self.made.push(route.clone());
                }
            }
            for route in &running.touched {
                if !self.touched.contains(route) {
                    self.touched.push(route.clone());
                }
            }
            let new = running
                .produced
                .get(self.produced.len()..)
                .unwrap_or_default();
            let steps: Vec<String> = running.invoked.iter().map(step_brief).collect();
            if !steps.is_empty() {
                let _ = write!(wrote, " Ran: {}.", steps.join("; "));
            }
            if !new.is_empty() {
                let _ = write!(wrote, " Produced {}.", new.join(", "));
            }
            let observed = running.observations();
            if !observed.is_empty() {
                found = Some(newest_within(&observed.join("\n  "), MOST_FOUND_CHARS));
            }
            self.produced.clone_from(&running.produced);
            self.produced_by.clone_from(&running.produced_by);
            if running.last.is_some() {
                self.last.clone_from(&running.last);
            }
            self.performed = self.performed.saturating_add(running.performed);
        }
        let said = reply
            .as_deref()
            .map(str::trim)
            .filter(|reply| !reply.is_empty());
        let cannot = said.and_then(cannot_why);
        let (state, how) = match (outcome, cannot) {
            (Err(error), _) => (State::Failed(bounded(error, MOST_RESULT_CHARS)), "not done"),
            (Ok(_), Some(why)) => (State::Failed(why), "could not be done"),
            (Ok(_), None) => (State::Done, "done"),
        };
        let failed = matches!(state, State::Failed(_));
        let said = said
            .map(|said| format!(" It said: {}", sentence(said)))
            .unwrap_or_default();
        if let Some(item) = self.items.get_mut(at) {
            item.state = state;
            item.result = Some(bounded(&format!("{how}.{said}{wrote}"), MOST_RESULT_CHARS));
            item.found = found;
        }
        if failed {
            self.failures.push(at);
        }
    }

    /// Keep `arrived`, messages others sent, for the waits of the items.
    pub(super) fn remember(&mut self, arrived: Vec<Arrival>) {
        self.arrivals.extend(arrived);
        let over = self.arrivals.len().saturating_sub(MOST_KEPT_ARRIVALS);
        self.arrivals.drain(..over);
    }

    /// Whether `arrival` is a wait's: one pending or held waits for it, or
    /// one ended on it.
    pub(super) fn awaited(&self, arrival: &Arrival) -> bool {
        self.items.iter().any(|item| {
            let waits = matches!(item.state, State::Pending | State::Waiting)
                && item
                    .wait
                    .as_ref()
                    .is_some_and(|wait| answers(wait, arrival));
            let took = item.arrived.as_ref().is_some_and(|taken| {
                taken.route == arrival.route
                    && taken.lines.iter().any(|line| arrival.lines.contains(line))
            });
            waits || took
        })
    }

    /// The task's result: what was done, what was set aside and what was
    /// left undone; a failure when an item could not be done.
    pub(super) fn outcome(&self) -> Result<String, String> {
        let listed = |wanted: fn(&State) -> Option<String>| -> Vec<String> {
            self.items
                .iter()
                .enumerate()
                .filter_map(|(at, item)| {
                    let why = wanted(&item.state)?;
                    let number = at.saturating_add(1);
                    Some(if why.is_empty() {
                        format!("{number}. {}", item.text)
                    } else {
                        format!("{number}. {} ({why})", item.text)
                    })
                })
                .collect()
        };
        let done = listed(|state| (*state == State::Done).then(String::new));
        let undone = listed(|state| match state {
            State::Failed(why) => Some(why.clone()),
            State::Pending | State::Running | State::Waiting => Some(String::new()),
            State::Done | State::SetAside(_) => None,
        });
        let set_aside = listed(|state| match state {
            State::SetAside(why) => Some(why.clone()),
            _ => None,
        });
        let mut text = format!(
            "Done: {} action{}.",
            self.performed,
            if self.performed == 1 { "" } else { "s" }
        );
        if !self.produced.is_empty() {
            let _ = write!(text, " Produced {}.", self.produced.join(", "));
        }
        if !done.is_empty() {
            let _ = write!(text, " Items done: {}.", done.join("; "));
        }
        if !undone.is_empty() {
            let _ = write!(text, " Left undone: {}.", undone.join("; "));
        }
        if !set_aside.is_empty() {
            let _ = write!(text, " Set aside: {}.", set_aside.join("; "));
        }
        let failed = self
            .items
            .iter()
            .enumerate()
            .find_map(|(at, item)| match &item.state {
                State::Failed(why) => Some(format!(
                    "Item {} ({}) could not be done: {why}.",
                    at.saturating_add(1),
                    item.text
                )),
                _ => None,
            });
        match failed {
            Some(failed) => Err(format!("{failed} {text}")),
            None => Ok(text),
        }
    }

    /// The items for the plan log: each with where it is, why, its apps,
    /// what it follows and waits for, and what it did.
    pub(in crate::participant) fn logged(&self) -> serde_json::Value {
        self.items
            .iter()
            .enumerate()
            .map(|(at, item)| {
                let (state, reason) = match &item.state {
                    State::Pending => ("open", None),
                    State::Running => ("running", None),
                    State::Waiting => ("waiting", None),
                    State::Done => ("done", None),
                    State::SetAside(why) => ("set aside", Some(why.as_str())),
                    State::Failed(why) => ("not done", Some(why.as_str())),
                };
                serde_json::json!({
                    "item": at.saturating_add(1),
                    "text": item.text,
                    "state": state,
                    "reason": reason,
                    "apps": item.apps,
                    "after": item.after,
                    "wait": item.wait.as_ref().map(|wait| wait.contains.clone().unwrap_or_default()),
                    "result": item.result,
                    "note": item.note,
                    "redone": item.redone,
                    "waived": item.waived,
                    "fixup": item.fixup,
                })
            })
            .collect()
    }
}

/// Why an item's agent said it cannot do its item, when its reply says so
/// ("cannot: …", "Cannot …").
pub(super) fn cannot_why(reply: &str) -> Option<String> {
    let reply = reply.trim();
    reply.to_lowercase().starts_with("cannot").then(|| {
        reply
            .get(6..)
            .unwrap_or_default()
            .trim_start_matches([':', ' '])
            .to_owned()
    })
}

/// Whether `new`, an item's text, repeats `listed`, an item listed
/// already: the same words but for joining ones ([`ITEM_FILLER`]), each
/// covering most of the other's, and no number in `new` that `listed`
/// lacks. A number says which ("LR-2041" is not "LR-2042") or what to set
/// ("set the frame size to 1920x1080" fixes "set the frame size").
pub(super) fn same_item(listed: &str, new: &str) -> bool {
    let words = |text: &str| -> BTreeSet<String> {
        wait_words(text)
            .filter(|word| !ITEM_FILLER.contains(&word.as_str()))
            .collect()
    };
    let (listed, new) = (words(listed), words(new));
    if listed.is_empty() || new.is_empty() {
        return false;
    }
    let numbered = |word: &&String| word.chars().any(|c| c.is_ascii_digit());
    if new
        .iter()
        .filter(numbered)
        .any(|number| !listed.contains(number))
    {
        return false;
    }
    let shared = listed.intersection(&new).count();
    let (fewer, more) = (listed.len().min(new.len()), listed.len().max(new.len()));
    shared.saturating_mul(4) >= fewer.saturating_mul(3)
        && shared.saturating_mul(3) >= more.saturating_mul(2)
}

/// Whether `arrival` is what `wait` waits for: in an app it covers, and
/// holding its text.
pub(super) fn answers(wait: &Wait, arrival: &Arrival) -> bool {
    let app = arrival.route.app_id.as_str();
    let covered = MESSAGE_APPS.contains(&app)
        && (wait.apps.is_empty() || wait.apps.iter().any(|wanted| wanted == app));
    if !covered || arrival.lines.is_empty() {
        return false;
    }
    let Some(wanted) = &wait.contains else {
        return true;
    };
    let mut said: String = [arrival.title.as_deref(), arrival.from.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    for line in &arrival.lines {
        said.push('\n');
        said.push_str(line);
    }
    holds_wanted(&said, wanted)
}

/// What arrived, as the items after a wait are told it.
pub(super) fn arrived_text(arrivals: &[Arrival]) -> String {
    arrivals
        .iter()
        .map(|arrival| {
            let from = arrival
                .from
                .as_deref()
                .map(|from| format!(" from {from}"))
                .unwrap_or_default();
            format!(
                "in {}{from}: {}",
                short_app(&arrival.route.app_id),
                crate::values::annotate_text(&arrival.lines.join(" / "))
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The app ids of `granted` (each id with its title) that `named`, apps
/// as a model wrote them (`Sheets`, `chat`, `so.tilo.word-processor`, `Word
/// Processor`, `pdf` for the PDF viewer), stand for.
pub(super) fn resolve_apps(named: &[String], granted: &[(String, String)]) -> Vec<String> {
    let plain = |text: &str| {
        let text = text.trim().to_lowercase().replace([' ', '_'], "-");
        text.strip_suffix('s')
            .map_or_else(|| text.clone(), str::to_owned)
    };
    let mut apps: Vec<String> = Vec::new();
    for name in named {
        let wanted = plain(name);
        let known_as = |id: &str, title: &str| {
            [
                plain(id),
                plain(id.rsplit('.').next().unwrap_or(id)),
                plain(title),
            ]
        };
        let exact = granted
            .iter()
            .find(|(id, title)| known_as(id, title).contains(&wanted));
        // A model's short name for an app ("pdf" for the PDF viewer) is the
        // first word of its own.
        let first_word = || {
            granted.iter().find(|(id, title)| {
                !wanted.is_empty()
                    && known_as(id, title)
                        .iter()
                        .any(|known| known.split('-').next() == Some(wanted.as_str()))
            })
        };
        let found = message_app(name)
            .map(str::to_owned)
            .or_else(|| exact.or_else(first_word).map(|(id, _)| id.clone()));
        if let Some(app) = found.filter(|app| granted.iter().any(|(id, _)| id == app))
            && !apps.contains(&app)
        {
            apps.push(app);
        }
    }
    apps
}

/// `text` as a sentence: ending in a full stop, a question or an
/// exclamation mark, adding a full stop where it ends in none.
pub(super) fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.ends_with(['.', '!', '?']) {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}

/// `text` within `most` characters.
pub(super) fn bounded(text: &str, most: usize) -> String {
    let text = text.trim();
    if text.chars().count() > most {
        format!("{}…", text.chars().take(most).collect::<String>())
    } else {
        text.to_owned()
    }
}

/// The end of `text` within `most` characters: what a task read last is
/// what it went on with.
pub(super) fn newest_within(text: &str, most: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count > most {
        format!(
            "…{}",
            text.chars()
                .skip(count.saturating_sub(most))
                .collect::<String>()
        )
    } else {
        text.to_owned()
    }
}
