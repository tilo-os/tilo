//! The triggers at a time a resource holds
//! ([`TriggerSpec::ResourceTime`]): the times are read as they stand now,
//! and each moment that has come is fired once
//! ([`tilo_automation::resource_time`]).
//!
//! The times read here are those of a person's calendars on a calendar
//! server of their own, which the holder of their accounts keeps a copy of:
//! every occurrence of every entry, as the Calendar app's own reader expands
//! them. They are read for the person the program is carried out for, and
//! only from the calendar accounts that person named their assistant for,
//! since what an entry holds is handed to that assistant.

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::{Armed, Event, Firer, Firing, TimeEvent};
use tilo_automation::resource_time::{self, Answered, Held};

use super::ProgramRunner;

/// Whether `trigger` waits on a time a resource holds: nothing a member
/// writes in the workspace wakes it.
pub(super) const fn waits_on_a_time(trigger: &TriggerSpec) -> bool {
    matches!(trigger, TriggerSpec::ResourceTime { .. })
}

/// One occurrence of an entry, as a person's calendars answer it, as the
/// time `field` names of it.
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
        // An occurrence keeps its entry and its date in the series when it
        // is moved.
        key: format!("{}/{}", text("entry"), text("occurrence")),
        at_ms,
        resource: text("account"),
        label: text("title"),
        facts,
    })
}

impl ProgramRunner {
    /// The firings of the program's triggers at a time a resource holds
    /// whose moment has come by `now_ms`, each time answered once, and the
    /// next moment to wait for. What was answered is kept with the
    /// program's progress, so a restart answers nothing twice and a moment
    /// that passed while nothing looked is answered as
    /// [`tilo_automation::resource_time`] says. Times that could not be
    /// read are looked at again on the next pass, and nothing is answered
    /// or forgotten for them meanwhile.
    pub(super) async fn time_firings(&mut self, now_ms: i64) -> Vec<Firing> {
        let installation = self.program.id.clone();
        let shortcut = Arc::clone(&self.shortcut);
        let armed = [Armed {
            firer: Firer::Bot {
                installation: &installation,
            },
            shortcut: &shortcut,
        }];
        let mut firings = Vec::new();
        let mut next: Option<i64> = None;
        for (index, trigger) in shortcut.triggers.iter().enumerate() {
            let TriggerSpec::ResourceTime {
                kind,
                field,
                offset_secs,
                filter,
            } = trigger
            else {
                continue;
            };
            let (from_ms, to_ms) = resource_time::window(*offset_secs, now_ms);
            let held = match self.times_held(kind, field, filter, from_ms, to_ms).await {
                Ok(held) => held,
                Err(error) => {
                    tracing::warn!(%installation, %error, "the times a program waits on could not be read; it looks again");
                    continue;
                }
            };
            let mark = tilo_peer_state::trigger_mark(trigger);
            let answered = self
                .progress
                .times
                .entry(index)
                .or_insert_with(|| Answered::starting(&mark, now_ms));
            // Another trigger in this one's place starts over.
            if answered.mark != mark {
                *answered = Answered::starting(&mark, now_ms);
            }
            let due = resource_time::due(*offset_secs, &held, answered, now_ms);
            if let Some(at) = due.next_ms {
                next = Some(next.map_or(at, |kept| kept.min(at)));
            }
            for one in due.fire {
                let event = TimeEvent {
                    workspace: self.workspace.clone(),
                    trigger: index,
                    key: one.key.clone(),
                    at_ms: one.at_ms,
                    payload: resource_time::told(kind, field, *offset_secs, one, now_ms),
                };
                firings.extend(
                    tilo_automation::dispatch::dispatch(&armed, Event::Time(&event)).firings,
                );
            }
        }
        self.progress
            .times
            .retain(|index, _| shortcut.triggers.get(*index).is_some_and(waits_on_a_time));
        self.next_time_ms = next;
        firings
    }

    /// The times `field` names in the resources of `kind`, from `from_ms`
    /// to before `to_ms`, that `filter` takes.
    async fn times_held(
        &self,
        kind: &str,
        field: &str,
        filter: &Map<String, Value>,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<Held>, String> {
        // A person's calendars are the one kind of resource whose times are
        // read here; a program is refused another when it is set up.
        if kind != resource_time::PERSON_CALENDARS {
            return Ok(Vec::new());
        }
        // A program carried out for nobody has nobody's calendars.
        let Some(person) = self.person.as_deref().and_then(|person| {
            hex::decode(person)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        }) else {
            return Ok(Vec::new());
        };
        // What an entry says is handed to its person's assistant: none, nothing read.
        if tilo_workspace_service::agents::assistant::assistant_of(
            &self.workspaces,
            &self.workspace,
            &person,
        )
        .await?
        .is_none()
        {
            return Ok(Vec::new());
        }
        let Some(workspace) = collab_types::WorkspaceId::parse(&self.workspace) else {
            return Ok(Vec::new());
        };
        let occurrences = self
            .workspaces
            .calendar_occurrences_for(workspace, &person, from_ms, to_ms)
            .await?;
        Ok(occurrences
            .into_iter()
            .filter_map(|occurrence| calendar_time(field, occurrence))
            .filter(|held| resource_time::matches(filter, &held.facts))
            .collect())
    }
}
