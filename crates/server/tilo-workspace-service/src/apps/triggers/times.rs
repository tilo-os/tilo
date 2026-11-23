//! The triggers at a time a resource holds ([`TriggerSpec::ResourceTime`]): the times are read as
//! they stand now, and each moment that has come fires once
//! ([`tilo_automation::resource_time`]).
//!
//! The times read here are those of a person's calendars, kept in the workspace as the pull of
//! their servers left them: every occurrence of every entry, as the Calendar app's own reader
//! expands them. They are read for the shortcut's saver, and only where that person keeps an
//! assistant in the workspace, since what an entry holds is handed to that assistant.
//!
//! What each trigger answered is kept in this device's memory, by workspace, shortcut and
//! trigger. A moment this device answered is not answered again while it runs, and the turn's
//! lease keeps it answered workspace-wide even where another device reads the same moment: its
//! invocation id is derived from the event ([`super::dispatched`]), so both devices name one
//! firing.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::{Event, TimeEvent};
use tilo_automation::resource_time::{self, Answered, Held};

use super::{Armed, Runner, dispatched, person_key};

/// The longest the times wait before they are read again, so a time another client moved or
/// added is found within it: as often as the runner looks by itself.
const LOOK_AGAIN: Duration = Duration::from_secs(30);

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

/// One occurrence of an entry, as a person's calendars answer it, as the time `field` names of
/// it.
fn calendar_time(field: &str, occurrence: Value) -> Option<Held> {
    let Value::Object(facts) = occurrence else {
        return None;
    };
    let named = if field == resource_time::CALENDAR_END {
        "endMs"
    } else {
        "startMs"
    };
    let at_ms = facts.get(named)?.as_i64()?;
    let text = |name: &str| {
        facts
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Some(Held {
        // An occurrence keeps its entry and its date in the series when it is moved.
        key: format!("{}/{}", text("entry"), text("occurrence")),
        at_ms,
        resource: text("account"),
        label: text("title"),
        facts,
    })
}

/// The times `field` names in the calendars of `armed`'s saver, from `from_ms` to before
/// `to_ms`, that `filter` takes.
async fn times_read(
    runner: &Runner,
    armed: &Armed,
    field: &str,
    filter: &Map<String, Value>,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<Held>, String> {
    // A shortcut carried out for nobody has nobody's calendars.
    let Some(person) = person_key(&armed.saver) else {
        return Ok(Vec::new());
    };
    // What an entry says is handed to its person's assistant: where they keep none, nothing is
    // read.
    if crate::agents::assistant::assistant_of(&runner.workspaces, &armed.workspace, &person)
        .await?
        .is_none()
    {
        return Ok(Vec::new());
    }
    let Some(workspace) = collab_types::WorkspaceId::parse(&armed.workspace) else {
        return Ok(Vec::new());
    };
    let occurrences = runner
        .workspaces
        .calendar_occurrences_for(workspace, &person, from_ms, to_ms)
        .await?;
    Ok(occurrences
        .into_iter()
        .filter_map(|occurrence| calendar_time(field, occurrence))
        .filter(|one| resource_time::matches(filter, &one.facts))
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
                // A person's calendars are the one kind of resource whose times are read here.
                if kind != resource_time::PERSON_CALENDARS {
                    continue;
                }
                let key = (armed.workspace.clone(), armed.shortcut.id.clone(), index);
                let mark = tilo_peer_state::trigger_mark(trigger);
                let read = {
                    let progress = self
                        .times
                        .entry(key.clone())
                        .or_insert_with(|| Progress::starting(&mark, now_ms));
                    // Another trigger in this one's place starts over.
                    if progress.answered.mark != mark {
                        *progress = Progress::starting(&mark, now_ms);
                    }
                    now_ms >= progress.read_at_ms
                };
                if !read {
                    continue;
                }
                let (from_ms, to_ms) = resource_time::window(*offset_secs, now_ms);
                let times = times_read(self, armed, field, filter, from_ms, to_ms).await;
                let Some(progress) = self.times.get_mut(&key) else {
                    continue;
                };
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
