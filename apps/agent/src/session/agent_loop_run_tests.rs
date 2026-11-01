//! How a run ends, and what it says about what it did: an issue's run ending
//! on its own, a reply finishing its turn, a part that must be done or
//! explained, a bounded refusal, and a repeated failure called out.

use super::*;

/// Runs of several issues by the same agent each end on their own: one
/// the model could not complete goes back to its person with the
/// model's reply, and the agent still takes the next.
#[test]
fn each_issue_run_ends_on_its_own() {
    let mut f = fixture(
        "ignored",
        World::default(),
        vec![
            turn(
                &[
                    invoke("type-text", &serde_json::json!({"text": "First."})),
                    finish("Wrote the first line.", true),
                ],
                64,
            ),
            turn(
                &[finish(
                    "I can't create a note here: no Notes operation is available.",
                    false,
                )],
                64,
            ),
            turn(&[finish("Hello again!", true)], 64),
        ],
    );
    for (index, title) in ["write a line", "make a note", "say hi"]
        .into_iter()
        .enumerate()
    {
        let mut task = f.run.clone();
        task.task_id = format!("tasks.tracker/issues/{index}");
        task.title = title.into();
        task.run_id = 100 + u64::try_from(index).unwrap();
        bind_context(&mut task);
        f.world
            .borrow_mut()
            .native_control
            .as_mut()
            .unwrap()
            .task_id
            .clone_from(&task.task_id);
        f.session.run_task(&task).unwrap();
    }
    let terminals: Vec<runtime_collab::TaskRunUpdate> = f
        .sent
        .borrow()
        .iter()
        .filter_map(|request| match request {
            CollabRequest::ReportTaskRun { update, .. }
                if !matches!(
                    update,
                    runtime_collab::TaskRunUpdate::Progress { .. }
                        | runtime_collab::TaskRunUpdate::Accepted
                ) =>
            {
                Some(update.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [
            runtime_collab::TaskRunUpdate::Succeeded {
                message: "Wrote the first line.".into()
            },
            runtime_collab::TaskRunUpdate::Failed {
                message: "I can't create a note here: no Notes operation is available.".into()
            },
            runtime_collab::TaskRunUpdate::Succeeded {
                message: "Hello again!".into()
            },
        ]
    );
    assert_eq!(f.world.borrow().blocks[0].1, "First.");
    let accepted = f
        .sent
        .borrow()
        .iter()
        .filter(|request| {
            matches!(
                request,
                CollabRequest::ReportTaskRun {
                    update: runtime_collab::TaskRunUpdate::Accepted,
                    ..
                }
            )
        })
        .count();
    assert_eq!(accepted, 3);
}

/// The last invoke can carry the reply: the task finishes in the turn
/// that did the work, with no closing `plan` turn.
#[test]
fn an_invoke_carrying_the_reply_finishes_in_its_own_turn() {
    let mut f = fixture(
        "write hi",
        World::default(),
        vec![turn(
            &[invoke_replying(
                "type-text",
                &serde_json::json!({"text": "Hi."}),
                "Wrote hi.",
            )],
            64,
        )],
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.script.bodies.lock().unwrap().len(), 1);
    assert_eq!(f.world.borrow().blocks[0].1, "Hi.");
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Wrote hi.".into()
        }
    );
}

/// A reply on an invoke that fails does not finish the task.
#[test]
fn a_failed_invoke_carrying_the_reply_does_not_finish() {
    let world = World {
        refuse: vec!["type-text".into()],
        ..World::default()
    };
    let mut f = fixture(
        "write hi",
        world,
        vec![
            turn(
                &[invoke_replying(
                    "type-text",
                    &serde_json::json!({"text": "Hi."}),
                    "Wrote hi.",
                )],
                64,
            ),
            turn(&[finish("Typing was refused.", false)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.script.bodies.lock().unwrap().len(), 2);
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Failed {
            message: "Typing was refused.".into()
        }
    );
}

/// The prompt prefix (system, tools, catalogue) is byte-identical
/// across turns and runs so the provider's prompt cache holds it; the
/// request comes after it.
#[test]
fn the_prompt_prefix_is_stable_across_turns_and_runs() {
    let mut f = fixture(
        "ignored",
        World::default(),
        vec![
            turn(
                &[invoke_replying(
                    "type-text",
                    &serde_json::json!({"text": "Hello chat."}),
                    "Posted hello.",
                )],
                64,
            ),
            turn(&[(tools::LIST_APPLICATIONS, serde_json::json!({}))], 64),
            turn(
                &[finish("I typed \"Hello chat.\" into the document.", true)],
                64,
            ),
        ],
    );
    for title in ["post hello", "what did you just do?"] {
        let mut run = f.run.clone();
        run.title = title.into();
        bind_context(&mut run);
        f.session.run_task(&run).unwrap();
    }
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3);
    let user = |body: &Value| {
        body["messages"][1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["text"].as_str().unwrap())
            .collect::<String>()
    };
    let first = user(&bodies[0]);
    let second = user(&bodies[1]);
    assert!(!second.contains("Hello chat."), "{second}");
    assert!(second.ends_with("what did you just do?"), "{second}");
    // Stable prefix: same system and tools on every turn of every task,
    // and the catalogue opens both tasks' first message byte-identically.
    for body in &bodies {
        assert_eq!(body["tools"], bodies[0]["tools"]);
        assert_eq!(body["messages"][0], bodies[0]["messages"][0]);
    }
    let catalogue_end = first.find("The request:").unwrap();
    assert_eq!(first[..catalogue_end], second[..catalogue_end]);
    assert_eq!(
        bodies[2]["messages"].as_array().unwrap()[..2],
        bodies[1]["messages"].as_array().unwrap()[..2]
    );
    // The agent places no cache markers of its own: that is the gateway's.
    for body in &bodies {
        assert!(!body.to_string().contains("cache_control"), "{body}");
    }
}

/// "…then ping me in Chat": finishing after only the Writer work is
/// refused (a reply in the panel reaches nobody); the model gets another
/// turn, posts, and only then does the task finish.
#[test]
fn a_ping_in_chat_must_be_posted_before_the_task_finishes() {
    let mut f = fixture(
        "write hello in Writer, then ping me in Chat",
        World::default(),
        vec![
            turn(
                &[invoke_replying(
                    "type-text",
                    &serde_json::json!({"text": "Hello."}),
                    "Wrote hello, and pinging you now!",
                )],
                64,
            ),
            turn(
                &[
                    post("Done writing hello in Writer."),
                    finish("All done.", true),
                ],
                64,
            ),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2, "the premature finish cost one more turn");
    assert!(bodies[1]["messages"].to_string().contains("Not finished"));
    assert_eq!(f.world.borrow().posts, ["Done writing hello in Writer."]);
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "All done.".into()
        }
    );
}

/// The model's own `parts` are checked the same way; a part it cannot do
/// is accepted once it is listed under `unable` with the reason.
#[test]
fn listed_parts_must_be_done_or_explained() {
    let parts = serde_json::json!([
        {"part": "write hello", "app": "so.tilo.word-processor"},
        {"part": "add a note", "app": "so.tilo.notes"}
    ]);
    let mut first = invoke("type-text", &serde_json::json!({"text": "Hello."}));
    first.1["parts"] = parts;
    let mut f = fixture(
        "write hello and add a note",
        World::default(),
        vec![
            turn(&[first, finish("Done.", true)], 64),
            turn(
                &[(
                    tools::PLAN,
                    serde_json::json!({
                        "reply": "I wrote hello; I can't add a note here.",
                        "done": true,
                        "unable": ["add a note: no Notes operation is available"]
                    }),
                )],
                64,
            ),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    let second = bodies[1]["messages"].to_string();
    assert!(second.contains("add a note (in so.tilo.notes)"), "{second}");
    assert!(
        !second.contains("write hello (in"),
        "the done part is not listed: {second}"
    );
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "I wrote hello; I can't add a note here.".into()
        }
    );
}

/// Every part done, then the closing model turn fails (e.g. the model
/// ran into its token limit without a call): the task is finished, not
/// failed. With a part still undone the same failure stays a failure.
#[test]
fn a_failed_closing_turn_after_all_parts_are_done_finishes() {
    let mut f = fixture(
        "write hello in Writer, then ping me in Chat",
        World::default(),
        vec![
            turn(
                &[invoke("type-text", &serde_json::json!({"text": "Hello."}))],
                64,
            ),
            turn(&[post("Hello is in Writer.")], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.world.borrow().posts, ["Hello is in Writer."]);
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded { .. }
    ));

    let mut f = fixture(
        "write hello in Writer, then ping me in Chat",
        World::default(),
        vec![turn(
            &[invoke("type-text", &serde_json::json!({"text": "Hello."}))],
            64,
        )],
    );
    f.session.run_task(&f.run).unwrap();
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Failed { .. }
    ));
}

/// The check cannot trap the loop: after a bounded number of refusals a
/// finish is accepted.
#[test]
fn refusals_are_bounded() {
    let turns = (0..4)
        .map(|_| turn(&[finish("Pinged you!", true)], 64))
        .collect();
    let mut f = fixture("ping me in Chat", World::default(), turns);
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.script.bodies.lock().unwrap().len(), MOST_REFUSALS + 1);
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded { .. }
    ));
}

/// Two messages to post need two posts: one Chat success satisfies one
/// Chat part.
#[test]
fn each_listed_message_needs_its_own_post() {
    let mut first = post("Starting now.");
    first.1["parts"] = serde_json::json!([
        {"part": "say you're starting", "app": "so.tilo.chat"},
        {"part": "write a note", "app": "so.tilo.word-processor"},
        {"part": "ping when done", "app": "so.tilo.chat"}
    ]);
    first.1["reply"] = serde_json::json!("Started");
    let mut f = fixture(
        "Write in Chat that you're starting, write a short note in Writer, then ping me in Chat when done.",
        World::default(),
        vec![
            turn(&[first], 64),
            turn(
                &[
                    invoke("type-text", &serde_json::json!({"text": "A note."})),
                    finish("Done.", true),
                ],
                64,
            ),
            turn(&[post("Done!"), finish("All done.", true)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3);
    assert!(bodies[2]["messages"].to_string().contains("ping when done"));
    assert_eq!(f.world.borrow().posts, ["Starting now.", "Done!"]);
}

/// Lab regression: an invoke without its required text is refused
/// before any step, with what the operation takes and how to pass it,
/// so the next turn gets it right without a describe round trip.
#[test]
fn a_missing_parameter_is_answered_with_what_the_operation_takes() {
    let mut f = fixture(
        "append a paragraph saying hi",
        World::default(),
        vec![
            turn(&[invoke("append-paragraph", &serde_json::json!({}))], 64),
            turn(
                &[invoke_replying(
                    "append-paragraph",
                    &serde_json::json!({"text": "hi"}),
                    "Done.",
                )],
                64,
            ),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    let second = bodies[1]["messages"].to_string();
    assert!(second.contains("required argument `text`"), "{second}");
    assert!(second.contains("`text`: text (required)"), "{second}");
    assert_eq!(
        f.world.borrow().blocks,
        [("p1".to_owned(), "hi".to_owned())]
    );
    assert_eq!(
        f.world.borrow().steps.len(),
        1,
        "the refused invoke never reached the application"
    );
}

/// The same failed invoke repeated unchanged is told so.
#[test]
fn a_repeated_identical_failure_is_called_out() {
    let mut f = fixture(
        "append a paragraph",
        World::default(),
        vec![
            turn(&[invoke("append-paragraph", &serde_json::json!({}))], 64),
            turn(&[invoke("append-paragraph", &serde_json::json!({}))], 64),
            turn(&[finish("I could not.", false)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert!(
        !bodies[1]["messages"]
            .to_string()
            .contains("already failed the same way")
    );
    assert!(
        bodies[2]["messages"]
            .to_string()
            .contains("already failed the same way")
    );
}

/// Turns that only look things up are answered, but after a few each
/// answer says how many turns are left and asks the model to act.
#[test]
fn repeated_discovery_is_nudged_toward_acting() {
    let mut turns: Vec<Vec<TurnEvent>> = (0..=DISCOVERY_NUDGE)
        .map(|_| turn(&[(tools::LIST_OPERATIONS, serde_json::json!({}))], 64))
        .collect();
    turns.push(turn(&[finish("Nothing to do.", true)], 64));
    let mut f = fixture("look around", World::default(), turns);
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    let before = bodies[DISCOVERY_NUDGE]["messages"].to_string();
    assert!(!before.contains("only looking things up"), "{before}");
    let after = bodies[DISCOVERY_NUDGE + 1]["messages"].to_string();
    assert!(after.contains("only looking things up"), "{after}");
}

#[test]
fn parts_match_applications_by_id_or_name() {
    assert!(app_matches("so.tilo.chat", "so.tilo.chat"));
    assert!(app_matches("Chat", "so.tilo.chat"));
    assert!(app_matches("Writer", "so.tilo.word-processor"));
    assert!(app_matches("word processor", "so.tilo.word-processor"));
    assert!(!app_matches("so.tilo.notes", "so.tilo.chat"));
    assert!(!app_matches("Chat", "so.tilo.word-processor"));
}

/// A question about a selection is answered in the reply: it implies no
/// message and no operation in an application it names, while a request
/// to send something still does.
#[test]
fn a_question_implies_no_parts_and_a_request_to_send_still_does() {
    for question in [
        "explain this passage",
        "what does this paragraph in writer mean?",
        "translate this to german",
        "tell me what this chat message says",
        "summarize this for me",
    ] {
        assert!(asks_question(question), "{question}");
    }
    for request in [
        "rewrite this more politely",
        "post this in chat",
        "explain this and post it to chat",
        "summarise this into a note",
    ] {
        assert!(!asks_question(request), "{request}");
    }
    assert!(asks_for_message("tell melissa in chat"));
    assert!(asks_for_message("post the summary in chat"));
    assert!(!asks_for_message("tell me what this chat says"));
    assert!(asks_for_message("ping me in chat"));
}
