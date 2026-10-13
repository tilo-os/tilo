//! The moderation agent: on a report (or a burst of deletions), it reads the
//! person's recent actions, asks the model, and carries the verdict out
//! through the room's own doors.
//!
//! Every effect goes through what the room already has. A restriction or a
//! ban is a governance change ([`Change::Restrict`], [`Change::Ban`]) the
//! room's policy checks like anyone's. Taking back what the person did is
//! the published `undo` action of each record they touched, with `by` naming
//! them and `to` the version before their first change in the window, run
//! as one history run per decision, so overturning takes the take-back back
//! with `undo { run }`. The decision itself is the moderation record's
//! `decide`, holding the model's reasoning.

use std::collections::BTreeMap;

use tilo_action_policy::Change;

use crate::activity::{Activity, burst};
use crate::judge::{Case, Earlier, Judge, JudgeError, Reported, Verdict};
use crate::record::{Decision, Moderation, Outcome, Reverted, Ruling, moderation};

/// How far back the agent looks at a reported person's actions.
pub const LOOKBACK_MS: i64 = 30 * 60 * 1000;
/// Destructive changes within [`BURST_WINDOW_MS`] that make the agent look
/// without a report.
pub const BURST_COUNT: usize = 8;
pub const BURST_WINDOW_MS: i64 = 60 * 1000;

/// The room as the moderation agent reaches it. The host implements it over
/// the room it governs; every call acts as [`crate::AGENT`].
pub trait RoomDoor {
    /// Unix milliseconds now.
    fn now_ms(&self) -> i64;

    /// `person`'s changes across the room's records since `since_ms`,
    /// oldest first ([`crate::activity_of`] over each record).
    fn activity(&mut self, person: &str, since_ms: i64) -> Vec<Activity>;

    /// A governance change on the room.
    ///
    /// # Errors
    /// The room's policy refused it.
    fn govern(&mut self, change: Change) -> Result<(), String>;

    /// Runs `action` of a record with `args`, in history run `run`.
    ///
    /// # Errors
    /// The action was refused.
    fn invoke(
        &mut self,
        app: &str,
        record: &str,
        action: &str,
        args: serde_json::Value,
        run: &str,
    ) -> Result<(), String>;

    /// Writes a decision into the room's moderation record.
    ///
    /// # Errors
    /// The record refused it.
    fn decide(&mut self, decision: moderation::Decide) -> Result<(), String>;
}

/// What one look at a person did.
#[derive(Clone, Debug)]
pub struct Enforced {
    /// The decision as written to the record.
    pub decision: moderation::Decide,
    /// Takings-back that failed, with why; a decision is still recorded.
    pub failures: Vec<String>,
}

/// What the agent found about one person, ready to put to the model.
#[derive(Clone, Debug)]
pub struct Look {
    /// When it looked, Unix ms.
    pub now: i64,
    /// The open reports the decision will settle.
    pub reports: Vec<String>,
    pub case: Case,
}

/// The agent, with the model it asks.
#[derive(Debug)]
pub struct ModerationAgent<J> {
    judge: J,
}

impl<J: Judge> ModerationAgent<J> {
    pub const fn new(judge: J) -> Self {
        Self { judge }
    }

    /// The model the agent asks.
    pub fn model(&self) -> &str {
        self.judge.model()
    }

    /// The people with reports nobody decided yet.
    #[must_use]
    pub fn reported(moderation: &Moderation) -> Vec<String> {
        let mut people: Vec<String> = moderation
            .open_reports()
            .map(|report| report.person.clone())
            .collect();
        people.sort();
        people.dedup();
        people
    }

    /// Whether `person`'s recent actions are a burst of deletions worth a
    /// look without a report.
    pub fn bursting(door: &mut impl RoomDoor, person: &str) -> bool {
        let since = door.now_ms().saturating_sub(BURST_WINDOW_MS);
        burst(&door.activity(person, since), BURST_COUNT, BURST_WINDOW_MS)
    }

    /// Looks into `person`: their open reports, their recent actions and
    /// earlier decisions. The host asks [`Self::verdict`] on it away from the
    /// room, then [`Self::enforce`]s the answer, so the room is never held
    /// while the model thinks.
    pub fn look(door: &mut impl RoomDoor, moderation: &Moderation, person: &str) -> Look {
        let now = door.now_ms();
        let reports: Vec<_> = moderation.open_reports_on(person).collect();
        let activity = door.activity(person, now.saturating_sub(LOOKBACK_MS));
        let case = Case {
            person: person.to_owned(),
            reports: reports
                .iter()
                .map(|report| Reported {
                    reporter: report.reporter.clone(),
                    reason: report.reason.clone(),
                    item: match (&report.app, &report.record, &report.item) {
                        (Some(app), Some(record), item) => {
                            Some(format!("{app}/{record}/{}", item.as_deref().unwrap_or("")))
                        }
                        _ => report.item.clone(),
                    },
                })
                .collect(),
            activity,
            earlier: moderation
                .decisions
                .iter()
                .filter(|decision| decision.person == person)
                .map(|decision| Earlier {
                    outcome: decision.outcome,
                    reasoning: decision.reasoning.clone(),
                    overturned: !decision.stands(),
                })
                .collect(),
        };
        Look {
            now,
            reports: reports.iter().map(|report| report.id.clone()).collect(),
            case,
        }
    }

    /// The model's verdict on `look`, and the model that gave it.
    ///
    /// # Errors
    /// The model gave no verdict: nothing should be enforced or recorded,
    /// and the reports stay open for a human Moderator.
    pub async fn verdict(&self, look: &Look) -> Result<(Verdict, String), JudgeError>
    where
        J: Sync,
    {
        if look.case.activity.is_empty() {
            // Nothing they did in the window: no model is needed, and a
            // report alone never restricts anyone.
            return Ok((
                Verdict {
                    outcome: Outcome::NoAction,
                    minutes: None,
                    revert: false,
                    reasoning: "They made no changes in the room in the last 30 minutes, so \
                                there is nothing to act on."
                        .to_owned(),
                },
                String::new(),
            ));
        }
        Ok((
            self.judge.judge(&look.case).await?,
            self.judge.model().to_owned(),
        ))
    }

    /// Carries `verdict` out on the room and records it with its reasoning.
    pub fn enforce(
        door: &mut impl RoomDoor,
        look: Look,
        verdict: Verdict,
        model: String,
    ) -> Enforced {
        let Look { now, reports, case } = look;
        let person = case.person.as_str();
        let id = format!("{person}@{now}");
        let run = format!("moderation:{id}");
        let mut failures = Vec::new();
        let mut outcome = verdict.outcome;
        let mut reasoning = verdict.reasoning;
        let mut until_ms = None;
        let governed = match outcome {
            Outcome::NoAction => Ok(()),
            Outcome::Restrict => {
                let minutes = i64::from(
                    verdict
                        .minutes
                        .unwrap_or(crate::judge::MIN_RESTRICT_MINUTES),
                );
                let until = now.saturating_add(minutes.saturating_mul(60_000));
                until_ms = Some(until);
                door.govern(Change::Restrict {
                    person: person.to_owned(),
                    until_ms: u64::try_from(until).unwrap_or_default(),
                })
            }
            Outcome::Ban => door.govern(Change::Ban {
                person: person.to_owned(),
            }),
        };
        if let Err(why) = governed {
            // Out of the agent's reach (a Moderator, the Owner): record what
            // the model thought, enforce nothing, and leave it to people.
            reasoning = format!("{reasoning} Not enforced: {why}.");
            outcome = Outcome::NoAction;
            until_ms = None;
        }
        let reverted = if verdict.revert && outcome != Outcome::NoAction {
            revert(door, &case.activity, &run, &mut failures)
        } else {
            Vec::new()
        };
        let decision = moderation::Decide {
            id,
            person: person.to_owned(),
            reports,
            ruling: Ruling {
                outcome,
                until_ms,
                reasoning,
                model,
                reverted,
            },
        };
        if let Err(why) = door.decide(decision.clone()) {
            failures.push(format!("recording the decision: {why}"));
        }
        Enforced { decision, failures }
    }

    /// Carries out a Moderator's overturning of `decision`: lifts its
    /// restriction or ban and takes back what it took back.
    ///
    /// # Errors
    /// Each part that failed, with why; the others are still done.
    pub fn restore(door: &mut impl RoomDoor, decision: &Decision) -> Result<(), Vec<String>> {
        let mut failures = Vec::new();
        let person = decision.person.clone();
        let lifted = match decision.outcome {
            Outcome::NoAction => Ok(()),
            Outcome::Restrict => {
                let lapsed = decision
                    .until_ms
                    .is_some_and(|until| until <= door.now_ms());
                if lapsed {
                    Ok(())
                } else {
                    door.govern(Change::Unrestrict { person })
                }
            }
            Outcome::Ban => door.govern(Change::Unban { person }),
        };
        if let Err(why) = lifted {
            failures.push(format!("lifting: {why}"));
        }
        for reverted in &decision.reverted.0 {
            if let Err(why) = door.invoke(
                &reverted.app,
                &reverted.record,
                "undo",
                serde_json::json!({ "run": reverted.run }),
                &format!("{}:restore", reverted.run),
            ) {
                failures.push(format!("{}/{}: {why}", reverted.app, reverted.record));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }
}

/// Takes back `activity` record by record: per record and per invoker
/// history names, `undo { by, to }` back to the version before their first
/// change there, or `undo { by, steps }` when that change began the record.
fn revert(
    door: &mut impl RoomDoor,
    activity: &[Activity],
    run: &str,
    failures: &mut Vec<String>,
) -> Vec<Reverted> {
    let mut by_record: BTreeMap<(&str, &str, &str), Vec<&Activity>> = BTreeMap::new();
    for item in activity {
        by_record
            .entry((&item.app, &item.record, &item.actor))
            .or_default()
            .push(item);
    }
    let mut reverted: BTreeMap<(String, String), u32> = BTreeMap::new();
    for ((app, record, actor), changes) in by_record {
        // Only content is taken back: joining, presence and read marks stay,
        // so the person keeps their place in the room's roster. An undo goes
        // back to a version, so it stops at their newest such change and
        // leaves content older than it alone.
        let kept = changes.iter().rposition(|change| !change.content);
        let (start, to) = match kept {
            Some(at) => (
                at.saturating_add(1),
                changes.get(at).map(|kept| kept.change.as_str()),
            ),
            None => (0, changes.first().and_then(|first| first.before.as_deref())),
        };
        let changes = changes.get(start..).unwrap_or_default();
        if changes.is_empty() {
            continue;
        }
        let args = match to {
            Some(to) => serde_json::json!({ "by": actor, "to": to }),
            None => serde_json::json!({ "by": actor, "steps": changes.len() }),
        };
        match door.invoke(app, record, "undo", args, run) {
            Ok(()) => {
                let count = reverted
                    .entry((app.to_owned(), record.to_owned()))
                    .or_default();
                *count = count.saturating_add(u32::try_from(changes.len()).unwrap_or(u32::MAX));
            }
            Err(why) => failures.push(format!("{app}/{record}: {why}")),
        }
    }
    reverted
        .into_iter()
        .map(|((app, record), changes)| Reverted {
            app,
            record,
            run: run.to_owned(),
            changes,
        })
        .collect()
}

#[cfg(test)]
mod tests;
