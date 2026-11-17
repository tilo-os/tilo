//! A run's steps: followed by their host's answers rather than a timer,
//! and run alongside each other when the model marks them standalone.

use super::*;

/// The steps the agent started, in order: one invoke each.
fn children_started(sent: &[CollabRequest]) -> Vec<tilo_actions::InvocationId> {
    sent.iter()
        .filter_map(invoked)
        .map(|(invocation, _, _, _)| invocation)
        .collect()
}

/// The asks the agent sent about its running steps: how long each asked to be held, and what it
/// had seen.
fn reasks(sent: &[CollabRequest]) -> Vec<(u32, tilo_actions::remote::UpdateMark)> {
    sent.iter()
        .filter_map(|request| match request {
            CollabRequest::RemoteAction {
                message:
                    tilo_actions::remote::RemoteActionMessage::Observe {
                        wait_ms: Some(wait_ms),
                        seen: Some(seen),
                        ..
                    },
            } => Some((*wait_ms, *seen)),
            _ => None,
        })
        .collect()
}

/// One measured run of the benchmark plan.
struct Measured {
    wall_ms: f64,
    /// Host round trips while the steps ran that brought no news: asks that found the step still
    /// running.
    idle_per_step: f64,
}

/// The native benchmark: the three-step plan on three resources, every step
/// taking `settle` in its application, run as the model marks it
/// (standalone) and unmarked (in order). Wall time is the whole task; idle
/// wakeups are the host round trips per step that brought no news while a
/// step ran. `TILO_AGENT_NATIVE_SAMPLES` names a directory to write the
/// samples to (`pipelined.txt`, `sequential.txt` in ms per task, and
/// `idle.txt` in idle round trips per step of the sequential plan), and
/// `TILO_AGENT_NATIVE_REPS` how many runs of each to take.
#[test]
fn turn_steps_benchmark() {
    use std::fmt::Write as _;
    let settle = Duration::from_millis(100);
    // Measured only when asked for (the scheduled perf run): an ordinary
    // run checks the plan runs either way, not how fast this machine is.
    let measured: Option<usize> = std::env::var("TILO_AGENT_NATIVE_REPS")
        .ok()
        .and_then(|reps| reps.parse().ok());
    let reps = measured.unwrap_or(1);
    let run_once = |independent: bool| {
        let mut f = fixture(
            "post the standup change and start the agenda and the minutes",
            World {
                second_window: Some("doc-2"),
                settle_after: Some(settle),
                ..World::default()
            },
            vec![turn(
                &[
                    three_resources(independent),
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
            terminal(&f.sent.borrow()),
            TurnUpdate::Succeeded { .. }
        ));
        let idle = world.idle_observations;
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

tilo_test_env::scenario! {
    fn native_steps_run_wait_and_stop_as_their_plan_says;

    phase standalone_native_steps_run_alongside_each_other_in_plan_order;
    // Steps the model marks standalone, on resources no running step acts on,
    // start without waiting for the ones before them to settle. Each is still
    // its own native child (its own request, id, admission and journal row),
    // and the model reads their results in the order it wrote the calls.
    {
        let mut f = fixture(
            "post the standup change and start the agenda and the minutes",
            World {
                second_window: Some("doc-2"),
                settle_after: Some(Duration::from_millis(10)),
                settle_gate: 3,
                ..World::default()
            },
            vec![
                turn(&three_resources(true), 24),
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
        let ids: Vec<String> = results.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(ids, asked_ids(&bodies[1]), "plan order");
        for (id, result) in &results {
            assert_eq!(result["result"], "succeeded", "{id}: {result}");
        }
        assert_eq!(
            terminal(&f.sent.borrow()),
            TurnUpdate::Succeeded {
                message: "Posted and started both documents.".into()
            }
        );
    }

    phase a_native_step_that_may_read_earlier_writes_waits_for_them;
    // An unmarked native step, or one on a resource a running step acts on,
    // starts only after every running step has settled.
    {
        let mut f = fixture(
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
            "the same resource waits for everything running: {started:?}"
        );
        assert!(
            started[3].1 >= started[2].2,
            "an unmarked step waits for the one before it: {started:?}"
        );
        assert_eq!(world.blocks[0].1, "Notes (draft)");
        assert_eq!(world.posts, ["Writing the notes.", "Notes are drafted."]);
    }

    phase cancelling_a_native_task_stops_every_step_in_flight;
    // Cancelling the native run from Task Center stops every step still
    // running, not only one being waited on: the host cancels each child when
    // it reads the run cancelled, and the run ends cancelled.
    {
        let mut f = fixture(
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
            terminal(&f.sent.borrow()),
            TurnUpdate::Cancelled { .. }
        ));
    }

    phase a_refused_native_step_does_not_stop_the_others;
    // The host refusing one standalone native step (an authorization
    // refusal) does not stop the others; an unmarked step is still skipped
    // after a failure.
    {
        let mut f = fixture(
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

    phase a_native_step_is_answered_when_it_settles_not_on_a_timer;
    // A running native step is followed by its host's answer, not a timer:
    // the one re-ask out is held until the child settles, so it is answered
    // once, with the outcome, and no round trip comes back empty.
    {
        let mut f = fixture(
            "post the change",
            World {
                settle_after: Some(Duration::from_millis(250)),
                // However long the machine takes to get there, the re-ask is
                // still held when the child settles.
                reask_hold: ReaskHold::UntilNews,
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
        assert_eq!(reasks[0].1, tilo_actions::remote::UpdateMark::Accepted);
        assert!(reasks[0].0 > 0);
        assert_eq!(world.observations, 1);
        assert_eq!(world.idle_observations, 0, "no answer came back empty");
    }

    phase a_host_that_answers_reasks_at_once_is_paced_not_spun;
    // A host from before held re-asks answers at once, with nothing new: the
    // agent then re-asks at the old cadence, never in a loop.
    {
        let mut f = fixture(
            "post the change",
            World {
                settle_after: Some(tilo_test_env::hang_bound()),
                settles_at_reask: Some(4),
                reask_hold: ReaskHold::AnswerAtOnce,
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
                .all(|gap| *gap >= crate::session::steps::REASK_PACE / 2),
            "re-asked {gaps:?} apart"
        );
    }

    phase a_step_that_asks_the_person_is_held_until_they_answer;
    // A step that asks the person is held, not refused: the host answers the
    // ask with its draft waiting, the agent takes the child and follows it,
    // and the person's answer — arriving when the child may start — is what
    // ends the wait. No deadline of the step's own cuts it short
    // (`docs/designs/app-language/shell.md` §1, "Trust stays system-drawn").
    {
        let mut f = fixture(
            "Append the release line",
            World {
                awaiting: Some(("append-paragraph".into(), 2)),
                settle_after: Some(Duration::from_millis(10)),
                ..World::default()
            },
            vec![
                turn(
                    &[invoke(
                        "append-paragraph",
                        &serde_json::json!({"text": "Ship it"}),
                    )],
                    24,
                ),
                turn(&[finish("The line is appended.", true)], 64),
            ],
        );
        f.session.run_task(&f.run).unwrap();
        // The step waits on the person: the agent follows it with a held re-ask
        // rather than refusing it.
        assert!(
            f.sent.borrow().iter().any(|request| matches!(
                request,
                CollabRequest::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Confirm { .. }
                }
            )),
            "the step waits on the person: {:?}",
            f.sent.borrow()
        );
        assert_eq!(
            f.world.borrow().steps.len(),
            1,
            "the step ran once, after the person's answer"
        );
        assert_eq!(
            terminal(&f.sent.borrow()),
            TurnUpdate::Succeeded {
                message: "The line is appended.".into()
            }
        );
    }

    phase a_native_child_id_names_its_request_not_its_position;
    // A native child's id names its request: the same request in a run
    // delivered again (a new agent process) finds the same child, a different
    // request at the same position is a different child, and asking the same
    // thing twice in one run makes two children.
    {
        let ids = |requests: &[&str]| {
            let mut f = fixture("ids", World::default(), Vec::new());
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
}
