//! A running task: its steps, what they answered and produced, and the
//! turns it is planned again on once they ran (reads answered, an export
//! missing, its work checked, stale exports run again).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_browser_peer::DocumentRoute;

use super::NOTHING_LEFT;
use super::obligations::{Checklist, OBLIGATIONS_LEFT};
use super::repairs::Repairs;
use super::waits::{Arrival, Waiting, Waits, Woken};
use crate::planner::{
    MOST_OBSERVED, MOST_WAITS, Step, StepAction, Wait, observation, rendered_picture,
};

/// A turn a task is planned again on once every step ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FollowUp {
    /// After a plan of reads alone, with what they found.
    AfterReads,
    /// The export the request asks for, when the steps made none: only
    /// export operations of its plan run.
    Exporting,
    /// The check of the result against the request: a check that cannot
    /// plan leaves the task done, never failed.
    Checking,
    /// The exports a later step's change made stale, run again as they ran.
    Reexporting,
    /// The obligations still open when the task would finish
    /// ([`Checklist::remind`]).
    Obligations,
    /// The rest of the task after a step failed ([`Repairs`]): taken once
    /// per repair.
    Repairing,
}

/// The follow-up turns a task took, each at most once but a repair, and the
/// one being planned.
#[derive(Default)]
pub(super) struct FollowUps {
    pub(super) taken: Vec<FollowUp>,
    pub(super) turn: Option<FollowUp>,
}

pub(super) struct Running {
    pub(super) id: String,
    /// The task as it was given, which a re-plan is shown beside the rest.
    pub(super) request: String,
    pub(super) steps: VecDeque<Step>,
    pub(super) performed: usize,
    /// What the latest step that produced content returned: the content a
    /// step that [carries](Step::carries) is given. A step that makes no
    /// content (a read, a view, an edit), opening a record or planning
    /// again keeps it.
    pub(super) last: Option<ActionOutput>,
    /// Content addresses the task's steps produced, oldest first.
    pub(super) produced: Vec<String>,
    /// The app whose step made each of [`Self::produced`], in step.
    pub(super) produced_by: Vec<String>,
    /// What the task's steps answered, oldest first, at most
    /// [`MOST_OBSERVED`]: what the task is planned again with.
    pub(super) observed: VecDeque<Observed>,
    /// The failed steps it was planned around: a model's slip (an argument
    /// out of range, a stale target) is fixed rather than ending the task,
    /// a few times, and a step that cannot work still fails it.
    pub(super) repairs: Repairs,
    /// How many times it planned again for news ([`MOST_NEWS_REPLANS`]).
    pub(super) news_replans: u32,
    /// The steps a re-plan for news replaced, until that re-plan is made:
    /// one that answers nothing (no steps, "nothing offered fits") picks
    /// them up again rather than ending the task, so a wait planned a
    /// moment ago still waits.
    pub(super) news_resume: Option<Vec<Step>>,
    /// Its waits for messages ([`StepAction::Wait`]).
    pub(super) waits: Waits,
    /// The records its steps opened or acted on: what a re-plan is always
    /// offered, whatever the rest of the task calls them.
    pub(super) touched: Vec<DocumentRoute>,
    /// The records its steps opened new, oldest first: where an import
    /// naming no record goes ([`Running::record_for`]).
    pub(super) made: Vec<DocumentRoute>,
    /// How many of its performed steps did something (not only opened a
    /// record).
    pub(super) acted: usize,
    /// Whether a step changed a record or made content: a plan of reads
    /// alone has done nothing the request asked for yet.
    pub(super) changed: bool,
    /// The turns the task was planned again on once its steps ran.
    pub(super) follow_ups: FollowUps,
    /// The exports that ran, each with the step count when it did.
    pub(super) exports: Vec<(usize, Step)>,
    /// The step count when each app's record was last changed.
    pub(super) edited_at: BTreeMap<String, usize>,
    /// Whether a model planned any of it: steps written out, or one
    /// operation the task stated outright, are neither re-planned nor
    /// checked.
    pub(super) by_model: bool,
    /// The steps that ran, briefly, oldest first: what a repair must not
    /// do again.
    pub(super) ran: Vec<String>,
    /// The operations that ran, oldest first: a check that plans one again
    /// with the same arguments would add a second slide or event.
    pub(super) invoked: Vec<Step>,
    /// The pictures its steps and its plans' reads rendered (a canvas
    /// look), by content address, oldest first, at most [`MOST_LOOKED`].
    pub(super) looked: Vec<String>,
    /// Apps an import of the task was refused in for what the record held:
    /// its plans are offered a new document of each
    /// ([`PlanContext::new_documents`]).
    pub(super) new_documents: Vec<String>,
    /// What its request asks for, as its first plan listed it, ticked as
    /// the steps that fulfil it succeed.
    pub(super) checklist: Checklist,
}

/// The most rendered pictures a task keeps the addresses of.
pub(super) const MOST_LOOKED: usize = 16;

impl FollowUps {
    pub(super) fn took(&self, follow_up: FollowUp) -> bool {
        self.taken.contains(&follow_up)
    }

    pub(super) fn take(&mut self, follow_up: FollowUp) {
        self.taken.push(follow_up);
        self.turn = Some(follow_up);
    }
}

impl Running {
    /// The reply to a task whose steps all ran: how many, and the content
    /// they produced.
    pub(super) fn done(&self) -> String {
        let performed = self.performed;
        let produced = if self.produced.is_empty() {
            String::new()
        } else {
            format!(" Produced {}.", self.produced.join(", "))
        };
        let waited = if self.waits.in_vain {
            " Nothing it waited for arrived."
        } else {
            ""
        };
        format!(
            "Done: {performed} action{}.{produced}{waited}{}",
            if performed == 1 { "" } else { "s" },
            self.checklist.outcome()
        )
    }

    /// Hold the task for `wait` until `until`, `before` being what the
    /// message records it waits on hold now. Returns `false`, leaving it
    /// unheld, once it waited [`MOST_WAITS`] times.
    pub(super) fn begin_wait(
        &mut self,
        wait: Wait,
        until: Instant,
        before: BTreeMap<DocumentRoute, Arc<str>>,
    ) -> bool {
        if self.waits.count >= MOST_WAITS {
            return false;
        }
        self.waits.count = self.waits.count.saturating_add(1);
        self.waits.current = Some(Waiting {
            wait,
            since: Instant::now(),
            until,
            before,
            elsewhere: Vec::new(),
            changed: Vec::new(),
            unmatched: BTreeMap::new(),
        });
        true
    }

    /// The wait is over: `arrival` came, or with `None` it ran out. The rest
    /// is planned again with what arrived (or that nothing did) beside the
    /// request, before anything else the plan left.
    pub(super) fn end_wait(&mut self, arrival: Option<&Arrival>) {
        let Some(waiting) = self.waits.current.take() else {
            return;
        };
        self.waits.in_vain = arrival.is_none();
        self.waits.replan = true;
        let brief = match arrival {
            Some(arrival) => waiting.arrived_brief(std::slice::from_ref(arrival)),
            None => waiting.nothing_brief(MOST_WAITS.saturating_sub(self.waits.count)),
        };
        self.steps.push_front(Step {
            app: String::new(),
            record: None,
            action: StepAction::Continue(brief),
            carries: None,
            serves: None,
        });
        self.waits.woken = arrival.map(|arrival| Woken {
            first: arrival.route.clone(),
            waiting,
        });
    }

    /// The wait that ended on what arrived, while the plan after it is not
    /// made yet: its re-plan is the next step.
    pub(super) fn woken(&self) -> Option<&Woken> {
        self.waits.woken.as_ref().filter(|_| {
            matches!(
                self.steps.front().map(|step| &step.action),
                Some(StepAction::Continue(_))
            )
        })
    }

    /// Tell the plan after the wait everything that arrived in the records
    /// it waited on, `arrivals`, the first being what ended it: a message
    /// often comes in a burst, and a second part a moment after the first
    /// is part of what was waited for, not news that undoes the plan.
    /// `texts`, what those records hold now, is what that plan was shown.
    pub(super) fn rewake(
        &mut self,
        arrivals: &[Arrival],
        texts: BTreeMap<DocumentRoute, Arc<str>>,
    ) {
        let Some(woken) = self.woken() else {
            return;
        };
        if arrivals.is_empty() {
            return;
        }
        let brief = woken.waiting.arrived_brief(arrivals);
        if let Some(Step {
            action: StepAction::Continue(rest),
            ..
        }) = self.steps.front_mut()
        {
            *rest = brief;
        }
        self.waits.seen = texts;
    }

    /// Whether `text`, what the message record at `route` holds now, has
    /// nothing a plan after a wait was not already shown.
    pub(super) fn shown_already(&self, route: &DocumentRoute, text: &str) -> bool {
        let Some(seen) = self.waits.seen.get(route) else {
            return false;
        };
        let seen: std::collections::HashSet<&str> = seen.lines().map(str::trim).collect();
        text.lines()
            .map(str::trim)
            .all(|line| line.is_empty() || seen.contains(line))
    }
}

impl Running {
    /// `step` ran and answered `output`, making `content`; `edited` when it
    /// changed a record. Only an operation changes one: opening a new record
    /// publishes it, but a task that only opened has done none of its work.
    pub(super) fn ran_step(
        &mut self,
        step: &Step,
        output: Option<ActionOutput>,
        content: Vec<String>,
        edited: bool,
    ) {
        self.performed = self.performed.saturating_add(1);
        let invoked = matches!(step.action, StepAction::Invoke { .. });
        let edited = edited && invoked;
        self.repairs.ran(step);
        if invoked {
            self.acted = self.acted.saturating_add(1);
            self.invoked.push(step.clone());
            if let Some(number) = step.serves {
                self.checklist.tick(number);
            }
        }
        if edited {
            self.edited_at.insert(step.app.clone(), self.performed);
        }
        if !content.is_empty() && is_export(step) {
            self.exports.push((self.performed, step.clone()));
        }
        self.changed |= edited || !content.is_empty();
        if edited {
            // What was read of this app's record before shows it as it was:
            // a check planned with "1 slide, empty" adds the slides again.
            for observed in &mut self.observed {
                observed.before_change |= observed.app == step.app;
            }
        }
        self.ran.push(step_brief(step));
        if let Some(output) = output {
            if edited {
                self.edit_answered(step, output);
            } else {
                self.answered(step, output);
            }
        }
        self.produced_by
            .extend(std::iter::repeat_n(step.app.clone(), content.len()));
        self.produced.extend(content);
    }

    /// The record a step naming none acts on, among `open`, the records
    /// the agent has open, `blank` saying which of them hold only what a
    /// new record starts with. An import replaces what a document holds, so
    /// one naming no record goes only to a blank record this task made (the
    /// one the host opened for it): never one another person or task is
    /// writing, nor one the task already filled. Other steps act on the
    /// latest of their app the task made, else the latest it acted on, else
    /// one open, written ones first; never one `foreign` says is another
    /// member's written document. `None` opens a new record.
    pub(super) fn record_for<'a>(
        &self,
        step: &Step,
        open: impl IntoIterator<Item = &'a DocumentRoute>,
        blank: impl Fn(&DocumentRoute) -> bool,
        foreign: impl Fn(&DocumentRoute) -> bool,
    ) -> Option<DocumentRoute> {
        let of_app = |route: &&DocumentRoute| route.app_id == step.app;
        if imports(step) {
            return self
                .made
                .iter()
                .rev()
                .filter(of_app)
                .find(|route| blank(route))
                .cloned();
        }
        if let Some(route) = self.made.iter().rev().find(of_app) {
            return Some(route.clone());
        }
        if let Some(route) = self
            .touched
            .iter()
            .rev()
            .filter(of_app)
            .find(|route| !foreign(route))
        {
            return Some(route.clone());
        }
        let open: Vec<&DocumentRoute> = open
            .into_iter()
            .filter(of_app)
            .filter(|route| !foreign(route))
            .collect();
        open.iter()
            .find(|route| !blank(route))
            .or_else(|| open.first())
            .map(|route| (*route).clone())
    }

    /// Whether `route` is a record this task made or acted on.
    pub(super) fn holds(&self, route: &DocumentRoute) -> bool {
        self.made.contains(route) || self.touched.contains(route)
    }

    /// The content a placeholder in a step of `app` stands for: the latest
    /// another app's step made (the picture an export made, to place in a
    /// document), else the latest of all. A document's own export placed in
    /// itself is never what a plan means.
    pub(super) fn latest_from_elsewhere(&self, app: &str) -> Option<&String> {
        self.produced
            .iter()
            .zip(&self.produced_by)
            .rev()
            .find(|(_, by)| by.as_str() != app)
            .map(|(content, _)| content)
            .or_else(|| self.produced.last())
    }

    /// When every step ran: a model plan of reads alone is planned once
    /// more with what they found; a model plan that made or changed
    /// something is checked once against the request when `checks_work`.
    /// Steps written out or stated outright run as written.
    pub(super) fn plan_more(&mut self, checks_work: bool) {
        if !self.steps.is_empty() || self.acted == 0 || !self.by_model {
            return;
        }
        let rest = if !self.changed && !self.follow_ups.took(FollowUp::AfterReads) {
            self.follow_ups.take(FollowUp::AfterReads);
            READS_ANSWERED.to_owned()
        } else if self.changed
            && self.produced.is_empty()
            && asks_export(&self.request)
            && !self.follow_ups.took(FollowUp::Exporting)
        {
            // A plan that edited and stopped short of the export asked for
            // leaves nothing to hand on: every model class gets this turn.
            self.follow_ups.take(FollowUp::Exporting);
            EXPORT_MISSING.to_owned()
        } else if self.changed && checks_work && !self.follow_ups.took(FollowUp::Checking) {
            self.follow_ups.take(FollowUp::Checking);
            // What ran is said outright, so the check sees the slides it
            // added were added rather than adding them again.
            format!("{CHECK_WORK}{CHECK_RAN}{}", self.ran.join("; "))
        } else {
            // An export taken before a later step changed its record holds
            // the record as it was: it runs again, as it ran, once.
            let stale = self.stale_exports();
            if !stale.is_empty() && !self.follow_ups.took(FollowUp::Reexporting) {
                self.follow_ups.take(FollowUp::Reexporting);
                self.steps.extend(stale);
            }
            return;
        };
        self.steps.push_back(Step {
            app: String::new(),
            record: None,
            action: StepAction::Continue(rest),
            carries: None,
            serves: None,
        });
    }

    /// The exports that ran before a later step changed their app's record:
    /// of each record, only its latest export, and none when an export of
    /// it ran after that last change. The deliverable is what the plan
    /// exported last; re-running an earlier PNG after the final PPTX made
    /// the PNG the task's result.
    pub(super) fn stale_exports(&self) -> Vec<Step> {
        let mut stale: Vec<Step> = Vec::new();
        for (at, step) in self.exports.iter().rev() {
            let Some(edited) = self.edited_at.get(&step.app) else {
                continue;
            };
            let same_record = |other: &Step| other.app == step.app && other.record == step.record;
            let newer_ran = self
                .exports
                .iter()
                .any(|(later, other)| later > at && same_record(other));
            if edited > at && !newer_ran {
                stale.push(step.clone());
            }
        }
        stale.reverse();
        stale
    }

    /// When a follow-up plan found nothing offered fits after the task
    /// changed something: one check of what the request still asks, rather
    /// than ending on a `then` the model could not plan. Not after the check
    /// or the export turn, which end the task done. Returns whether it is
    /// queued.
    pub(super) fn check_left(&mut self) -> bool {
        if !self.changed
            || self.follow_ups.took(FollowUp::Checking)
            || matches!(
                self.follow_ups.turn,
                Some(FollowUp::Checking | FollowUp::Exporting | FollowUp::Obligations)
            )
        {
            return false;
        }
        self.follow_ups.take(FollowUp::Checking);
        self.steps.clear();
        self.steps.push_back(Step {
            app: String::new(),
            record: None,
            action: StepAction::Continue(format!("{CHECK_LEFT}{CHECK_RAN}{}", self.ran.join("; "))),
            carries: None,
            serves: None,
        });
        true
    }

    /// Whether a follow-up plan that failed with `error` leaves the task
    /// done rather than failed: a reminder of its obligations, as the task
    /// was finishing when it was reminded; otherwise only when its steps
    /// changed or made something (reads and opens alone are not the work),
    /// and the plan was the check or the export turn, or found nothing
    /// offered fits.
    pub(super) fn done_despite(&self, error: &str) -> bool {
        let finishing = matches!(
            self.follow_ups.turn,
            Some(FollowUp::Checking | FollowUp::Exporting)
        );
        self.follow_ups.turn == Some(FollowUp::Obligations)
            || (self.changed && (finishing || error.starts_with(NOTHING_LEFT)))
    }

    /// When the task would finish with obligations of a model's plan still
    /// open: plan it once more for them instead ([`Checklist::remind`]),
    /// told what ran so it does none of it again. Returns whether it is
    /// queued; past the reminders, the task finishes and its result names
    /// what was left undone.
    pub(super) fn remind_open(&mut self) -> bool {
        if !self.by_model || !self.checklist.remind() {
            return false;
        }
        tracing::debug!(task = %self.id, "agent: obligations are open; the task is planned for them");
        self.follow_ups.take(FollowUp::Obligations);
        self.steps.clear();
        self.steps.push_back(Step {
            app: String::new(),
            record: None,
            action: StepAction::Continue(format!("{OBLIGATIONS_LEFT}{}", self.ran.join("; "))),
            carries: None,
            serves: None,
        });
        true
    }

    /// Take up what a plan of the rest declared ([`Checklist::adopt`]): it
    /// judged the finished work when it is a reminder or a check.
    pub(super) fn adopt_obligations(&mut self, declared: crate::planner::Obligations) {
        let judged = matches!(
            self.follow_ups.turn,
            Some(FollowUp::Obligations | FollowUp::Checking)
        );
        self.checklist.adopt(declared, judged);
    }

    /// Take up again the steps a re-plan for news replaced, before what
    /// is left, once that re-plan answered nothing. Returns whether there
    /// were any.
    pub(super) fn resume_after_news(&mut self) -> bool {
        let Some(replaced) = self.news_resume.take() else {
            return false;
        };
        for step in replaced.into_iter().rev() {
            self.steps.push_front(step);
        }
        true
    }

    /// Put `steps`, a plan of the rest, before what is left
    /// ([`Self::queue_rest`]). A plan of none answering a re-plan for news
    /// takes up what that re-plan replaced; one that only plans again has
    /// not answered yet, and what was replaced waits for its answer.
    pub(super) fn queue_answer(&mut self, steps: Vec<Step>) {
        let answered_nothing = steps.is_empty();
        let replans = steps
            .iter()
            .all(|step| matches!(step.action, StepAction::Continue(_)));
        if !replans {
            self.news_resume = None;
        }
        self.queue_rest(steps);
        if answered_nothing {
            self.resume_after_news();
        }
    }

    /// Put `steps`, a plan of the rest, before what is left. The export
    /// turn runs exports only: a model asked for the missing export that
    /// rewrote the note as well lost what the first plan had written right.
    /// A check, a reminder of obligations or a repair runs no operation
    /// again as it ran: one that adds (a slide, an event) would add it
    /// twice. What such a step was planned to fulfil, it fulfilled when it
    /// ran.
    pub(super) fn queue_rest(&mut self, steps: Vec<Step>) {
        self.waits.replan = false;
        let turn = self.follow_ups.turn.take();
        for step in steps.into_iter().rev() {
            let dropped = match turn {
                Some(FollowUp::Exporting) => {
                    matches!(step.action, StepAction::Invoke { .. }) && !is_export(&step)
                }
                Some(FollowUp::Checking | FollowUp::Obligations | FollowUp::Repairing) => {
                    self.ran_already(&step)
                }
                _ => false,
            };
            if dropped {
                if let Some(number) = step.serves {
                    self.checklist.tick(number);
                }
            } else {
                self.steps.push_front(step);
            }
        }
    }

    /// Whether `step` is an operation that already ran with the same
    /// arguments, in the same app.
    pub(super) fn ran_already(&self, step: &Step) -> bool {
        let StepAction::Invoke { address, arguments } = &step.action else {
            return false;
        };
        self.invoked.iter().any(|ran| {
            ran.app == step.app
                && matches!(&ran.action, StepAction::Invoke { address: a, arguments: b }
                    if a == address && b == arguments)
        })
    }

    /// A task that starts with `steps`, and what was read while planning
    /// them.
    pub(super) fn new(
        id: String,
        request: String,
        steps: Vec<Step>,
        read: Vec<(String, String)>,
    ) -> Self {
        let mut running = Self {
            id,
            request,
            steps: steps.into(),
            performed: 0,
            last: None,
            produced: Vec::new(),
            produced_by: Vec::new(),
            observed: VecDeque::new(),
            repairs: Repairs::default(),
            news_replans: 0,
            news_resume: None,
            waits: Waits::default(),
            touched: Vec::new(),
            made: Vec::new(),
            acted: 0,
            changed: false,
            follow_ups: FollowUps::default(),
            exports: Vec::new(),
            edited_at: BTreeMap::new(),
            by_model: false,
            ran: Vec::new(),
            invoked: Vec::new(),
            looked: Vec::new(),
            new_documents: Vec::new(),
            checklist: Checklist::default(),
        };
        for (app, seen) in read {
            running.observe(app, seen);
        }
        running
    }

    /// What a plan of the task is shown of its pictures: on a re-plan
    /// (`replan`), the newest its steps looked at; and every picture its
    /// apps rendered or made, which the plan may look at whatever the
    /// agent may do with the room's own.
    pub(super) fn pictures(&self, replan: bool) -> (Vec<String>, Vec<String>) {
        let newest = if replan {
            self.looked.last().cloned().into_iter().collect()
        } else {
            Vec::new()
        };
        let rendered = self.looked.iter().chain(&self.produced).cloned().collect();
        (newest, rendered)
    }

    /// Keep the addresses of `pictures`, rendered for the task, newest last.
    pub(super) fn looked_at(&mut self, pictures: impl IntoIterator<Item = String>) {
        for picture in pictures {
            self.looked.retain(|looked| *looked != picture);
            self.looked.push(picture);
        }
        let over = self.looked.len().saturating_sub(MOST_LOOKED);
        self.looked.drain(..over);
    }

    /// Keep `seen`, read from a record of `app`, for the rest of the task to
    /// be planned with: the newest [`MOST_OBSERVED`].
    pub(super) fn observe(&mut self, app: String, seen: String) {
        if self.observed.len() >= MOST_OBSERVED {
            self.observed.pop_front();
        }
        self.observed.push_back(Observed {
            app,
            seen,
            before_change: false,
            edit: false,
        });
    }

    /// What the task is planned again with: each answer, those read before
    /// a later step changed their app's record said to be so.
    pub(super) fn observations(&self) -> Vec<String> {
        self.observed.iter().map(Observed::shown).collect()
    }

    /// `step`, which changed its record, answered `output` (a deck's outline,
    /// the cells it wrote): that replaces what the app's last edit answered,
    /// so a run of edits keeps the task's reads rather than crowding them out.
    pub(super) fn edit_answered(&mut self, step: &Step, output: ActionOutput) {
        if let StepAction::Invoke { address, .. } = &step.action
            && observation(&step.app, address, &output).is_some()
        {
            self.observed
                .retain(|observed| !(observed.edit && observed.app == step.app));
        }
        let before = self.observed.len();
        self.answered(step, output);
        if self.observed.len() > before
            && let Some(newest) = self.observed.back_mut()
        {
            newest.edit = true;
        }
    }

    /// `step` answered `output`: its value — what a view or read found, an
    /// artwork's size or a document's text — is kept for the rest of the
    /// task to be planned with, and when it produced content, that is what
    /// a step that carries is given next. A read between an export and the
    /// step that places it leaves the export to be placed.
    pub(super) fn answered(&mut self, step: &Step, output: ActionOutput) {
        if let StepAction::Invoke { address, .. } = &step.action
            && let Some(observed) = observation(&step.app, address, &output)
        {
            self.observe(step.app.clone(), observed);
        }
        self.looked_at(rendered_picture(&output));
        if !produced(&output).is_empty() {
            self.last = Some(output);
        }
    }
}

/// Why `address` names nothing, with what the record offers instead: the
/// resource's own actions, else the resources that have that action.
pub(super) fn unknown_action(address: &ActionAddress, offered: &[ActionDescriptor]) -> String {
    const LISTED: usize = 12;
    let mut own: Vec<&str> = offered
        .iter()
        .filter(|descriptor| descriptor.address.resource == address.resource)
        .map(|descriptor| descriptor.address.action.as_str())
        .collect();
    own.dedup();
    let elsewhere: Vec<&str> = offered
        .iter()
        .filter(|descriptor| descriptor.address.action == address.action)
        .map(|descriptor| descriptor.address.resource.as_str())
        .take(LISTED)
        .collect();
    let mut reason = format!(
        "unknown_action: {} has no action `{}`",
        address.resource, address.action
    );
    if !own.is_empty() {
        own.truncate(LISTED);
        reason = format!("{reason}; it offers {}", own.join(", "));
    }
    if !elsewhere.is_empty() {
        reason = format!(
            "{reason}; `{}` is on {}",
            address.action,
            elsewhere.join(", ")
        );
    }
    reason
}

/// What a task whose steps made or changed something is checked with, once,
/// when its planner checks work.
pub(super) const CHECK_WORK: &str = "Check what this task made against the request, item by item: each value, \
     name, date, element, file and export it asks for, using what the records above now hold \
     (read one only if what it holds is not shown). If anything asked for is missing or wrong, \
     plan only the steps that fix it, without redoing what is right. If everything is as asked, \
     plan no steps and say so in `reply`.";

/// What a task is checked with, once, when a follow-up plan found nothing
/// offered fits after its steps changed something: the request is shown
/// beside it.
pub(super) const CHECK_LEFT: &str = "Plan what the request still asks for that the steps so far have not \
     done, using what the records above now hold. If everything it asks for is done, plan no \
     steps and say so in `reply`.";

/// The apps of `granted` that `task` names and of which no record is
/// `held` (open or offered in the room).
pub(super) fn named_without_documents(task: &str, granted: &[&str], held: &[&str]) -> Vec<String> {
    granted
        .iter()
        .filter(|app| crate::model::task_names_app_itself(task, app) && !held.contains(app))
        .map(|app| (*app).to_owned())
        .collect()
}

/// What a task naming an app with no document yet is planned again with,
/// after a first plan found nothing that fits: the apps follow.
pub(super) const NEW_DOCUMENT_NOTE: &str = "(Nothing needs opening first: plan the operations of an app \
     that has no document yet directly, starting with the one that makes the document; the \
     first of them opens a new, empty document and the rest run there. Apps with no document \
     yet:";

/// What a task whose request asks for an export, and whose steps made no
/// content, is planned again with.
pub(super) const EXPORT_MISSING: &str = "The steps so far changed the document but exported nothing, and \
     the request asks for an export. Plan only the export the request asks for (its format, kept \
     as content when it says so), without redoing the changes.";

/// Whether `step` is an export (an operation whose name starts so).
pub(super) fn is_export(step: &Step) -> bool {
    matches!(&step.action, StepAction::Invoke { address, .. } if address.action.starts_with("export"))
}

/// Whether `request` asks for something to be exported.
pub(super) fn asks_export(request: &str) -> bool {
    request
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == "export")
}

/// What follows [`CHECK_WORK`]: the steps the task ran, each of which
/// succeeded (a failed step is repaired or ends the task).
pub(super) const CHECK_RAN: &str = "\nThe steps this task already ran succeeded, but what they wrote may \
     still be wrong: compare it with the request. Fix a wrong value by setting it again. Never \
     plan again a step that adds something (slides, events, contacts, lines, marks, rows): what \
     it added is there, and running it again adds a second copy. An export runs again by itself \
     after a change to what it exported. The steps: ";

/// What a task whose plan was only reads is planned again with.
pub(super) const READS_ANSWERED: &str = "The reads above have answered. Now plan the steps that make or change \
     what the request asks for, using what they found. If the request asks for nothing to be \
     made or changed, plan no steps and say so in `reply`.";

/// What a step or a plan's read answered, from a record of `app`.
#[derive(Debug)]
pub(super) struct Observed {
    pub(super) app: String,
    pub(super) seen: String,
    /// A later step of the task changed a record of `app`, so this shows it
    /// as it was then.
    pub(super) before_change: bool,
    /// What an edit answered, which the app's next edit's answer replaces.
    pub(super) edit: bool,
}

/// How an answer read before a later step changed its record is shown.
pub(super) const BEFORE_CHANGE: &str = "(read BEFORE this task's later steps changed it, so it no longer \
     holds this: what it holds now is under what the documents hold now)";

/// A step as a repair is told it already ran: its action and, briefly, its
/// arguments.
pub(super) fn step_brief(step: &Step) -> String {
    let (_, action) = step.log_names();
    let arguments = match &step.action {
        // Inlined data (a picture as base64) is left out with its size: a
        // brief goes to the model and to the plan log.
        StepAction::Invoke { arguments, .. } if !arguments.is_empty() => {
            crate::planner::compact(&serde_json::Value::Object(arguments.clone())).to_string()
        }
        _ => String::new(),
    };
    let mut brief = format!("{action} {arguments}");
    if brief.chars().count() > 160 {
        brief = brief.chars().take(157).collect::<String>() + "...";
    }
    brief.trim().to_owned()
}

/// Whether `step` imports a file into its record (`import-docx-content`,
/// `import-pptx-content`, …): what the record held is replaced. A video
/// project's `import-content` adds media to it instead, and is not one.
pub(super) fn imports(step: &Step) -> bool {
    matches!(&step.action, StepAction::Invoke { address, .. }
        if address.action.starts_with("import-")
            && address.action.ends_with("-content")
            && address.action != "import-content")
}

/// The content addresses among what `output` names.
/// `output` naming among its resources the content its answer carries as
/// `content` (`{"content": "tilo.content:<hash>", …}`, as Mail, Notes and
/// Calendar export), so a step's produced content, `tilo.content:latest` and
/// a carried step find it as they find an export kept with
/// `Effected::content`.
pub(super) fn content_named_in_value(mut output: ActionOutput) -> ActionOutput {
    let named = output
        .value
        .as_ref()
        .and_then(|value| value.get("content"))
        .and_then(serde_json::Value::as_str)
        .filter(|address| tilo_actions::content_hash(address).is_some())
        .map(str::to_owned);
    if let Some(address) = named
        && !output.resources.contains(&address)
    {
        output.resources.push(address);
    }
    output
}

pub(super) fn produced(output: &ActionOutput) -> Vec<String> {
    output
        .resources
        .iter()
        .filter(|resource| tilo_actions::content_hash(resource).is_some())
        .cloned()
        .collect()
}

impl Observed {
    pub(super) fn shown(&self) -> String {
        if self.before_change {
            format!("{BEFORE_CHANGE} {}", self.seen)
        } else {
            self.seen.clone()
        }
    }
}
