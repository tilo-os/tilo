//! Every app an agent can open, mounted headless and edited the way an
//! agent edits it: through a published action or an on-screen control,
//! found without app-specific knowledge. An app that publishes nothing, or
//! whose record no user-equivalent input changes, is a coverage gap and
//! fails here by name.
//!
//! `shared_session_covers_every_app` proves the same edits over the real
//! session: the browser receives them and sees the agent's cursor.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

use std::fmt::Write as _;

use common::{Driveable, LOCAL_STATE, catalogue, find_edit};
use tilo_actions::door::ActionDescriptor;
use tilo_agent_participant::apps::is_semantic;
use tilo_agent_participant::{Control, HostedApp, Seed, StepAction};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

/// How many attempts an app gets before it is a gap.
const ATTEMPTS: usize = 40;

struct Mounted(HostedApp);

impl Driveable for Mounted {
    fn survey(&mut self) -> Vec<ActionDescriptor> {
        self.0.survey()
    }
    fn controls(&mut self) -> Vec<Control> {
        self.0.controls()
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
            StepAction::Operate { control, input } => self.0.operate(control, input),
            StepAction::Open | StepAction::Continue(_) => Ok(()),
        }
    }
}

#[test]
fn every_web_app_takes_a_user_equivalent_edit() {
    let catalogue = catalogue();
    let mut table = String::from(
        "| App | Declared | Projected | Controls | Edit | Via |\n|---|---|---|---|---|---|\n",
    );
    let mut gaps = Vec::new();
    for definition in catalogue.iter() {
        let app = HostedApp::open(
            &catalogue,
            definition.app_id,
            ROOM,
            "coverage-agent",
            true,
            Seed::New,
        )
        .unwrap_or_else(|error| panic!("{} did not mount: {error}", definition.app_id));
        let mut app = Mounted(app);
        let _ = app.0.take_mutation();
        let survey = app.survey();
        let projected = survey.iter().filter(|d| is_semantic(d)).count();
        let declared = survey.len() - projected;
        let controls = app.controls().len();
        let (edit, refused) = find_edit(&mut app, ATTEMPTS);
        let (label, via) = edit.as_ref().map_or_else(
            || ("—".to_owned(), "—"),
            |edit| (edit.label.clone(), edit.path),
        );
        let _ = writeln!(
            table,
            "| {} | {declared} | {projected} | {controls} | `{label}` | {via} |",
            definition.app_id
        );
        let local = LOCAL_STATE
            .iter()
            .find(|(app, _)| *app == definition.app_id);
        if let (None, Some((_, why))) = (&edit, local) {
            assert!(
                refused
                    .iter()
                    .any(|attempt| attempt.ends_with(": no change")),
                "{}: the agent operated nothing: {refused:#?}",
                definition.app_id
            );
            let _ = writeln!(table, "|   ↳ local state | | | | {why} | |");
        } else if edit.is_none() {
            gaps.push(format!("{}: {refused:#?}", definition.app_id));
        } else {
            assert!(
                app.0.take_mutation().is_some(),
                "{}: an edit leaves a change to publish",
                definition.app_id
            );
        }
        app.0.close();
    }
    eprintln!("{table}");
    assert!(gaps.is_empty(), "agent coverage gaps:\n{}", gaps.join("\n"));
}

/// A projected control (`ui:<node>#focus`, `#activate`, `#set-value`) runs
/// through the headless door the same way it runs in a window.
#[test]
fn projected_controls_are_invocable_headless() {
    let catalogue = catalogue();
    for definition in catalogue.iter() {
        let mut app = HostedApp::open(
            &catalogue,
            definition.app_id,
            ROOM,
            "coverage-agent",
            true,
            Seed::New,
        )
        .unwrap();
        let control = app
            .survey()
            .into_iter()
            .find(|d| is_semantic(d) && d.address.action == "focus");
        if let Some(control) = control {
            app.invoke(&control.address, &serde_json::Map::new(), false)
                .unwrap_or_else(|error| {
                    panic!(
                        "{}: {}#focus: {}",
                        definition.app_id,
                        control.address.resource,
                        error.code()
                    )
                });
        }
        app.close();
    }
}

/// Custom-drawn nodes an app describes without a position. Their verbs
/// still work (a projected control carries the position); pointing at them
/// does not. May only fall: give the node `with_bounds` and lower this.
const MAX_UNPLACED_CONTROLS: usize = 88;

/// Adapted apps (a legacy root mounted through `AdaptedRoot`) export their
/// controls, and every projected control resolves to a point a person could
/// press.
#[test]
fn every_projected_action_has_a_position() {
    let catalogue = catalogue();
    let only = std::env::var("TILO_COVERAGE_APPS").unwrap_or_default();
    let mut missing = Vec::new();
    let mut unplaced = Vec::new();
    for definition in catalogue.iter() {
        if !only.is_empty() && !only.split(',').any(|id| id == definition.app_id) {
            continue;
        }
        let mut app = HostedApp::open(
            &catalogue,
            definition.app_id,
            ROOM,
            "coverage-agent",
            true,
            Seed::New,
        )
        .unwrap();
        let controls = app.controls();
        if controls.is_empty() {
            missing.push(format!("{}: no controls", definition.app_id));
        }
        unplaced.extend(
            controls
                .iter()
                .filter(|control| control.bounds.is_none())
                .map(|control| {
                    format!(
                        "{}: {:?} {:?}",
                        definition.app_id, control.role, control.name
                    )
                }),
        );
        for action in app.survey().into_iter().filter(is_semantic) {
            if app.pointer_for(&action.address).is_none() {
                missing.push(format!(
                    "{}: {} has no position",
                    definition.app_id, action.address.resource
                ));
            }
        }
        app.close();
    }
    missing.dedup();
    assert!(missing.is_empty(), "{}", missing.join("\n"));
    eprintln!(
        "{} controls without a position:\n{}",
        unplaced.len(),
        unplaced.join("\n")
    );
    assert!(
        !only.is_empty() || unplaced.len() <= MAX_UNPLACED_CONTROLS,
        "controls without a position grew to {} (allowed {MAX_UNPLACED_CONTROLS})",
        unplaced.len()
    );
}
