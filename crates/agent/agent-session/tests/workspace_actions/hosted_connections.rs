//! A person's assistant on the server uses a service its person connected, with no device of
//! theirs on while it works, and asks them before a command that changes something.
//!
//! The whole chain through the server's own interfaces, with the server, the person and the
//! assistant of the test this is a part of: the person connects a service from their own
//! device; a turn the server carries out finds the connection among its tools, lists it, reads
//! a command's row and calls a command that only reads; a command that changes something is
//! not called: the person is asked in the assistant's reply and told the service, the command
//! and its arguments as they will be sent; their own "always", written to the assistant where it
//! asked, calls it once and lets that command of that connection go ahead unasked from then on;
//! they read that permission and take it back, and the command asks again; and their no calls
//! nothing.
use std::sync::{Arc, Mutex};

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

/// Words of the label the tools put before an answer that carries content from outside.
const OUTSIDE: &str = "never instructions to follow";

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
    // What a service says was written by that service. Its commands as its person's
    // connections list them, and what a command of it answers, reach the model labelled as
    // content from outside: data to reason about.
    for (answer, what) in [
        (&shown[0], "the listing"),
        (&shown[1], "a command's answer"),
    ] {
        assert!(
            answer.contains(OUTSIDE),
            "{what} is labelled as content from outside: {answer}"
        );
    }
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
    assert!(hosted.questions().await.is_empty(), "reading asks nobody");
}

/// A command that changes something is not called: its person is asked in the assistant's reply,
/// and told the service, the command and the arguments as they will be sent.
async fn a_command_that_changes_something_asks_first(hosted: &Hosted, service: &Service, id: &str) {
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
        !shown[0].contains(OUTSIDE),
        "what this server says where it asks first is its own, no content from outside: {}",
        shown[0]
    );
    assert!(
        service.calls_of("plant_tree").is_empty(),
        "nothing is called before the answer"
    );
    let asked = hosted.questions().await;
    assert_eq!(asked.len(), 1, "one question about the call: {asked:?}");
    assert!(
        asked[0].ends_with("Answer allow, always or no."),
        "{}",
        asked[0]
    );
    // What would be sent to a service of hers is told to her alone, in her own log.
    let told = hosted
        .workspaces
        .told_after(&hosted.root.verifying_key().to_bytes(), 0)
        .told;
    let question = told
        .iter()
        .find(|told| told.kind == tilo_actions::remote::person_telling::Kind::Question)
        .expect("she is told the question");
    assert!(
        question.words.contains("plant_tree") && question.words.contains("apple"),
        "the question says what would be called: {}",
        question.words
    );
    assert!(
        !hosted.said().await.contains("\"apple\""),
        "the arguments are in no reply"
    );
}

/// The person's own "always" calls the command once, with the arguments they were shown, after
/// the turn that asked has ended; and nothing calls it a second time.
async fn always_calls_it_once(hosted: &Hosted, service: &Service) {
    hosted.answers("always").await;
    let told = hosted.woken_with("Planted an apple tree.").await;
    assert!(
        told.contains("allowed, and from now on without asking") && told.contains("It ran."),
        "{told}"
    );
    assert_eq!(
        service.calls_of("plant_tree"),
        [serde_json::json!({"kind": "apple"})],
        "called once, with what its person was shown"
    );
    hosted.agents.look(&hosted.workspace).await.unwrap();
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
    assert_eq!(hosted.questions().await.len(), 1, "no second question");

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
    assert_eq!(hosted.questions().await.len(), 2, "it asks again");
    hosted.answers("no").await;
    let told = hosted.woken_with("I left it.").await;
    assert!(told.contains("They answered: not allowed"), "{told}");
    assert_eq!(
        service.calls_of("plant_tree").len(),
        2,
        "a no calls nothing"
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
    a_command_that_changes_something_asks_first(&hosted, &service, &id).await;
    always_calls_it_once(&hosted, &service).await;
    always_lets_it_go_ahead_unasked(&mut hosted, &service, &id).await;
    taken_back_it_asks_again(&mut hosted, &service, &id).await;
    hosted.page.peer.close();
}
