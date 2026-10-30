//! An application test drives its app the way everyone else does: through the
//! resources it publishes and the actions on them, and then reads the state
//! behind them — not the widget that drew them.
//!
//! `HeadlessApp` mounts a component in one frame and opens that door on it.
//! What the cases below assert is what a collaborator, a script and an agent
//! get: the resource ids they address, the action's own answer, and the
//! door's refusal — the wrong address, a parameter the contract did not
//! declare, a question the caller skipped.

use automerge::ReadDoc as _;
use serde_json::json;
use tilo_actions::ActionAddress;
use tilo_test_env::TestEnv;
use tilo_ui::application::{LaunchTarget, RestoreInput};
use tilo_ui::headless::{Action, HeadlessApp, Invocation, Outcome, Step};

use crate::support::application::{
    ProbeHost, body, operation_observations, owner_permissions, runner_probe_definition,
};

/// A step that invokes one action of the probe's shared resource with no
/// arguments, the way a scenario names what to do rather than where to
/// click.
fn invokes(action: &str) -> Step {
    Step::Action(Action::Invoke(Invocation {
        resource: "probe:shared".into(),
        action: action.into(),
        args: serde_json::Map::new(),
        confirm: false,
        confirmation_phrase: None,
        expect_error: None,
        bind: None,
    }))
}

/// A mounted probe that publishes its actions, and the host whose record
/// store it writes to — kept, because the store flushes on drop.
fn mounted_probe() -> (ProbeHost, HeadlessApp) {
    let host = ProbeHost::new();
    let component = runner_probe_definition()
        .instantiate(
            host.prepare_with_permissions(
                runner_probe_definition(),
                11,
                &LaunchTarget::New,
                owner_permissions(),
            )
            .unwrap(),
            LaunchTarget::New,
            RestoreInput::None,
        )
        .unwrap();
    // The frame is the window: it mounts the component itself.
    let app = HeadlessApp::mount(component).unwrap();
    (host, app)
}

#[test]
fn an_app_publishes_the_resources_a_caller_addresses_it_by() {
    // Case `the_mounted_app_publishes_its_own_resource`.
    {
        let (_host, app) = mounted_probe();
        assert!(
            app.publishes("probe:shared", "edit"),
            "the app publishes the action its callers address: {:?}",
            app.resources()
                .iter()
                .map(|resource| resource.id.0.clone())
                .collect::<Vec<_>>(),
        );
        assert!(!app.publishes("probe:shared", "absent"));
        let contract = app.contract("probe:shared", "edit").unwrap();
        assert_eq!(contract.id, "edit");
    }

    // Case `an_address_the_app_does_not_publish_is_refused_not_guessed`.
    {
        let (_host, mut app) = mounted_probe();
        let refused = app
            .call(
                &ActionAddress::new("probe:shared", "absent"),
                serde_json::Map::new(),
            )
            .unwrap_err();
        assert_eq!(refused.code(), "unknown_action");
    }
}

#[test]
fn an_action_runs_and_the_app_is_what_answers_for_it() {
    // Case `an_immediate_action_runs_and_answers_when_it_returns`.
    {
        let (host, mut app) = mounted_probe();
        let checkout = app.frame().component_record().cloned();
        let address = ActionAddress::new("probe:shared", "edit");
        let answer = app.call(&address, serde_json::Map::new()).unwrap();
        assert!(
            answer.completed().is_some(),
            "an immediate action answers on return",
        );
        let checkout = checkout.expect("the mounted app keeps the record it was launched on");
        assert_eq!(
            body(&host.records, &checkout),
            "edited",
            "the app applied what the action does",
        );
    }

    // Case `an_effect_runs_the_operation_the_app_declared_for_it`.
    {
        let (_host, mut app) = mounted_probe();
        let answer = app.call_on("probe:shared", "effect", json!({})).unwrap();
        assert!(answer.completed().is_some());
        let observed = operation_observations();
        let seen = observed.last().unwrap().borrow().clone();
        assert_eq!(seen, vec!["Effect"], "an effect has no document to edit");
    }

    // Case `an_argument_the_contract_does_not_accept_is_refused`.
    {
        let (_host, mut app) = mounted_probe();
        let refused = app
            .call_on("probe:shared", "typed", json!({ "value": "x" }))
            .unwrap_err();
        assert_eq!(
            refused.code(),
            "invalid_arguments",
            "a typed action binds every parameter its contract names",
        );
    }

    // Case `a_question_one_confirmation_answers_is_asked_first`.
    {
        let (_host, mut app) = mounted_probe();
        let refused = app
            .call_on("probe:shared", "confirm", json!({}))
            .unwrap_err();
        assert_eq!(refused.code(), "action_confirmation_required");
        app.call_on_confirmed("probe:shared", "confirm", json!({}))
            .expect("the caller answered the question it was asked");
    }
}

#[test]
fn a_scenario_drives_the_app_and_the_record_changes_it_made_are_what_it_asserts_on() {
    // Case `a_view_action_reads_without_leaving_an_undo_entry`.
    {
        let (host, mut app) = mounted_probe();
        let checkout = app
            .frame()
            .component_record()
            .cloned()
            .expect("the mounted app keeps the record it was launched on");
        let outcome = app.drive(&[invokes("view")]);
        assert!(outcome.is_pass(), "{:?}", outcome.failure());
        host.records
            .with_history(&checkout.id, |doc, history| {
                assert!(
                    history
                        .next_undo_in(doc, &crate::support::application::author(), 11)
                        .is_none(),
                    "reading leaves nothing to undo",
                );
            })
            .unwrap();
        assert_eq!(body(&host.records, &checkout), "initialized");
    }

    // Case `an_edit_action_makes_one_undoable_change_the_collaborator_can_undo`.
    {
        let (host, mut app) = mounted_probe();
        let checkout = app
            .frame()
            .component_record()
            .cloned()
            .expect("the mounted app keeps the record it was launched on");
        let outcome = app.drive(&[invokes("view"), invokes("edit")]);
        assert!(outcome.is_pass(), "{:?}", outcome.failure());
        assert_eq!(body(&host.records, &checkout), "edited");
        host.records
            .with_history(&checkout.id, |doc, history| {
                let author = crate::support::application::author();
                assert_eq!(
                    history
                        .next_undo_in(doc, &author, 11)
                        .unwrap()
                        .author
                        .as_ref(),
                    Some(&author),
                    "the change is attributed to the person who asked for it",
                );
                history.undo_in(doc, &author, 11, None, None).unwrap();
                assert!(history.next_undo_in(doc, &author, 11).is_none());
                assert_eq!(
                    doc.get(automerge::ROOT, "body")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("initialized"),
                    "undo reaches the state the app started in",
                );
            })
            .unwrap();
    }

    // Case `a_refusal_the_scenario_expected_is_an_outcome_not_a_failure`.
    {
        let (_host, mut app) = mounted_probe();
        let mut step = invokes("absent");
        let Step::Action(Action::Invoke(invocation)) = &mut step else {
            unreachable!("invokes builds an invocation");
        };
        invocation.expect_error = Some("unknown_action".into());
        let outcome = app.drive(&[step]);
        assert!(
            outcome.is_pass(),
            "a refusal the step predicted is the answer it asked for: {:?}",
            outcome.failure(),
        );
    }

    // Case `a_step_that_cannot_run_ends_the_scenario_and_names_itself`.
    {
        let (_host, mut app) = mounted_probe();
        let outcome: Outcome = app.drive(&[invokes("absent"), invokes("edit")]);
        let failure = outcome.failure().expect("the first step did not pass");
        assert_eq!(failure.index, 0, "the first step is the one that failed");
        let reason = failure.error.clone().unwrap_or_default();
        assert!(reason.contains("unknown_action"), "{reason}");
        assert_eq!(outcome.steps.len(), 1, "a step after the failure never ran");
    }
}

#[test]
fn an_environment_gives_the_app_a_home_and_a_clock_instead_of_the_developers_machine() {
    let env = TestEnv::new()
        .unwrap()
        .with_files(&[("notes/Todo.md", b"buy milk".as_slice())])
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(env.join("notes/Todo.md")).unwrap(),
        "buy milk",
        "a fixture the test wrote is where the app looks for it",
    );
    assert!(
        !env.network().external_allowed(),
        "a test reaches no host it did not name",
    );
    assert_eq!(
        env.today(),
        env.clock().local().date_naive(),
        "the day a test asserts on is the day its clock reads",
    );
}
