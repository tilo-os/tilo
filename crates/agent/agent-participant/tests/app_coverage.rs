//! Every app an agent can open, held as a replica of its resource and
//! edited the way an agent edits it: through an operation the record
//! publishes, found without app-specific knowledge. An app whose record no
//! published operation changes is a coverage gap and fails here by name.
//!
//! `session_coverage` proves the same edits over the real session: the
//! browser receives them, authored by the agent.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fmt::Write as _;

use crate::common::{Driveable, LOCAL_STATE, catalogue, find_edit};
use tilo_actions::door::ActionDescriptor;
use tilo_agent_participant::{Replica, ReplicaActor, Seed, StepAction};

/// How many attempts an app gets before it is a gap.
const ATTEMPTS: usize = 40;

struct Held(Replica);

impl Driveable for Held {
    fn survey(&mut self) -> Vec<ActionDescriptor> {
        self.0.survey()
    }
    fn body(&mut self) -> String {
        self.0
            .read(|doc| serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap())
            .unwrap()
    }
    fn run(&mut self, action: &StepAction) -> Result<(), String> {
        match action {
            StepAction::Invoke { address, arguments } => self
                .0
                .invoke(address, arguments, false)
                .map(|_| ())
                .map_err(|error| format!("{}: {error}", error.code())),
            StepAction::Open | StepAction::Continue(_) | StepAction::Wait(_) => Ok(()),
        }
    }
}

#[test]
fn every_web_app_takes_an_edit_through_its_published_operations() {
    let catalogue = catalogue();
    let mut table = String::from("| App | Operations | Edit |\n|---|---|---|\n");
    let mut gaps = Vec::new();
    for definition in catalogue.iter() {
        let app = crate::common::open(
            definition.app_id(),
            "coverage-agent",
            ReplicaActor::Agent,
            Seed::New,
        )
        .unwrap_or_else(|error| panic!("{} did not open: {error}", definition.app_id()));
        let mut app = Held(app);
        let _ = app.0.take_mutation();
        let operations = app.survey().len();
        let (edit, refused) = find_edit(&mut app, ATTEMPTS);
        let label = edit
            .as_ref()
            .map_or_else(|| "—".to_owned(), |edit| edit.label.clone());
        let _ = writeln!(
            table,
            "| {} | {operations} | `{label}` |",
            definition.app_id()
        );
        let local = LOCAL_STATE
            .iter()
            .find(|(app, _)| *app == definition.app_id());
        if let (None, Some((_, why))) = (&edit, local) {
            let _ = writeln!(table, "|   ↳ local state | | {why} |");
        } else if edit.is_none() {
            gaps.push(format!("{}: {refused:#?}", definition.app_id()));
        } else {
            assert!(
                app.0.take_mutation().unwrap().is_some(),
                "{}: an edit leaves a change to publish",
                definition.app_id()
            );
        }
    }
    eprintln!("{table}");
    assert!(gaps.is_empty(), "agent coverage gaps:\n{}", gaps.join("\n"));
}

/// Every app's record reads, with no window open: the read a caller holding
/// the record's path invokes (`tilo_read <path>`, the SDK's
/// `Document.load()`) answers what the record holds, so an agent or a script
/// reaches every app's content and never a record that has no read at all.
#[test]
fn every_web_apps_record_reads_with_no_window_open() {
    let catalogue = catalogue();
    let mut gaps = Vec::new();
    let apps = catalogue.iter().count();
    let mut readable = 0;
    for definition in catalogue.iter() {
        let app = definition.app_id();
        // Opened the way a host holds a record for the door: no window, so
        // nothing but the record's own declaration can answer.
        let replica =
            crate::common::confined(app, "reading-agent", &ReplicaActor::Agent, Seed::New)
                .unwrap_or_else(|error| panic!("{app} did not open: {error}"));
        let root = definition.record_kind.clone();
        let Some(descriptor) = replica.survey().into_iter().find(|descriptor| {
            descriptor.address.action == "read" && descriptor.address.resource == root.as_str()
        }) else {
            gaps.push(format!("{app}: its record publishes no read"));
            continue;
        };
        match replica.invoke(&descriptor.address, &serde_json::Map::new(), false) {
            // A record's fields, as JSON: what it holds, however little.
            Ok(tilo_actions::ActionOutput {
                value: Some(serde_json::Value::Object(_)),
                ..
            }) => readable += 1,
            Ok(output) => gaps.push(format!("{app}: the read answers {:?}", output.value)),
            Err(error) => gaps.push(format!("{app}: {} {error}", descriptor.address.action)),
        }
    }
    assert!(
        gaps.is_empty(),
        "records a caller with no window open cannot read:\n{}",
        gaps.join("\n")
    );
    assert_eq!(
        readable, apps,
        "every app of the catalogue reads its record"
    );
}
