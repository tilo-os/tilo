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

mod common;

use std::fmt::Write as _;

use common::{Driveable, LOCAL_STATE, catalogue, find_edit};
use tilo_actions::door::ActionDescriptor;
use tilo_agent_participant::{Replica, Seed, StepAction};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

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
        let app = Replica::open(
            &catalogue,
            definition.app_id(),
            ROOM,
            "coverage-agent",
            tilo_agent_participant::ReplicaActor::Agent,
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
                app.0.take_mutation().is_some(),
                "{}: an edit leaves a change to publish",
                definition.app_id()
            );
        }
    }
    eprintln!("{table}");
    assert!(gaps.is_empty(), "agent coverage gaps:\n{}", gaps.join("\n"));
}
