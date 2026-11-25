//! The triggers at a time a resource holds ([`TriggerSpec::ResourceTime`]): the times are read as
//! they stand now, and each moment that has come fires once
//! ([`tilo_automation::resource_time`]).
//!
//! The times read here are the Calendar app's: its entries, as the workspace's store holds the
//! records the calendar bridge keeps for it, every occurrence of an entry that repeats among them,
//! read as the app's own views read them ([`tilolang_calendar::records`]). Nothing of a calendar is
//! kept by the host that runs the shortcut, and nothing is asked of a calendar server: the app
//! publishes its records and the trigger reads them as it reads any app's.
//!
//! What each trigger answered is kept in the workspace ([`super::keep_progress`]), so a moment
//! answered is not answered again after a restart, and the device that looks next carries on
//! from it. Where two devices read the same moment at once, the turn's lease keeps it answered
//! once: its invocation id is derived from the event ([`super::dispatched`]), so both name one
//! firing.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::{Event, TimeEvent};
use tilo_automation::resource_time::{self, Answered, Held};
use tilolang_calendar::records;

use super::{Armed, Runner, dispatched, fields_of, keep_progress, kept_progress};

/// The longest the times wait before they are read again, so a time another client moved or
/// added is found within it: as often as the runner looks by itself.
const LOOK_AGAIN: Duration = Duration::from_secs(30);

/// The most records one look reads: every entry of every calendar of a workspace, well past what
/// one person keeps.
const MOST_RECORDS: usize = 10_000;

/// What one trigger at a time a resource holds has answered, and when its times are read next.
pub(super) struct Progress {
    answered: Answered,
    /// When the times are read next, ms since the epoch: the next moment still to come, and no
    /// later than [`LOOK_AGAIN`] after the last read.
    read_at_ms: i64,
}

impl Progress {
    /// The progress of the trigger marked `mark`, first seen at `now_ms`.
    fn starting(mark: &str, now_ms: i64) -> Self {
        Self {
            answered: Answered::starting(mark, now_ms),
            read_at_ms: now_ms,
        }
    }
}

/// The times `field` names in the records of `kind` that `filter` takes, from `from_ms` to before
/// `to_ms`.
async fn times_read(
    runner: &Runner,
    armed: &Armed,
    kind: &str,
    field: &str,
    filter: &Map<String, Value>,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<Held>, String> {
    let found = runner
        .workspaces
        .store_found(
            &armed.workspace,
            tilo_resource_store::Query::of(kind, MOST_RECORDS),
        )
        .await?;
    let entries: Vec<records::Entry> = found
        .into_iter()
        .map(|(id, held)| records::Entry {
            id,
            fields: fields_of(&held),
        })
        .collect();
    Ok(records::times(&entries, field, from_ms, to_ms)
        .into_iter()
        .filter(|one| resource_time::matches(filter, &one.facts))
        .map(|one| {
            let text = |name: &str| {
                one.facts
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            Held {
                key: one.key,
                at_ms: one.at_ms,
                resource: text("calendar"),
                label: text("title"),
                facts: one.facts,
            }
        })
        .collect())
}

/// The time `LOOK_AGAIN` is, in milliseconds.
fn look_again_ms() -> i64 {
    i64::try_from(LOOK_AGAIN.as_millis()).unwrap_or(i64::MAX)
}

impl Runner {
    /// Fire the triggers at a time a resource holds whose moment has come, each moment answered
    /// once ([`resource_time::due`]). The times are read at the next moment still to come, and
    /// otherwise every [`LOOK_AGAIN`], so a time that moved is found.
    pub(super) async fn moments(&mut self, now_ms: i64) {
        let mut firings = Vec::new();
        for armed in &self.armed {
            for (index, trigger) in armed.shortcut.triggers.iter().enumerate() {
                let TriggerSpec::ResourceTime {
                    kind,
                    field,
                    offset_secs,
                    filter,
                } = trigger
                else {
                    continue;
                };
                // The Calendar app's entries are the one kind of resource whose times are read
                // here; a kind this host knows nothing of fires nothing.
                if kind != resource_time::CALENDAR_EVENTS {
                    continue;
                }
                let key = (armed.workspace.clone(), armed.shortcut.id.clone(), index);
                let mark = tilo_peer_state::trigger_mark(trigger);
                // A trigger first seen here, or another trigger in this one's place, carries on
                // from what the workspace kept of it, and starts over where nothing was.
                if self
                    .times
                    .get(&key)
                    .is_none_or(|progress| progress.answered.mark != mark)
                {
                    let kept = kept_progress::<Answered>(&self.workspaces, armed, index, &mark)
                        .await
                        .filter(|answered| answered.mark == mark);
                    self.times.insert(
                        key.clone(),
                        kept.map_or_else(
                            || Progress::starting(&mark, now_ms),
                            |answered| Progress {
                                answered,
                                read_at_ms: now_ms,
                            },
                        ),
                    );
                }
                if self
                    .times
                    .get(&key)
                    .is_some_and(|progress| now_ms < progress.read_at_ms)
                {
                    continue;
                }
                let (from_ms, to_ms) = resource_time::window(*offset_secs, now_ms);
                let times = times_read(self, armed, kind, field, filter, from_ms, to_ms).await;
                let Some(progress) = self.times.get_mut(&key) else {
                    continue;
                };
                let before = progress.answered.clone();
                match times {
                    Ok(times) => {
                        let due = resource_time::due(
                            *offset_secs,
                            &times,
                            &mut progress.answered,
                            now_ms,
                        );
                        progress.read_at_ms = due.next_ms.map_or_else(
                            || now_ms.saturating_add(look_again_ms()),
                            |at| at.min(now_ms.saturating_add(look_again_ms())),
                        );
                        for one in due.fire {
                            let event = TimeEvent {
                                workspace: armed.workspace.clone(),
                                trigger: index,
                                key: one.key.clone(),
                                at_ms: one.at_ms,
                                payload: resource_time::told(
                                    kind,
                                    field,
                                    *offset_secs,
                                    one,
                                    now_ms,
                                ),
                            };
                            firings.extend(dispatched(
                                std::slice::from_ref(armed),
                                Event::Time(&event),
                                now_ms,
                            ));
                        }
                    }
                    // Times that could not be read are looked at again, and nothing is answered
                    // or forgotten for them meanwhile.
                    Err(error) => {
                        tracing::warn!(%error, shortcut = %armed.shortcut.id, "the times a shortcut waits on could not be read; it looks again");
                        progress.read_at_ms = now_ms.saturating_add(look_again_ms());
                    }
                }
                // What was answered is kept in the workspace, so a moment answered here is not
                // answered again after a restart or on the device that looks next.
                if progress.answered != before {
                    let answered = progress.answered.clone();
                    keep_progress(&self.workspaces, armed, index, &mark, &answered).await;
                }
            }
        }
        let waited: BTreeSet<(String, String, usize)> = self
            .armed
            .iter()
            .flat_map(|armed| {
                armed
                    .shortcut
                    .triggers
                    .iter()
                    .enumerate()
                    .filter(|(_, trigger)| matches!(trigger, TriggerSpec::ResourceTime { .. }))
                    .map(|(index, _)| (armed.workspace.clone(), armed.shortcut.id.clone(), index))
            })
            .collect();
        self.times.retain(|key, _| waited.contains(key));
        self.offer(firings);
    }
}
