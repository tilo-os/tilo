//! The room's moderation agent, run on the host as an installed bot over a
//! confined replica of the record that holds the room's reports.
//!
//! Nothing here reads or writes around the record: the agent's door
//! ([`ReplicaDoor`]) reads the person's recent changes from the replica's own
//! history, takes them back with the record's published `undo` action and
//! records its decision with the record's `decide-reports`, each invoked
//! through the replica's door as the bot, so the room's policy and history
//! see the bot's steps like anyone's. Restricting and banning are governance,
//! which a bot never holds: the host enforces those as the room's root
//! through `govern`, held to the room's policy.

use tilo_action_policy::Change;
use tilo_actions::entry::Did;
use tilo_agent_participant::Replica;
use tilo_moderation::{Activity, Moderation, RoomDoor, Source, activity_of, destructive_verbs};
use tilo_resource::Resource;

/// The history group kind a run's steps are recorded under, which the
/// record's `undo { run }` takes back as one.
const AGENT_RUN: &str = "agent-run";

/// The record verbs that file a report, on which the agent looks.
pub const REPORT_VERBS: [&str; 2] = ["report-message", "report-person"];

/// Whether a committed change filed a report.
#[must_use]
pub fn files_a_report(did: &Did) -> bool {
    match did {
        Did::Verb(call) => REPORT_VERBS.contains(&call.action.action.as_str()),
        Did::Group(group) => group
            .members
            .iter()
            .chain(&group.distinct)
            .any(files_a_report),
        _ => false,
    }
}

/// The room as the moderation agent reaches it: one record's replica, held
/// as the installed bot, and the host's governance.
pub struct ReplicaDoor<'a, R: Resource> {
    replica: &'a mut Replica,
    moderation: fn(&R::State) -> &Moderation,
    govern: &'a mut dyn FnMut(Change) -> Result<(), String>,
    now_ms: i64,
}

impl<'a, R: Resource> ReplicaDoor<'a, R> {
    /// The door over `replica`, whose state holds the room's moderation
    /// record at `moderation`, enforcing governance through `govern`.
    pub fn new(
        replica: &'a mut Replica,
        moderation: fn(&R::State) -> &Moderation,
        govern: &'a mut dyn FnMut(Change) -> Result<(), String>,
        now_ms: i64,
    ) -> Self {
        Self {
            replica,
            moderation,
            govern,
            now_ms,
        }
    }

    /// The room's moderation record as the replica holds it now.
    #[must_use]
    pub fn moderation(&self) -> Option<Moderation> {
        let pick = self.moderation;
        self.replica
            .read(|doc| R::read(doc).ok().map(|state| pick(&state).clone()))
            .flatten()
    }

    fn record(&self) -> String {
        self.replica.record_id().to_string()
    }

    fn run_action(
        &mut self,
        action: &str,
        args: serde_json::Value,
        context: Option<tilo_actions::ActionContext>,
    ) -> Result<(), String> {
        let address = self
            .replica
            .survey()
            .into_iter()
            .find(|offered| offered.address.action == action)
            .map(|offered| offered.address)
            .ok_or_else(|| format!("the record offers the moderation agent no {action}"))?;
        let serde_json::Value::Object(arguments) = args else {
            return Err(format!("{action} takes an object"));
        };
        self.replica
            .invoke_with(None, context, &address, &arguments, true)
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

impl<R: Resource> RoomDoor for ReplicaDoor<'_, R> {
    fn now_ms(&self) -> i64 {
        self.now_ms
    }

    fn activity(&mut self, person: &str, since_ms: i64) -> Vec<Activity> {
        let app = self.replica.app_id();
        let record = self.record();
        self.replica
            .read(|doc| {
                let versions = tilo_resource::history::committed_versions(doc);
                let destructive = R::read(doc)
                    .map(|state| {
                        destructive_verbs(&tilo_resource::resource::publish::<R>(
                            R::KIND,
                            R::LABEL,
                            &state,
                        ))
                    })
                    .unwrap_or_default();
                activity_of(
                    &Source {
                        app,
                        record: &record,
                        versions: &versions,
                        destructive: &destructive,
                    },
                    person,
                    since_ms,
                )
            })
            .unwrap_or_default()
    }

    fn govern(&mut self, change: Change) -> Result<(), String> {
        (self.govern)(change)
    }

    fn invoke(
        &mut self,
        app: &str,
        record: &str,
        action: &str,
        args: serde_json::Value,
        run: &str,
    ) -> Result<(), String> {
        if app != self.replica.app_id() || record != self.record() {
            return Err(format!("{app}/{record} is not held here"));
        }
        let context =
            tilo_actions::ActionContext::default().with_group(run, "Moderation", AGENT_RUN);
        self.run_action(action, args, Some(context))
    }

    fn decide(
        &mut self,
        decision: tilo_moderation::record::moderation::Decide,
    ) -> Result<(), String> {
        let args = serde_json::to_value(decision).map_err(|error| error.to_string())?;
        self.run_action("decide-reports", args, None)
    }
}

#[cfg(test)]
mod tests;
