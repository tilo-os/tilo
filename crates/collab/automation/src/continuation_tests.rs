//! Runs that go on from a step of an earlier one: what a person or an agent
//! asks for when they fix a step of a shortcut that failed and run it again
//! without repeating the steps before it.

use serde_json::json;

use crate::tests::{
    FakeHost, Hooks, INBOX, MAIL, NOTES, app_rule, block_on, caller, failure, invoke, list,
    run_with, shortcut,
};
use crate::{
    Binding, Condition, NoShortcuts, RunId, RunMode, RunOutcome, RunReport, RunRequest, RunTrigger,
    Selector, Shortcut, Step, StepKind, StepPath, StepStatus, check, run,
};
use tilo_actions::remote::InvokeArgument;

/// Run `shortcut` again from `from`, on top of what `origin` recorded. A
/// continued run is a run of its own, as the door starts one: the steps
/// before `from` are taken from `origin`'s record, and `from` and the rest
/// run under this run's own invocations.
fn continue_from(
    host: &FakeHost,
    shortcut: &Shortcut,
    origin: &RunReport,
    from: &str,
) -> RunReport {
    block_on(run(
        host,
        &Hooks::default(),
        RunRequest {
            shortcut,
            run: RunId::new(),
            caller: caller(),
            mode: RunMode::Unattended,
            trigger: RunTrigger::Manual,
            inputs: serde_json::Map::new(),
            caused_by: None,
            parent: None,
            chain: Vec::new(),
            resume: Some(origin),
            from: Some(StepPath::parse(from)),
        },
    ))
}

#[test]
fn continuing_from_a_step_does_not_do_the_steps_before_it_again() {
    let host = FakeHost::new();
    let tidy = shortcut(
        "tidy",
        vec![
            list(),
            invoke(
                "make",
                NOTES,
                Selector::Fixed(INBOX.into()),
                "create",
                &[("title", Binding::step("list", "/titles/0"))],
            ),
            invoke(
                "send",
                MAIL,
                Selector::Fixed("mail.outbox:main".into()),
                "send",
                &[("body", Binding::Template("there are {input/count}".into()))],
            ),
        ],
    );
    let mut sendable = tidy;
    sendable.grant.push(app_rule(MAIL));
    assert_eq!(check(&sendable, &host.catalog(), &NoShortcuts), Ok(()));
    let first = run_with(
        &host,
        &Hooks::default(),
        &sendable,
        RunId::new(),
        RunMode::Unattended,
    );
    assert_eq!(first.outcome, RunOutcome::Completed, "{first:?}");
    assert_eq!(
        host.started().len(),
        3,
        "the first run does all three steps"
    );

    // The person fixed the third step and runs from it.
    let mut fixed = sendable.clone();
    fixed.steps[2] = invoke(
        "send",
        MAIL,
        Selector::Fixed("mail.outbox:main".into()),
        "send",
        &[("body", Binding::Literal(json!("two notes")))],
    );
    let second = continue_from(&host, &fixed, &first, "2");
    assert_eq!(second.outcome, RunOutcome::Completed, "{second:?}");
    let started = host.started();
    assert_eq!(
        started
            .iter()
            .map(|request| request.target.address.action.as_str())
            .collect::<Vec<_>>()
            .as_slice(),
        ["list", "create", "send", "send"],
        "continuing from the third step runs the third step and nothing before it"
    );
    assert_eq!(
        started.get(2).map(|request| request.argument.clone()),
        // The mail step ran again, from the edited binding.
        Some(InvokeArgument::Json(
            json!({"body": "there are {input/count}"})
                .as_object()
                .unwrap()
                .clone()
        ))
    );
    // The steps before the one it goes on from are taken from what the first
    // run produced: the same steps, under the same invocations, with the
    // output that ran. A run of its own would have given them fresh ones.
    assert_ne!(second.run, first.run, "a continued run is a run of its own");
    assert_eq!(second.steps.len(), 3, "{second:?}");
    for (earlier, later) in first.steps.iter().zip(&second.steps).take(2) {
        assert_eq!(later.path, earlier.path);
        assert_eq!(later.step, earlier.step);
        assert_eq!(later.invocation, earlier.invocation);
        // A taken step reports what it produced, as the first run did, so a
        // report of the continued run shows the list's output.
        assert_eq!(later.status, earlier.status);
    }
    assert!(
        matches!(
            &second.steps[0].status,
            StepStatus::Succeeded { output: Some(_) }
        ),
        "the report shows what the list produced: {:?}",
        second.steps[0]
    );
    assert_ne!(
        second.steps[2].invocation, first.steps[2].invocation,
        "the step the person fixed runs again, under this run's own invocation"
    );
}

#[test]
fn a_continued_step_reads_the_output_an_earlier_run_produced() {
    let host = FakeHost::new();
    let tidy = shortcut(
        "tidy",
        vec![
            list(),
            invoke(
                "make",
                NOTES,
                Selector::Fixed(INBOX.into()),
                "create",
                &[("title", Binding::step("list", "/titles/1"))],
            ),
        ],
    );
    let first = run_with(
        &host,
        &Hooks::default(),
        &tidy,
        RunId::new(),
        RunMode::Unattended,
    );
    assert_eq!(first.outcome, RunOutcome::Completed, "{first:?}");
    // The second step now reads the list differently; the list itself must
    // not run again, so the value it reads is the one the first run saw.
    let mut fixed = tidy;
    fixed.steps[1] = invoke(
        "make",
        NOTES,
        Selector::Fixed(INBOX.into()),
        "create",
        &[("title", Binding::step("list", "/titles/0"))],
    );
    let second = continue_from(&host, &fixed, &first, "1");
    assert_eq!(second.outcome, RunOutcome::Completed, "{second:?}");
    let started = host.started();
    assert_eq!(
        started
            .iter()
            .map(|request| request.target.address.action.as_str())
            .collect::<Vec<_>>(),
        ["list", "create", "create"],
        "the list is read once: the continued run takes it from the invocation"
    );
    assert_eq!(
        started.get(2).map(|request| request.argument.clone()),
        Some(InvokeArgument::Json(
            json!({"title": "a"}).as_object().unwrap().clone()
        )),
        "and the list it reads is the one the first run saw"
    );
}

#[test]
fn a_loop_and_a_branch_continue_from_the_iterations_and_branch_they_took() {
    let host = FakeHost::new();
    let tidy = shortcut(
        "tidy",
        vec![
            list(),
            Step::new(
                "each",
                StepKind::ForEach {
                    items: Binding::step("list", "/titles"),
                    item: "title".into(),
                    body: vec![invoke(
                        "make",
                        NOTES,
                        Selector::Fixed(INBOX.into()),
                        "create",
                        &[("title", Binding::Var("title".into()))],
                    )],
                },
            ),
            Step::new(
                "two",
                StepKind::If {
                    cond: Condition::Equals {
                        left: Binding::step("list", "/count"),
                        right: Binding::Literal(json!(2)),
                    },
                    then: vec![invoke(
                        "archive",
                        NOTES,
                        Selector::FirstOfKind("notes.note".into()),
                        "archive",
                        &[],
                    )],
                    otherwise: Vec::new(),
                },
            ),
        ],
    );
    let first = run_with(
        &host,
        &Hooks::default(),
        &tidy,
        RunId::new(),
        RunMode::Unattended,
    );
    assert_eq!(first.outcome, RunOutcome::Completed, "{first:?}");
    // The list, then both loop iterations, then the branch's step.
    assert_eq!(
        host.started()
            .iter()
            .map(|request| request.target.address.action.as_str())
            .collect::<Vec<_>>(),
        ["list", "create", "create", "archive"]
    );

    // The branch's step already ran: continuing from the branch takes it as
    // taken. The list and the loop are not run again either.
    let second = continue_from(&host, &tidy, &first, "2");
    assert_eq!(second.outcome, RunOutcome::Completed, "{second:?}");
    let started = host.started();
    assert_eq!(
        started
            .iter()
            .map(|request| request.target.address.action.as_str())
            .collect::<Vec<_>>(),
        ["list", "create", "create", "archive", "archive"],
        "the list and the loop are taken from the record, and the branch runs again"
    );
    let paths: Vec<_> = second.steps.iter().map(|step| step.path.as_str()).collect();
    assert_eq!(
        paths,
        ["0", "1", "1.each[0].0", "1.each[1].0", "2", "2.then.0"]
    );

    // A step inside the branch continues too. Everything before it is taken
    // — the list, both iterations, the branch itself — and the step it goes
    // on from runs again, as the step it named always does.
    let third = continue_from(&host, &tidy, &first, "2.then.0");
    assert_eq!(third.outcome, RunOutcome::Completed, "{third:?}");
    assert_eq!(
        host.started()
            .iter()
            .map(|request| request.target.address.action.as_str())
            .collect::<Vec<_>>(),
        ["list", "create", "create", "archive", "archive", "archive"],
        "only the step it goes on from reaches the host again"
    );
}

#[test]
fn continuing_from_a_failed_step_runs_that_step_again_and_the_rest() {
    let host = FakeHost::new();
    let tidy = shortcut(
        "tidy",
        vec![
            list(),
            invoke("send", NOTES, Selector::Fixed(INBOX.into()), "send", &[]),
        ],
    );
    let first = run_with(
        &host,
        &Hooks::default(),
        &tidy,
        RunId::new(),
        RunMode::Unattended,
    );
    assert!(
        matches!(failure(&first), Some((path, _)) if path.as_str() == "1"),
        "{first:?}"
    );
    assert_eq!(
        host.started()
            .iter()
            .map(|request| request.target.address.action.as_str())
            .collect::<Vec<_>>(),
        ["list"],
        "the run stops at the step that failed: nothing reaches the host for it"
    );

    // The person fixed the step that failed: continuing from it runs it
    // again, and only it.
    let mut fixed = tidy;
    fixed.steps[1] = invoke(
        "make",
        NOTES,
        Selector::Fixed(INBOX.into()),
        "create",
        &[("title", Binding::Literal(json!("fixed")))],
    );
    let second = continue_from(&host, &fixed, &first, "1");
    assert_eq!(second.outcome, RunOutcome::Completed, "{second:?}");
    assert_eq!(
        host.started()
            .iter()
            .map(|request| request.target.address.action.as_str())
            .collect::<Vec<_>>(),
        ["list", "create"],
        "the step that failed is the one the person came to fix: it runs again, and the list it read does not"
    );
}
