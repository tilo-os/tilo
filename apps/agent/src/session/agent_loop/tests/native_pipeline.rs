//! Native task steps: followed by their host's answers rather than a timer,
//! and run alongside each other when the model marks them standalone —
//! with the same rules as a Task Center run's.

use super::*;

/// A native workspace task's run (`native:` lease, a control frontier the
/// cancellation poll answers with): its steps are native children, each
/// re-asked by its stable id to be observed.
fn native_run_fixture(title: &str, world: World, turns: Vec<Vec<TurnEvent>>) -> Fixture {
    let control = runtime_collab::TaskRunControl {
        version: 1,
        task_id: "task:loop".into(),
        run_epoch: 1,
        from_revision: 0,
        applied_revision: 0,
        latest_revision: 0,
        directions: Vec::new(),
        has_more: false,
    };
    let mut f = fixture(
        title,
        World {
            native_control: Some(control),
            ..world
        },
        turns,
    );
    f.run.lease = "native:00000000000000000000000000000000:0".into();
    f.run.action_context = serde_json::to_value(
        tilo_actions::ActionContext::default()
            .with_purpose(title)
            .with_group(f.run.task_id.as_str(), title, "task"),
    )
    .unwrap();
    super::super::super::validate_task_run(&f.run).unwrap();
    f
}

/// A native run's terminal report.
fn native_terminal(sent: &[CollabRequest]) -> runtime_collab::TaskRunUpdate {
    legacy_terminal(sent)
}

/// The children the agent started, in order: one `InvokeTaskAction` each
/// with no wait (a re-ask always asks to be held).
fn children_started(sent: &[CollabRequest]) -> Vec<tilo_actions::InvocationId> {
    sent.iter()
        .filter_map(|request| match request {
            CollabRequest::InvokeTaskAction {
                invocation,
                wait: None,
                ..
            } => Some(*invocation),
            _ => None,
        })
        .collect()
}

/// Re-asks the agent sent for its children.
fn reasks(sent: &[CollabRequest]) -> Vec<runtime_collab::TaskChildWait> {
    sent.iter()
        .filter_map(|request| match request {
            CollabRequest::InvokeTaskAction {
                wait: Some(wait), ..
            } => Some(*wait),
            _ => None,
        })
        .collect()
}

/// Steps the model marks standalone, on records no running step acts on,
/// start without waiting for the ones before them to settle. Each is still
/// its own native child (its own request, id, admission and journal row),
/// and the model reads their results in the order it wrote the calls.
#[test]
fn standalone_native_steps_run_alongside_each_other_in_plan_order() {
    let mut f = native_run_fixture(
        "post the standup change and start the agenda and the minutes",
        World {
            second_window: Some("doc-2"),
            settle_after: Some(Duration::from_millis(10)),
            settle_gate: 3,
            ..World::default()
        },
        vec![
            turn(&three_records(true), 24),
            turn(&[finish("Posted and started both documents.", true)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let world = f.world.borrow();
    let started = &world.started;
    assert_eq!(started.len(), 3, "{started:?}");
    let first_settles = started[0].2;
    for (action, began, _) in started {
        assert!(
            *began < first_settles,
            "{action} waited for the first step to settle: {started:?}"
        );
    }
    let children = children_started(&f.sent.borrow());
    assert_eq!(children.len(), 3, "one request per step, nothing batched");
    let distinct: HashSet<_> = children.iter().collect();
    assert_eq!(
        distinct.len(),
        3,
        "each step is its own child: {children:?}"
    );
    let bodies = f.script.bodies.lock().unwrap().clone();
    let results = tool_results(&bodies[1]);
    let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["call_0", "call_1", "call_2"], "plan order");
    for (id, result) in &results {
        assert_eq!(result["result"], "succeeded", "{id}: {result}");
    }
    assert_eq!(
        native_terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Posted and started both documents.".into()
        }
    );
}

/// An unmarked native step, or one on a record a running step acts on,
/// starts only after every running step has settled.
#[test]
fn a_native_step_that_may_read_earlier_writes_waits_for_them() {
    let mut f = native_run_fixture(
        "post, write a heading, then type under it and confirm in Chat",
        World {
            settle_after: Some(Duration::from_millis(10)),
            settle_gate: 2,
            ..World::default()
        },
        vec![
            turn(
                &[
                    invoke_on(
                        CHAT_PEER,
                        "chat.channel:general",
                        "post",
                        &serde_json::json!({"message": "Writing the notes."}),
                        true,
                    ),
                    invoke_on(
                        PEER,
                        DOC,
                        "append-paragraph",
                        &serde_json::json!({"text": "Notes"}),
                        true,
                    ),
                    invoke_on(
                        PEER,
                        DOC,
                        "type-text",
                        &serde_json::json!({"text": " (draft)"}),
                        true,
                    ),
                    invoke_on(
                        CHAT_PEER,
                        "chat.channel:general",
                        "post",
                        &serde_json::json!({"message": "Notes are drafted."}),
                        false,
                    ),
                ],
                24,
            ),
            turn(&[finish("Done.", true)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let world = f.world.borrow();
    let started = &world.started;
    let actions: Vec<&str> = started
        .iter()
        .map(|(action, _, _)| action.as_str())
        .collect();
    assert_eq!(actions, ["post", "append-paragraph", "type-text", "post"]);
    assert!(started[1].1 < started[0].2, "standalone: alongside");
    assert!(
        started[2].1 >= started[0].2 && started[2].1 >= started[1].2,
        "the same record waits for everything running: {started:?}"
    );
    assert!(
        started[3].1 >= started[2].2,
        "an unmarked step waits for the one before it: {started:?}"
    );
    assert_eq!(world.blocks[0].1, "Notes (draft)");
    assert_eq!(world.posts, ["Writing the notes.", "Notes are drafted."]);
}

/// Cancelling the native run from Task Center stops every step still
/// running, not only one being waited on: the host cancels each child when
/// it reads the run cancelled, and the run ends cancelled.
#[test]
fn cancelling_a_native_task_stops_every_step_in_flight() {
    let mut f = native_run_fixture(
        "post the change and start the agenda",
        World {
            // Longer than the test may run: the steps end cancelled.
            settle_after: Some(tilo_test_env::hang_bound()),
            cancel_after_steps: Some(2),
            ..World::default()
        },
        vec![turn(
            &[
                invoke_on(
                    CHAT_PEER,
                    "chat.channel:general",
                    "post",
                    &serde_json::json!({"message": "Standup moved."}),
                    true,
                ),
                invoke_on(
                    PEER,
                    DOC,
                    "append-paragraph",
                    &serde_json::json!({"text": "Agenda"}),
                    true,
                ),
                finish("Done.", true),
            ],
            24,
        )],
    );
    f.session.run_task(&f.run).unwrap();
    let world = f.world.borrow();
    assert_eq!(world.started.len(), 2);
    let started: HashSet<_> = children_started(&f.sent.borrow()).into_iter().collect();
    let cancelled: HashSet<_> = world.cancelled.iter().copied().collect();
    assert_eq!(cancelled, started, "every running child is cancelled");
    assert!(matches!(
        native_terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Cancelled { .. }
    ));
}

/// The host refusing one standalone native step (an authorization
/// refusal) does not stop the others; an unmarked step is still skipped
/// after a failure.
#[test]
fn a_refused_native_step_does_not_stop_the_others() {
    let mut f = native_run_fixture(
        "post the change and start the agenda",
        World {
            settle_after: Some(Duration::from_millis(100)),
            deny: vec!["post".into()],
            ..World::default()
        },
        vec![
            turn(
                &[
                    invoke_on(
                        CHAT_PEER,
                        "chat.channel:general",
                        "post",
                        &serde_json::json!({"message": "Standup moved."}),
                        true,
                    ),
                    invoke_on(
                        PEER,
                        DOC,
                        "append-paragraph",
                        &serde_json::json!({"text": "Agenda"}),
                        true,
                    ),
                    invoke_on(
                        PEER,
                        DOC,
                        "type-text",
                        &serde_json::json!({"text": " for Monday"}),
                        false,
                    ),
                ],
                24,
            ),
            turn(&[finish("Started the agenda; I may not post.", true)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let world = f.world.borrow();
    assert!(world.posts.is_empty(), "the refused post never ran");
    assert_eq!(world.blocks, [("p1".to_owned(), "Agenda".to_owned())]);
    let bodies = f.script.bodies.lock().unwrap().clone();
    let results = tool_results(&bodies[1]);
    assert_eq!(results[0].1["result"], "failed", "{:?}", results[0]);
    assert!(
        results[0].1["error"]
            .as_str()
            .unwrap()
            .contains("permission denied"),
        "{:?}",
        results[0]
    );
    assert_eq!(results[1].1["result"], "succeeded", "{:?}", results[1]);
    assert_eq!(results[2].1["result"], "not run", "{:?}", results[2]);
    assert_eq!(
        children_started(&f.sent.borrow()).len(),
        2,
        "each step asked the host on its own; the skipped one never"
    );
}

/// A running native step is followed by its host's answer, not a timer:
/// the one re-ask out is held until the child settles, so it is answered
/// once, with the outcome, and no round trip comes back empty.
#[test]
fn a_native_step_is_answered_when_it_settles_not_on_a_timer() {
    let mut f = native_run_fixture(
        "post the change",
        World {
            settle_after: Some(Duration::from_millis(250)),
            // However long the machine takes to get there, the re-ask is
            // still held when the child settles.
            holds_until_news: true,
            ..World::default()
        },
        vec![
            turn(
                &[invoke_on(
                    CHAT_PEER,
                    "chat.channel:general",
                    "post",
                    &serde_json::json!({"message": "Standup moved."}),
                    false,
                )],
                24,
            ),
            turn(&[finish("Posted.", true)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let world = f.world.borrow();
    assert_eq!(world.posts, ["Standup moved."]);
    let reasks = reasks(&f.sent.borrow());
    assert_eq!(
        reasks.len(),
        1,
        "one held re-ask, answered when the child settled: {reasks:?}"
    );
    assert_eq!(reasks[0].seen, tilo_actions::remote::UpdateMark::Accepted);
    assert!(reasks[0].wait_ms > 0);
    assert_eq!(world.observations, 1);
    assert_eq!(world.idle_observations, 0, "no answer came back empty");
}

/// A host from before held re-asks answers at once, with nothing new: the
/// agent then re-asks at the old cadence, never in a loop.
#[test]
fn a_host_that_answers_reasks_at_once_is_paced_not_spun() {
    let mut f = native_run_fixture(
        "post the change",
        World {
            // The child settles when the fourth re-ask reaches the host.
            settle_after: Some(tilo_test_env::hang_bound()),
            settles_at_reask: Some(4),
            answers_reasks_at_once: true,
            ..World::default()
        },
        vec![
            turn(
                &[invoke_on(
                    CHAT_PEER,
                    "chat.channel:general",
                    "post",
                    &serde_json::json!({"message": "Standup moved."}),
                    false,
                )],
                24,
            ),
            turn(&[finish("Posted.", true)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let world = f.world.borrow();
    assert_eq!(world.posts, ["Standup moved."]);
    assert_eq!(world.observations, 4);
    assert_eq!(world.idle_observations, 3);
    // Each re-ask after an empty answer waits for the old cadence, counted
    // from when the agent sent the one before (a moment before the host
    // took it in); a loop would send the next at once.
    let gaps: Vec<Duration> = world
        .reasked_at
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect();
    assert!(
        gaps.iter()
            .all(|gap| *gap >= crate::session::native_task::REASK_PACE / 2),
        "re-asked {gaps:?} apart"
    );
}

/// One measured run of the benchmark plan.
struct Measured {
    wall_ms: f64,
    /// Host round trips while the steps ran that brought no news: re-asks
    /// that found the child still running, and cancellation polls.
    idle_per_step: f64,
}

/// The native benchmark: the three-step plan on three records, every step
/// taking `settle` in its application, run as the model marks it
/// (standalone) and unmarked (in order). Wall time is the whole task; idle
/// wakeups are the host round trips per step that brought no news while a
/// step ran. `TILO_AGENT_NATIVE_SAMPLES` names a directory to write the
/// samples to (`pipelined.txt`, `sequential.txt` in ms per task, and
/// `idle.txt` in idle round trips per step of the sequential plan), and
/// `TILO_AGENT_NATIVE_REPS` how many runs of each to take.
#[test]
fn native_task_benchmark() {
    use std::fmt::Write as _;
    let settle = Duration::from_millis(100);
    // Measured only when asked for (the scheduled perf run): an ordinary
    // run checks the plan runs either way, not how fast this machine is.
    let measured: Option<usize> = std::env::var("TILO_AGENT_NATIVE_REPS")
        .ok()
        .and_then(|reps| reps.parse().ok());
    let reps = measured.unwrap_or(1);
    let run_once = |independent: bool| {
        let mut f = native_run_fixture(
            "post the standup change and start the agenda and the minutes",
            World {
                second_window: Some("doc-2"),
                settle_after: Some(settle),
                ..World::default()
            },
            vec![turn(
                &[
                    three_records(independent),
                    vec![finish("Posted and started both documents.", true)],
                ]
                .concat(),
                24,
            )],
        );
        let began = std::time::Instant::now();
        f.session.run_task(&f.run).unwrap();
        let wall_ms = began.elapsed().as_secs_f64() * 1000.0;
        let world = f.world.borrow();
        assert_eq!(world.started.len(), 3);
        assert!(matches!(
            native_terminal(&f.sent.borrow()),
            runtime_collab::TaskRunUpdate::Succeeded { .. }
        ));
        let idle = world.idle_observations + world.cancellation_checks;
        Measured {
            wall_ms,
            idle_per_step: idle as f64 / 3.0,
        }
    };
    let mut pipelined = Vec::new();
    let mut sequential = Vec::new();
    for _ in 0..reps {
        pipelined.push(run_once(true));
        sequential.push(run_once(false));
    }
    let median = |samples: &[f64]| {
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted[sorted.len() / 2]
    };
    let walls = |runs: &[Measured]| runs.iter().map(|run| run.wall_ms).collect::<Vec<_>>();
    let idles = |runs: &[Measured]| runs.iter().map(|run| run.idle_per_step).collect::<Vec<_>>();
    let (fast, slow) = (median(&walls(&pipelined)), median(&walls(&sequential)));
    let idle = median(&idles(&sequential));
    eprintln!(
        "native agent task, 3 steps x {settle:?}: pipelined {fast:.1} ms, sequential {slow:.1} ms \
         ({:.1} ms per step), {idle:.1} idle host round trips per step",
        slow / 3.0
    );
    if let Ok(dir) = std::env::var("TILO_AGENT_NATIVE_SAMPLES") {
        let dir = std::path::Path::new(&dir);
        std::fs::create_dir_all(dir).unwrap();
        for (name, samples) in [
            ("pipelined", walls(&pipelined)),
            ("sequential", walls(&sequential)),
            ("idle", idles(&sequential)),
        ] {
            let mut lines = String::new();
            for value in samples {
                let _ = writeln!(lines, "{value:.3}");
            }
            std::fs::write(dir.join(format!("{name}.txt")), lines).unwrap();
        }
    }
    // Three steps each settling in `settle` cannot finish in order in less
    // than three times it; alongside each other they need it once.
    let floor = settle.as_secs_f64() * 1000.0;
    assert!(slow >= 3.0 * floor, "sequential {slow:.1} ms");
    if measured.is_some() {
        assert!(
            fast < 2.0 * floor,
            "pipelined {fast:.1} ms should need one settle ({floor} ms), not three"
        );
    }
}

/// The descriptor of Writer's `append-paragraph`, as the agent reads it.
fn drafted(f: &mut Fixture) -> tilo_actions::ActionDescriptor {
    let catalogue = f.session.task_catalogue();
    surface::find(&catalogue.offers, &op("append-paragraph"))
        .unwrap()
        .descriptor
        .clone()
}

/// A run delivered again after its agent restarted, while one of its steps
/// waited for the person: the agent takes up exactly the drafted step before
/// the model plans anything, waits for the person's answer as before, and
/// tells the model what happened instead of planning the task over.
#[test]
fn a_redelivered_run_resumes_at_its_paused_step_instead_of_planning_over() {
    let mut f = native_run_fixture(
        "add a closing line",
        World {
            awaiting: Some(("append-paragraph".into(), 1)),
            ..World::default()
        },
        vec![turn(&[finish("Added the closing line.", true)], 24)],
    );
    let arguments = serde_json::Map::from_iter([("text".to_owned(), Value::from("Kind regards"))]);
    let descriptor = drafted(&mut f);
    f.run.paused = Some(runtime_collab::TaskRunPausedStep {
        app: "so.tilo.word-processor".into(),
        resource: DOC.into(),
        action: "append-paragraph".into(),
        draft: tilo_actions::Draft::render(&descriptor, &arguments),
        arguments_digest: tilo_actions::draft::arguments_digest(&arguments),
    });
    f.session.run_task(&f.run).unwrap();

    let sent = f.sent.borrow();
    let asks: Vec<_> = sent
        .iter()
        .filter_map(|request| match request {
            CollabRequest::InvokeTaskAction {
                invocation,
                action,
                arguments,
                wait: None,
                ..
            } => Some((*invocation, action.clone(), arguments.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(asks.len(), 2, "asked, then asked again once: {asks:?}");
    for (invocation, action, asked) in &asks {
        assert_eq!(action, "append-paragraph");
        assert_eq!(asked, &arguments, "exactly the drafted arguments");
        assert_eq!(*invocation, asks[0].0, "one child, asked again by its id");
    }
    let world = f.world.borrow();
    assert_eq!(world.steps.len(), 1, "the step ran once: {:?}", world.steps);
    assert_eq!(world.blocks.last().unwrap().1, "Kind regards");
    drop(world);
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1, "the model planned only what was left");
    let first = bodies[0].to_string();
    assert!(first.contains("has resumed at that step"), "{first}");
    assert!(first.contains("Append paragraph"), "{first}");
    assert_eq!(
        native_terminal(&sent),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Added the closing line.".into()
        }
    );
}

/// After a restart the step's resource is published through a new window,
/// under a new alias: it is still the same resource, so the run resumes at
/// it rather than leaving it to the model.
#[test]
fn a_paused_step_resumes_through_the_window_that_publishes_it_now() {
    let mut f = native_run_fixture(
        "add a closing line",
        World {
            awaiting: Some(("append-paragraph".into(), 1)),
            ..World::default()
        },
        vec![turn(&[finish("Added the closing line.", true)], 24)],
    );
    let arguments = serde_json::Map::from_iter([("text".to_owned(), Value::from("Kind regards"))]);
    let descriptor = drafted(&mut f);
    let hex = DOC.bytes().fold(String::new(), |mut hex, byte| {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    f.run.paused = Some(runtime_collab::TaskRunPausedStep {
        app: "so.tilo.word-processor".into(),
        resource: format!(
            "{}old-window:publication:{hex}",
            tilo_actions::selection::BOUND_RESOURCE_PREFIX
        ),
        action: "append-paragraph".into(),
        draft: tilo_actions::Draft::render(&descriptor, &arguments),
        arguments_digest: tilo_actions::draft::arguments_digest(&arguments),
    });
    f.session.run_task(&f.run).unwrap();
    let world = f.world.borrow();
    assert_eq!(world.steps.len(), 1, "the step ran once: {:?}", world.steps);
    drop(world);
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert!(bodies[0].to_string().contains("has resumed at that step"));
}

/// A paused step whose draft does not give back the exact arguments it was
/// confirmed for is not guessed at: nothing runs before the model, which is
/// told what waits so that it asks for that step again.
#[test]
fn a_paused_step_the_draft_cannot_rebuild_is_left_to_the_model() {
    let mut f = native_run_fixture(
        "add a closing line",
        World::default(),
        vec![turn(&[finish("Waiting on the person.", true)], 24)],
    );
    let arguments = serde_json::Map::from_iter([("text".to_owned(), Value::from("Kind regards"))]);
    let descriptor = drafted(&mut f);
    f.run.paused = Some(runtime_collab::TaskRunPausedStep {
        app: "so.tilo.word-processor".into(),
        resource: DOC.into(),
        action: "append-paragraph".into(),
        draft: tilo_actions::Draft::render(&descriptor, &arguments),
        arguments_digest: [7; 32],
    });
    f.session.run_task(&f.run).unwrap();
    assert!(children_started(&f.sent.borrow()).is_empty());
    let bodies = f.script.bodies.lock().unwrap().clone();
    let first = bodies[0].to_string();
    assert!(first.contains("still waits for them"), "{first}");
    assert!(first.contains("Kind regards"), "{first}");
}

/// A native child's id names its request: the same request in a run
/// delivered again (a new agent process) finds the same child, a different
/// request at the same position is a different child, and asking the same
/// thing twice in one run makes two children.
#[test]
fn a_native_child_id_names_its_request_not_its_position() {
    let ids = |requests: &[&str]| {
        let mut f = native_run_fixture("ids", World::default(), Vec::new());
        f.session.native_task = Some(crate::session::native_task::NativeTaskState {
            run_id: f.run.run_id,
            task_id: f.run.task_id.clone(),
            base: "00000000000000000000000000000000".into(),
            ..Default::default()
        });
        let offers = f.session.task_catalogue();
        requests
            .iter()
            .map(|text| {
                let arguments =
                    serde_json::Map::from_iter([("text".to_owned(), Value::from(*text))]);
                let offer = surface::find(&offers.offers, &op("append-paragraph")).unwrap();
                let action = task_action_from_offer(offer, arguments.clone()).unwrap();
                f.session
                    .task_child_id(&f.run, &action, &arguments)
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };
    let before = ids(&["a", "b", "a"]);
    let after = ids(&["b", "a"]);
    assert_ne!(before[0], before[2], "asking twice makes two children");
    assert_eq!(
        after[0], before[1],
        "the same request finds its child again"
    );
    assert_eq!(after[1], before[0]);
    assert_ne!(after[0], before[0], "another request at the same position");
}
