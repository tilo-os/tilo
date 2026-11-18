//! How often a turn asks its model before it has done what its person asked, counted with a
//! model that knows only what it was told: the tools' own words, the turn's instruction and
//! what its tools answered. It writes a call once it was shown the action in the app, as a
//! listing writes one; until then it looks: for the app, then into the app, then into the first
//! thing of the kind it works on there. Every ask of a real model is many seconds for the
//! person who waits, so a look is what a turn is told at its start to save.
use super::*;

/// Where what a turn is told of its workspace begins, at the head of its person's message.
const TOLD: &str = "What you have here now";
/// The most the person's message grows by: what a turn is told of its workspace.
const MOST_GROWN: usize = 6_200;

/// One thing a person asks for, as a model goes about it.
#[derive(Clone)]
struct Wanted {
    /// What the person writes.
    words: &'static str,
    /// The app the work is in, as the listing of the apps names it.
    app: &'static str,
    /// The action as a listing writes it, which the model must have been shown before it can
    /// write the call; empty where the work is a read by a title the person gave.
    shown: &'static str,
    /// The kind of thing in the app that carries the action, as a listing of the app names
    /// it; empty where the app itself does.
    kind: &'static str,
    /// The call that does the work.
    work: (&'static str, serde_json::Value),
    /// The view the model then answers with, where the person asked to be shown.
    view: Option<serde_json::Value>,
}

/// A model that knows only what it was told.
struct Learner {
    wanted: Wanted,
    /// Whether it writes every call it can already write into one answer.
    at_once: bool,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

/// Everything `request` tells its model: its tools' own words, its instruction, its person's
/// words and what its tools answered.
fn told_so_far(request: &ModelRequest) -> String {
    let mut told: Vec<&str> = request
        .tools
        .iter()
        .map(|tool| tool.description.as_str())
        .collect();
    for content in request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
    {
        match content {
            Content::Text { text } => told.push(text),
            Content::ToolResult(result) => told.push(&result.content),
            _ => {}
        }
    }
    told.join("\n")
}

impl tilo_agent_session::model::PlanTurns for Learner {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        let step = {
            let mut asked = self.asked.lock().unwrap();
            asked.push(request.clone());
            asked.len()
        };
        let told = told_so_far(request);
        let called = |name: &str| {
            request
                .messages
                .iter()
                .flat_map(|message| message.content.iter())
                .filter(|content| matches!(content, Content::ToolCall(call) if call.name == name))
                .count()
        };
        let wanted = &self.wanted;
        let looked = |path: &str| {
            request
                .messages
                .iter()
                .flat_map(|message| message.content.iter())
                .any(|content| {
                    matches!(content, Content::ToolCall(call) if call.name == "tilo_ls"
                        && call.input.get("path").and_then(serde_json::Value::as_str) == Some(path))
                })
        };
        // The first thing of the kind in the app, by the name its line in a listing begins with.
        let thing = told
            .lines()
            .find(|line| !wanted.kind.is_empty() && line.contains(wanted.kind))
            .and_then(|line| line.trim().split("  ").next())
            .map(|name| format!("{}/{}", wanted.app, name.trim_end_matches('/')));
        let mut calls: Vec<(&str, serde_json::Value)> = Vec::new();
        if !told.contains(wanted.shown) {
            // It was not shown the action yet: it looks, for the app, then into the app, then
            // into the first thing of the kind there. Then it gives up.
            if !told.contains(wanted.app) {
                if called("tilo_ls") == 0 {
                    calls.push(("tilo_ls", serde_json::json!({})));
                }
            } else if !looked(wanted.app) {
                let into = serde_json::json!({"path": wanted.app, "limit": 200});
                calls.push(("tilo_ls", into));
            } else if let Some(thing) = thing.filter(|thing| !looked(thing)) {
                let into = serde_json::json!({"path": thing, "limit": 200});
                calls.push(("tilo_ls", into));
            }
        } else if called(wanted.work.0) == 0 {
            calls.push(wanted.work.clone());
            if self.at_once
                && let Some(view) = &wanted.view
            {
                calls.push(("answer_with_view", view.clone()));
            }
        } else if let Some(view) = &wanted.view
            && called("answer_with_view") == 0
        {
            calls.push(("answer_with_view", view.clone()));
        }
        if calls.is_empty() {
            let words = Content::Text {
                text: "It is done.".into(),
            };
            return Ok(answer(vec![words], StopReason::EndTurn, 1));
        }
        let content = calls
            .into_iter()
            .enumerate()
            .map(|(at, (name, input))| {
                Content::ToolCall(ToolCall {
                    id: format!("call-{step}-{at}"),
                    name: name.into(),
                    input,
                    extra_content: None,
                })
            })
            .collect();
        Ok(answer(content, StopReason::ToolCalls, 1))
    }

    fn model(&self) -> &'static str {
        "learner"
    }
}

/// A model that goes about `wanted` knowing only what it is told. Every request it was sent is
/// kept in `asked`.
fn learning(wanted: &Wanted, at_once: bool, asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    let (wanted, asked) = (wanted.clone(), Arc::clone(asked));
    Arc::new(move || {
        Some(Box::new(Learner {
            wanted: wanted.clone(),
            at_once,
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// What a person asks for here: a document written and shown, a task added, a note read.
fn the_three_asks() -> [Wanted; 3] {
    let poem = "view poem() {\n    text(word_processor.document.title)\n}";
    [
        Wanted {
            words: "Write a line about plums in the word processor and show it to me.",
            app: "word-processor",
            shown: "#append-paragraph(",
            kind: "word-processor.document",
            work: (
                "tilo_do",
                serde_json::json!({
                    "path": "word-processor#append-paragraph",
                    "args": {"text": "Plums in the cold dish."},
                }),
            ),
            // The app listed as its id names it, where the view writes it its own way.
            view: Some(serde_json::json!({"source": poem, "reads": ["word-processor"]})),
        },
        Wanted {
            words: "Add a task to buy plums and tell me when it is there.",
            app: "tasks",
            shown: "#create(",
            kind: "",
            work: (
                "tilo_do",
                serde_json::json!({"path": "tasks#create", "args": {"title": "Buy plums"}}),
            ),
            view: None,
        },
        Wanted {
            words: "Read my note Plums and tell me what it says.",
            app: "notes",
            shown: "",
            kind: "",
            work: ("tilo_read", serde_json::json!({"path": "notes/Plums"})),
            view: None,
        },
    ]
}

/// A turn that is told at its start what its workspace has asks its model only for the work
/// and the answer: two asks for a task added, two for a note read, three for a document written
/// and shown, and two for that where the model writes both calls into one answer. Nothing it
/// calls is refused, and what its instruction says a server has not is true of this one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_told_what_it_has_asks_its_model_only_for_the_work() {
    let mut served = Served::start(1).await;
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    // Tasks and Notes in use beside the word processor, and a note to read: through the tools a
    // turn of her agent's has.
    let her_agent = on_a_turn(
        &Served::person(&served.ada, &served.devices.1, &workspace),
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &said),
        Vec::new(),
    );
    for (path, arguments) in [
        ("tasks#open", serde_json::json!({})),
        ("notes#open", serde_json::json!({})),
        ("notes#new-note", serde_json::json!({"title": "Plums"})),
    ] {
        let call = serde_json::json!({"path": path, "args": arguments});
        let (text, failed) = served.tool(&workspace, &her_agent, "tilo_do", call).await;
        assert!(!failed, "{path}: {text}");
    }

    let mut counted: Vec<usize> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    let mut instructions: Vec<String> = Vec::new();
    let mut told = String::new();
    for wanted in the_three_asks() {
        for at_once in [false, true] {
            let asked = Arc::new(Mutex::new(Vec::new()));
            // Each ask in words of its own: a message is found by what it says.
            let words = format!("{} (ask {})", wanted.words, counted.len());
            let message = write_to_agent(
                &served.actions,
                &served.workspaces,
                &workspace,
                (&served.ada, &served.devices.1),
                &words,
            )
            .await;
            let model = learning(&wanted, at_once, &asked);
            Box::pin(answered(&served, &workspace, &message, model)).await;
            let asked = asked.lock().unwrap();
            counted.push(asked.len());
            let answers: Vec<_> = asked
                .last()
                .into_iter()
                .flat_map(|request| request.messages.iter())
                .flat_map(|message| message.content.iter())
                .filter_map(|content| match content {
                    Content::ToolResult(result) => Some(result),
                    _ => None,
                })
                .collect();
            let called: Vec<String> = asked
                .last()
                .into_iter()
                .flat_map(|request| request.messages.iter())
                .flat_map(|message| message.content.iter())
                .filter_map(|content| match content {
                    Content::ToolCall(call) => Some(format!("{} {}", call.name, call.input)),
                    _ => None,
                })
                .map(|call| call.chars().take(110).collect())
                .collect();
            seen.push(format!(
                "{words}: {} asks, {} tool answers: {called:?}",
                asked.len(),
                answers.len()
            ));
            refused.extend(
                answers
                    .iter()
                    .filter(|result| result.is_error)
                    .map(|result| format!("{words}: {}", result.content)),
            );
            // The turn's instruction, and what stands at the head of its person's message.
            let first: Vec<&str> = asked
                .first()
                .into_iter()
                .flat_map(|request| request.messages.iter().take(2))
                .filter_map(|message| match message.content.first() {
                    Some(Content::Text { text }) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            instructions.extend(first.first().map(|text| (*text).to_owned()));
            if let Some(head) = first.get(1) {
                told = (*head).to_owned();
            }
        }
    }
    assert_eq!(
        counted,
        [3, 2, 2, 2, 2, 2],
        "asks of the model for each of the three asks, first one call an answer and then all \
         it can write at once:\n{}\nrefused: {refused:#?}\ntold at its start, {} bytes:\n{told}",
        seen.join("\n"),
        told.len()
    );
    assert!(refused.is_empty(), "{refused:#?}");
    // What the turn was told of its workspace stands at the head of its person's message,
    // within its bound.
    assert!(
        told.contains(TOLD) && told.len() <= MOST_GROWN,
        "{} bytes told: {told}",
        told.len()
    );
    assert!(
        told.contains("word-processor") && told.contains("#append-paragraph("),
        "{told}"
    );
    // The instruction is what a provider keeps and what a conversation's key is made of: it
    // is the same byte for byte in all six turns, though a task was added between them.
    assert_eq!(instructions.len(), 6, "{instructions:#?}");
    assert!(
        instructions.iter().all(|one| *one == instructions[0]),
        "the instruction changed between two turns of one conversation: {instructions:#?}"
    );

    // And what it says a server has not is true of this one.
    assert!(
        instructions[0].contains("no program is carried out here"),
        "{}",
        instructions[0]
    );
    // And every turn is told that a secret never passes through the conversation.
    assert!(
        instructions[0].contains("never asked for in words or typed into the chat"),
        "{}",
        instructions[0]
    );
    let program = serde_json::json!({"program": "notes.notebook"});
    let (text, failed) = served.tool(&workspace, &her_agent, "tilo", program).await;
    assert!(failed && text.contains("no programs"), "{text}");
    let sealed = format!("{}:so.tilo.chat:", tilo_actions::remote::RESOURCE_PROVIDER);
    let read = serde_json::json!({ "path": sealed });
    let (text, failed) = served.tool(&workspace, &her_agent, "tilo_read", read).await;
    assert!(
        failed && text.contains(tilo_actions::remote::RESOURCE_READS_UNAVAILABLE),
        "{text}"
    );
    page.peer.close();
}

/// What a tempted model says of its person's messages without having read them.
const GUESS: &str = "You have no messages yet.";

/// A model that is tempted: the read it tries is refused, and it then answers from what it was
/// told. Told to state nothing it did not read, it says what it tried and what was refused;
/// told nothing of the kind, it states a guess about its person's things as a fact.
struct Tempted {
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl tilo_agent_session::model::PlanTurns for Tempted {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        let refused = request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .find_map(|content| match content {
                Content::ToolResult(result) if result.is_error => Some(result.content.clone()),
                _ => None,
            });
        let Some(refused) = refused else {
            let sealed = format!("{}:so.tilo.chat:", tilo_actions::remote::RESOURCE_PROVIDER);
            let read = Content::ToolCall(ToolCall {
                id: "call-0".into(),
                name: "tilo_read".into(),
                input: serde_json::json!({ "path": sealed }),
                extra_content: None,
            });
            return Ok(answer(vec![read], StopReason::ToolCalls, 1));
        };
        let text = if told_so_far(request).contains("never state it as a fact") {
            format!("I tried to read your messages and was refused: {refused}")
        } else {
            GUESS.to_owned()
        };
        Ok(answer(vec![Content::Text { text }], StopReason::EndTurn, 1))
    }

    fn model(&self) -> &'static str {
        "tempted"
    }
}

/// A turn never states what it did not read. A model whose read is refused, and that would
/// state a guess about its person's things as a fact, is told by its turn to say what it tried
/// and what was refused: the reply its person reads holds the refusal's own words and no guess.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_whose_read_is_refused_says_so_and_states_no_guess() {
    let mut served = Served::start(1).await;
    let (workspace, page, _) = served.workspace(|_| Vec::new()).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let message = write_to_agent(
        &served.actions,
        &served.workspaces,
        &workspace,
        (&served.ada, &served.devices.1),
        "What did I write to you last week?",
    )
    .await;
    let model: Model = {
        let asked = Arc::clone(&asked);
        Arc::new(move || {
            Some(Box::new(Tempted {
                asked: Arc::clone(&asked),
            })
                as Box<dyn tilo_agent_session::model::PlanTurns>)
        })
    };
    Box::pin(answered(&served, &workspace, &message, model)).await;
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "the read that is refused, then the words"
    );
    let deadline = tilo_test_env::hang_deadline();
    let reply = loop {
        let part = conversation_part(&served.workspaces, &workspace).await;
        let found = part
            .messages
            .values()
            .map(|held| held.text.clone())
            .find(|text| text.contains("was refused") || text.contains(GUESS));
        if let Some(reply) = found {
            break reply;
        }
        assert!(Instant::now() < deadline, "the agent's reply never came");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        reply.contains(tilo_actions::remote::RESOURCE_READS_UNAVAILABLE) && !reply.contains(GUESS),
        "{reply}"
    );
    page.peer.close();
}

/// What a turn is told names only what its caller may use. An agent held to reading is told
/// the apps and no action that changes anything in them; what it is told is what its own
/// listing answers it; and a write it tries all the same is refused as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_held_to_reading_is_told_no_action_that_changes_anything() {
    let mut served = Served::start(1).await;
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let conversation = conversation_with(&served.ada, SCOUT);
    let free = on_a_turn(&ada, SCOUT, (&conversation, &said), Vec::new());
    let reading_only = vec![tilo_action_policy::Rule::new(
        tilo_action_policy::TreePath::workspace(&workspace).rest(),
        tilo_action_policy::KindSet::UNDOABLE.union(tilo_action_policy::KindSet::OUTSIDE),
        tilo_action_policy::Answer::Deny,
    )];
    let held = on_a_turn(&ada, SCOUT, (&conversation, &said), reading_only);

    let mut told: Vec<String> = Vec::new();
    for caller in [&free, &held] {
        let tools = tilo_mcp::McpServer::for_turn(service!(served, &workspace), caller.clone())
            .expect("a turn has its tools");
        told.push(tools.told_at_start(6_000).await);
    }
    assert!(
        told[0].contains("word-processor") && told[0].contains("#append-paragraph("),
        "an agent nothing holds is told the action that writes: {}",
        told[0]
    );
    assert!(
        told[1].contains("word-processor"),
        "held to reading, it is still told the app it may read: {}",
        told[1]
    );
    for changes in [
        "#append-paragraph",
        "#replace-body",
        "#insert-table",
        "#add-comment",
    ] {
        assert!(
            !told[1].contains(changes),
            "held to reading, it is told {changes}: {}",
            told[1]
        );
    }
    // What it is told of the app is what its own listing of the app answers it.
    let whole = serde_json::json!({"path": "word-processor", "kind": "actions", "limit": 200});
    let (listed, failed) = served.tool(&workspace, &held, "tilo_ls", whole).await;
    assert!(!failed, "{listed}");
    let of_the_app = told[1]
        .lines()
        .find(|line| line.starts_with("word-processor"))
        .unwrap_or_default();
    for action in of_the_app
        .split('#')
        .skip(1)
        .filter_map(|rest| rest.split(['(', ',', ' ']).next())
    {
        assert!(
            listed.contains(&format!("#{action}")),
            "{action} is told and not listed: {listed}"
        );
    }
    // And a write it tries all the same is refused as before.
    let add = serde_json::json!({
        "path": "word-processor#append-paragraph",
        "args": {"text": "Sloe for the warden."},
    });
    let (text, failed) = served.tool(&workspace, &held, "tilo_do", add).await;
    assert!(failed, "held to reading, its write was carried out: {text}");
    page.peer.close();
}
