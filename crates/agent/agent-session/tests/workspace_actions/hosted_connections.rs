//! A person's assistant on the server uses a service its person connected, with no device of
//! theirs on while it works, and asks them before a command that changes something.
//!
//! The whole chain through the server's own interfaces, with the server, the person and the
//! assistant of the test this is a part of: the person connects a service from their own
//! device; a turn the server carries out finds the connection among its tools, lists it, reads
//! a command's row and calls a command that only reads; a command that changes something is
//! not called: the person is asked in their conversation with the assistant, with the
//! service, the command and its arguments as they will be sent; their own "Always" calls it
//! once and lets that command of that connection go ahead unasked from then on; they read that
//! permission and take it back, and the command asks again; and a question the assistant
//! reworded after it asked calls nothing and lets nothing stand, whatever its person answers.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use test_http::{Reply as Answer, Responder};
use tilo_actions::remote::{person_asking, person_connections};

use super::*;

/// What the agent is told when its person is asked first.
const ASKED: &str = "Your person is asked";
/// The call of a command through the connections' own action.
const CALL: &str = "connections/connections#call";

/// A service that speaks the Model Context Protocol over HTTP as far as a connection needs it:
/// it says what it is, lists one command that only reads and one that changes something, and
/// keeps every call it was sent.
struct Service {
    answering: Responder,
    called: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
}

impl Service {
    fn start() -> Self {
        let called = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&called);
        let answering = Responder::handle(move |request| {
            if request.method != "POST" || request.target != "/mcp" {
                return Some(Answer::status(404));
            }
            let asked: serde_json::Value =
                serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null);
            let result = match asked["method"].as_str().unwrap_or_default() {
                "initialize" => serde_json::json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "orchard", "title": "Orchard Tools", "version": "1.0"},
                }),
                "notifications/initialized" => return Some(Answer::status(202)),
                "tools/list" => serde_json::json!({"tools": [
                    {
                        "name": "find_trees",
                        "title": "Find trees",
                        "description": "Lists the trees of an orchard.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"orchard": {"type": "string"}},
                            "required": ["orchard"],
                        },
                        "annotations": {"readOnlyHint": true},
                    },
                    {
                        "name": "plant_tree",
                        "description": "Plants a tree.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"kind": {"type": "string"}},
                        },
                        "annotations": {"readOnlyHint": false},
                    },
                ]}),
                "tools/call" => {
                    let command = asked["params"]["name"].as_str().unwrap_or_default();
                    let arguments = asked["params"]["arguments"].clone();
                    let said = if command == "find_trees" {
                        format!("3 trees in {}", arguments["orchard"])
                    } else {
                        format!("planted: {}", arguments["kind"])
                    };
                    kept.lock().unwrap().push((command.to_owned(), arguments));
                    serde_json::json!({"content": [{"type": "text", "text": said}], "isError": false})
                }
                _ => return Some(Answer::status(400)),
            };
            let answer = serde_json::json!({"jsonrpc": "2.0", "id": asked["id"], "result": result});
            Some(
                Answer::status(200)
                    .header("Content-Type", "application/json")
                    .body(answer.to_string()),
            )
        });
        Self { answering, called }
    }

    fn address(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.answering.port())
    }

    /// The calls of `command` the service was sent, each by its arguments.
    fn calls_of(&self, command: &str) -> Vec<serde_json::Value> {
        self.called
            .lock()
            .unwrap()
            .iter()
            .filter(|(called, _)| called == command)
            .map(|(_, arguments)| arguments.clone())
            .collect()
    }
}

/// The person writes `words` to their assistant, the server answers that one turn with the
/// model's next `steps`, and what the model was shown of each tool call is answered, in order.
async fn turn_shows(hosted: &Hosted, words: &str, steps: Vec<Reply>) -> Vec<String> {
    let before = hosted.model.asked().len();
    hosted.turn(words, steps).await;
    // Each request after the turn's first follows a tool call and ends with its result.
    hosted
        .model
        .asked()
        .iter()
        .skip(before.saturating_add(1))
        .map(|call| format!("{:?}", call.request.messages.last()))
        .collect()
}

/// What the server's agents owe the workspace's people is looked at until the question `nth`
/// says how its answer ended: that outcome.
async fn ended(hosted: &Hosted, nth: usize) -> String {
    hosted.agents.look(&hosted.workspace).await.unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = hosted.questions(chat::question::ACTION).await;
        let outcome = now[nth].1.question.as_ref().unwrap().outcome.clone();
        if let Some(outcome) = outcome
            .as_ref()
            .filter(|outcome| outcome.outcome.0 != chat::question::STARTED)
        {
            return outcome.outcome.0.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the server never carried the answer out: {outcome:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// What the person reads of their connections from their own device.
async fn of_connections(
    hosted: &mut Hosted,
    action: &str,
    argument: serde_json::Value,
) -> serde_json::Value {
    let asked = (
        person_connections::PROVIDER,
        person_connections::RESOURCE,
        action,
    );
    hosted.on_device(asked, argument).await
}

/// The permissions that stand for the person, as they read them from their own device.
async fn permissions(hosted: &mut Hosted) -> Vec<serde_json::Value> {
    let asked = (
        person_asking::PROVIDER,
        person_asking::PERMISSIONS,
        person_asking::LIST,
    );
    let listed = hosted.on_device(asked, serde_json::Value::Null).await;
    listed["permissions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The model's step that plants a tree of `kind` through the connection `id`.
fn plants(call: &str, id: &str, kind: &str) -> Reply {
    does(
        call,
        CALL,
        &serde_json::json!({
            "connection": id, "command": "plant_tree", "arguments": {"kind": kind},
        }),
    )
}

/// The turn finds its person's connection among its tools, reads what it offers and calls the
/// command that only reads, with every device of its person off. Its listing names each command
/// of the connection, with whether it asks first.
async fn the_turn_lists_the_connection_and_calls_what_only_reads(
    hosted: &Hosted,
    service: &Service,
    id: &str,
) {
    let finding = serde_json::json!({
        "connection": id, "command": "find_trees", "arguments": {"orchard": "north"},
    });
    let steps = vec![
        does(
            "call-1",
            "connections/connections#list",
            &serde_json::json!({}),
        ),
        does("call-2", CALL, &finding),
        Reply::tool_call(
            "call-2b",
            "tilo_ls",
            serde_json::json!({"path": "connections/Orchard Tools#plant_tree"}),
        ),
        Reply::text("Three trees stand in the north orchard."),
    ];
    let shown = turn_shows(hosted, "How many trees stand in the north orchard?", steps).await;
    assert_eq!(shown.len(), 3, "three tool calls were answered: {shown:?}");
    assert!(
        shown[0].contains("Orchard Tools") && shown[0].contains("plant_tree"),
        "the turn reads its person's connection and what it offers: {}",
        shown[0]
    );
    assert!(
        shown[1].contains("3 trees in"),
        "the turn is answered what the service answered: {}",
        shown[1]
    );
    assert!(
        shown[2].contains("Asks your person first"),
        "its listing says which command of the connection asks: {}",
        shown[2]
    );
    assert_eq!(
        service.calls_of("find_trees"),
        [serde_json::json!({"orchard": "north"})],
        "the arguments went to the service as given"
    );
    let asked = hosted.questions(chat::question::ACTION).await;
    assert!(asked.is_empty(), "reading asks nobody");
}

/// A command that changes something is not called: its person is asked in their conversation
/// with the assistant and shown the service, the command and the arguments as they will be
/// sent. The question that asks.
async fn a_command_that_changes_something_asks_first(
    hosted: &Hosted,
    service: &Service,
    id: &str,
) -> Id {
    let steps = vec![
        plants("call-3", id, "apple"),
        Reply::text("I asked you to allow it. It waits for your answer."),
    ];
    let shown = turn_shows(hosted, "Plant an apple tree.", steps).await;
    assert!(
        shown[0].contains(ASKED),
        "the agent is told its person is asked: {}",
        shown[0]
    );
    assert!(
        service.calls_of("plant_tree").is_empty(),
        "nothing is called before the answer"
    );
    let asked = hosted.questions(chat::question::ACTION).await;
    assert_eq!(asked.len(), 1, "one question about the call");
    let (question, held) = &asked[0];
    assert_eq!(held.author, MemberId::agent(SCOUT));
    let asks = held.question.as_ref().unwrap();
    assert_eq!(
        asks.person,
        MemberId::person(hosted.person()),
        "only its own person is asked"
    );
    let offered: Vec<&str> = asks
        .answers
        .iter()
        .map(|answer| answer.id.as_str())
        .collect();
    assert_eq!(
        offered,
        [
            chat::question::ALLOW,
            chat::question::ALWAYS,
            chat::question::DENY
        ]
    );
    let detail = asks.kept().expect("the question keeps what a client draws");
    assert_eq!(detail["call"]["connection"]["id"], id);
    assert_eq!(detail["call"]["connection"]["address"], service.address());
    assert_eq!(detail["call"]["connection"]["name"], "Orchard Tools");
    assert_eq!(detail["call"]["command"], "plant_tree");
    assert_eq!(
        detail["call"]["arguments"],
        serde_json::json!({"kind": "apple"}),
        "the arguments are shown whole, as they will be sent"
    );
    assert_eq!(
        detail["always"]["action"]["provider"],
        person_connections::PROVIDER
    );
    assert!(
        held.text.contains("plant_tree") && held.text.contains("apple"),
        "the question says what would be called: {}",
        held.text
    );
    question.clone()
}

/// The person's own "Always" calls the command once, with the arguments they were shown, after
/// the turn that asked has ended; and nothing calls it a second time.
async fn always_calls_it_once(hosted: &Hosted, service: &Service, question: &Id) {
    hosted
        .answers(question, chat::question::ALWAYS, serde_json::Value::Null)
        .await;
    assert_eq!(ended(hosted, 0).await, chat::question::DONE);
    assert_eq!(
        service.calls_of("plant_tree"),
        [serde_json::json!({"kind": "apple"})],
        "called once, with what its person was shown"
    );
    hosted.agents.look(&hosted.workspace).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        service.calls_of("plant_tree").len(),
        1,
        "an answer is carried out once"
    );
}

/// After "Always" that command of that connection is called unasked, and each call is written
/// down with the leave it was made by.
async fn always_lets_it_go_ahead_unasked(hosted: &mut Hosted, service: &Service, id: &str) {
    let steps = vec![
        plants("call-4", id, "pear"),
        Reply::text("Planted a pear tree."),
    ];
    let shown = turn_shows(hosted, "Plant a pear tree too.", steps).await;
    assert!(
        shown[0].contains("planted") && !shown[0].contains(ASKED),
        "{}",
        shown[0]
    );
    assert_eq!(service.calls_of("plant_tree").len(), 2);
    let asked = hosted.questions(chat::question::ACTION).await;
    assert_eq!(asked.len(), 1, "no second question");

    let reading = serde_json::json!({"connection": id});
    let used = of_connections(hosted, person_connections::USES, reading).await;
    let leaves: Vec<_> = used["uses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|used| {
            (
                used["command"].clone(),
                used["leave"].clone(),
                used["by"].clone(),
            )
        })
        .collect();
    assert_eq!(
        leaves,
        [
            ("find_trees".into(), "read-only".into(), "Scout".into()),
            ("plant_tree".into(), "always".into(), "Scout".into()),
            ("plant_tree".into(), "always".into(), "Scout".into()),
        ],
        "each call is written down with the leave it was made by: {used}"
    );
}

/// The person reads the permission on their own device and takes it back, and the command asks
/// again; their no calls nothing.
async fn taken_back_it_asks_again(hosted: &mut Hosted, service: &Service, id: &str) {
    let permitted = permissions(hosted).await;
    assert_eq!(permitted.len(), 1, "{permitted:?}");
    let permission = &permitted[0];
    assert_eq!(permission["agent"]["id"], SCOUT);
    assert_eq!(
        permission["action"]["provider"],
        person_connections::PROVIDER
    );
    assert_eq!(
        permission["action"]["kind"],
        person_connections::COMMAND_KIND
    );
    assert_eq!(permission["action"]["action"], person_connections::CALL);
    assert_eq!(permission["on"]["document"], id);
    assert!(
        permission["on"]["resource"]
            .as_str()
            .is_some_and(|resource| resource.starts_with("commands/plant_tree@")),
        "the command by its name and its definition: {permission}"
    );
    assert_eq!(permission["usedTimes"], 1);
    let taking = (
        person_asking::PROVIDER,
        person_asking::PERMISSIONS,
        person_asking::TAKE_BACK,
    );
    hosted
        .on_device(taking, serde_json::json!({"permission": permission["id"]}))
        .await;

    let steps = vec![
        plants("call-5", id, "plum"),
        Reply::text("I asked you to allow it."),
    ];
    let shown = turn_shows(hosted, "And a plum tree.", steps).await;
    assert!(shown[0].contains(ASKED), "{}", shown[0]);
    assert_eq!(service.calls_of("plant_tree").len(), 2, "not called");
    let asked = hosted.questions(chat::question::ACTION).await;
    assert_eq!(asked.len(), 2, "it asks again");
    hosted
        .answers(&asked[1].0, chat::question::DENY, serde_json::Value::Null)
        .await;
    assert_eq!(ended(hosted, 1).await, chat::question::DROPPED);
    assert_eq!(
        service.calls_of("plant_tree").len(),
        2,
        "a no calls nothing"
    );
}

/// A question the assistant reworded after it asked is not what its person was asked: their
/// "Always" to it calls nothing and lets nothing stand.
async fn a_reworded_question_calls_nothing_and_lets_nothing_stand(
    hosted: &mut Hosted,
    service: &Service,
    id: &str,
) {
    let steps = vec![
        plants("call-6", id, "cherry"),
        Reply::text("I asked you to allow it."),
    ];
    let shown = turn_shows(hosted, "And a cherry tree.", steps).await;
    assert!(shown[0].contains(ASKED), "{}", shown[0]);
    let asked = hosted.questions(chat::question::ACTION).await;
    assert_eq!(asked.len(), 3, "it asks");
    let nth = asked
        .iter()
        .position(|(_, held)| held.text.contains("cherry"))
        .expect("the question about the cherry tree");
    let question = asked[nth].0.clone();
    super::reworded::the_assistant_rewords(hosted, &question, "May I look at your trees?").await;
    let reworded = hosted.questions(chat::question::ACTION).await;
    assert!(
        reworded[nth].1.edited && !reworded[nth].1.text.contains("cherry"),
        "the question shows other words now: {}",
        reworded[nth].1.text
    );

    hosted
        .answers(&question, chat::question::ALWAYS, serde_json::Value::Null)
        .await;
    assert_eq!(ended(hosted, nth).await, chat::question::FAILED);
    assert_eq!(
        service.calls_of("plant_tree").len(),
        2,
        "nothing is called for words its person never read"
    );
    let permitted = permissions(hosted).await;
    assert!(
        permitted.is_empty(),
        "and nothing stands for them either: {permitted:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_every_device_off_the_assistant_uses_a_connection_and_asks_before_it_changes_anything()
{
    let service = Service::start();
    let mut hosted = Hosted::start().await;
    // The stand-in service answers on this machine, which a server calls only where its operator
    // lets it.
    hosted.workspaces.call_private_model_addresses(true);
    let adding = serde_json::json!({"address": service.address()});
    let added = of_connections(&mut hosted, person_connections::ADD, adding).await;
    let id = added["id"].as_str().expect("a connection").to_owned();
    assert_eq!(added["name"], "Orchard Tools");

    the_turn_lists_the_connection_and_calls_what_only_reads(&hosted, &service, &id).await;
    let question = a_command_that_changes_something_asks_first(&hosted, &service, &id).await;
    always_calls_it_once(&hosted, &service, &question).await;
    always_lets_it_go_ahead_unasked(&mut hosted, &service, &id).await;
    taken_back_it_asks_again(&mut hosted, &service, &id).await;
    a_reworded_question_calls_nothing_and_lets_nothing_stand(&mut hosted, &service, &id).await;
    hosted.page.peer.close();
}
