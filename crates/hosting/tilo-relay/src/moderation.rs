//! The room's moderation agent, run on the host as an installed bot over a
//! confined replica of the record that holds the room's reports.
//!
//! Nothing here reads or writes around the record: the agent's door
//! ([`ReplicaDoor`]) reads the person's recent changes from the replica's own
//! history and the histories of the parts its app keeps beside it (a chat's
//! messages), takes them back with the published `undo` action — the
//! record's own, or for a part the chain's, naming the part — and records
//! its decision with the record's `decide-reports`, each invoked
//! through the replica's door as the bot, so the room's policy and history
//! see the bot's steps like anyone's. Restricting and banning are governance,
//! which a bot never holds: the host enforces those as the room's root
//! through `govern`, held to the room's policy.

use tilo_action_policy::Change;
use tilo_actions::door::ActionAddress;
use tilo_actions::entry::Did;
use tilo_agent_participant::Replica;
use tilo_moderation::{Activity, Moderation, RoomDoor, Source, activity_of, destructive_verbs};
use tilo_resource::Resource;
use ui::record::RecordId;

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
/// as the installed bot, with the records its app keeps beside it (the
/// parts of a chat's messages), and the host's governance.
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

    /// The address of `action` the replica offers: on the record itself,
    /// or with `chain`, on the chain of parts the record's app publishes
    /// beside it.
    fn address(&self, action: &str, chain: Option<&RecordId>) -> Result<ActionAddress, String> {
        self.replica
            .survey()
            .into_iter()
            .find(|offered| {
                offered.address.action == action
                    && ui::record::chain_of_resource_id(&offered.address.resource).as_ref() == chain
            })
            .map(|offered| offered.address)
            .ok_or_else(|| format!("the record offers the moderation agent no {action}"))
    }

    fn run_action(
        &mut self,
        address: &ActionAddress,
        args: serde_json::Value,
        context: Option<tilo_actions::ActionContext>,
    ) -> Result<(), String> {
        let serde_json::Value::Object(arguments) = args else {
            return Err(format!("{} takes an object", address.action));
        };
        self.replica
            .invoke_with(None, context, address, &arguments, true)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// The verbs the chain `chain` publishes as destructive, on itself and
    /// on its items.
    fn destructive_in_chain(&self, chain: &RecordId) -> Vec<String> {
        let id = ui::record::chain_resource_id(chain);
        let resources = self.replica.resources();
        // Published beside the app's resource, among its children; its
        // items carry the verbs on one message.
        let Some(published) = resources
            .iter()
            .flat_map(|resource| std::iter::once(resource).chain(&resource.children))
            .find(|resource| resource.id.as_str() == id)
        else {
            return Vec::new();
        };
        let mut verbs = destructive_verbs(published);
        for item in &published.children {
            for verb in destructive_verbs(item) {
                if !verbs.contains(&verb) {
                    verbs.push(verb);
                }
            }
        }
        verbs
    }
}

impl<R: Resource> RoomDoor for ReplicaDoor<'_, R> {
    fn now_ms(&self) -> i64 {
        self.now_ms
    }

    fn activity(&mut self, person: &str, since_ms: i64) -> Vec<Activity> {
        let app = self.replica.app_id();
        let record = self.record();
        let mut activity = self
            .replica
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
            .unwrap_or_default();
        // What they wrote is kept in the parts of the chain the record's
        // app keeps beside it (a chat's messages): each part's history.
        for part in self.replica.records_beside() {
            let Some((chain, _)) = part.chain_part() else {
                continue;
            };
            let destructive = self.destructive_in_chain(&chain);
            let record = part.as_str().to_owned();
            activity.extend(
                self.replica
                    .read_record(&part, |doc| {
                        let versions = tilo_resource::history::committed_versions(doc);
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
                    .unwrap_or_default(),
            );
        }
        activity.sort_by_key(|item| item.time_ms);
        activity
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
        if app != self.replica.app_id() {
            return Err(format!("{app}/{record} is not held here"));
        }
        let context =
            tilo_actions::ActionContext::default().with_group(run, "Moderation", AGENT_RUN);
        if record == self.record() {
            let address = self.address(action, None)?;
            return self.run_action(&address, args, Some(context));
        }
        // A part held beside the record is changed through the chain its
        // app publishes, naming the part.
        let part = RecordId::parse(record)
            .filter(|part| self.replica.keeps_beside(part))
            .ok_or_else(|| format!("{app}/{record} is not held here"))?;
        let (chain, index) = part
            .chain_part()
            .ok_or_else(|| format!("{app}/{record} is not held here"))?;
        let address = self.address(action, Some(&chain))?;
        let mut args = args;
        if let serde_json::Value::Object(arguments) = &mut args {
            arguments.insert("part".into(), serde_json::Value::from(index));
        }
        self.run_action(&address, args, Some(context))
    }

    fn decide(
        &mut self,
        decision: tilo_moderation::record::moderation::Decide,
    ) -> Result<(), String> {
        let args = serde_json::to_value(decision).map_err(|error| error.to_string())?;
        let address = self.address("decide-reports", None)?;
        self.run_action(&address, args, None)
    }
}

#[cfg(test)]
mod tests;
