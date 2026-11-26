//! A workspace's connections as a turn its server carries out itself reaches
//! them: with no device of the turn's person on it lists them and calls a
//! command that only reads; a command that changes something asks that
//! person and is called only on their own answer; their "always" covers one
//! command of one connection as its service defined it, until they take it
//! back; a connection is proposed and added only by their yes; and no
//! caller that is not the turn's own, no narrowed agent without a rule that
//! names the connection, and no token reaches any of it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, and the server handle a test binds is kept until the test ends"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use collab_types::WorkspaceId;
use serde_json::{Map, Value, json};
use tilo_action_policy::{
    ActionKind, Answer, Grant, GrantId, Grantee, KindSet, Level, Rule, TreePath,
};
use tilo_actions::remote::person_connections::{
    self, ADD, Asks, BEGIN_SIGN_IN, CALL, FINISH_SIGN_IN, LIST, LOOK, Leave, PROPOSE, REFRESH,
    REMOVE, Taken, USES,
};
use tilo_actions::remote::{
    Caller, CatalogueRow, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_user_identity::DeviceCert;

use super::super::turn_use_tests::on_turn;
use super::super::vault_tests::ask_at;
use super::{Refused, Served, admit, caller, governed, governor, start_serving};
use crate::connections::DEFINED_ANEW;
use crate::connections::stand_in::StandIn;
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::HostedTurn;
use crate::workspaces::person_asking::kept_asks::{CARRIED_OUT, KeptAsk, NewAsk};
use crate::workspaces::person_asking::policy_node;

const SCOUT: &str = "scout";

fn request(target: RemoteActionTarget, argument: Value) -> InvokeRequest {
    let argument = match argument {
        Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// `action` of a person's connections, with `argument`.
fn of(action: &str, argument: Value) -> InvokeRequest {
    request(person_connections::target(action), argument)
}

/// The call of `command` of the connection `id` with `arguments`.
fn calling(id: &str, command: &str, arguments: Value) -> InvokeRequest {
    let mut call = Map::new();
    call.insert("connection".to_owned(), json!(id));
    call.insert("command".to_owned(), json!(command));
    call.insert("arguments".to_owned(), arguments);
    of(CALL, Value::Object(call))
}

/// What a turn's request was answered, where it was carried out.
fn done(taken: Result<Taken, Refused>) -> Value {
    match taken {
        Ok(Taken::Done(value)) => value,
        other => panic!("it is carried out: {other:?}"),
    }
}

/// What a turn's request asks its person, where it asks first.
fn asked(taken: Result<Taken, Refused>) -> Asks {
    match taken {
        Ok(Taken::Asks(asks)) => *asks,
        other => panic!("the person is asked first: {other:?}"),
    }
}

fn refused_with(refused: &Result<Taken, Refused>, why: &str) -> bool {
    matches!(refused, Err(Refused::Refused { reason }) if reason.contains(why))
}

/// The calls of commands among what a service saw, each by its command and
/// its arguments.
fn calls(seen: &[Value]) -> Vec<(String, Value)> {
    seen.iter()
        .filter(|asked| asked["method"] == "tools/call")
        .map(|asked| {
            (
                asked["command"].as_str().unwrap_or_default().to_owned(),
                asked["arguments"].clone(),
            )
        })
        .collect()
}

/// The actions the rows `rows` offer, each by its resource and its name.
fn offered(rows: &[CatalogueRow]) -> Vec<(String, String)> {
    rows.iter()
        .filter_map(|row| row.action.as_ref())
        .map(|action| {
            (
                action.address.resource.clone(),
                action.address.action.clone(),
            )
        })
        .collect()
}

/// Every file under `dir` whose bytes hold `needle`.
fn files_holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let needle = needle.as_bytes();
    let mut holding = Vec::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => waiting.push(path),
                Ok(metadata) if metadata.is_file() => {
                    let bytes = std::fs::read(&path).unwrap_or_default();
                    if bytes.windows(needle.len()).any(|window| window == needle) {
                        holding.push(path);
                    }
                }
                _ => {}
            }
        }
    }
    holding
}

/// The lines a subscriber wrote.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

/// Who a team is, kept while its server is stopped and started again.
struct Members {
    workspace: String,
    workspace_id: WorkspaceId,
    ada: DeviceCert,
    bo: DeviceCert,
}

/// One workspace on a server that calls services on its own machine, with
/// two members: Ada, whose agent Scout the server keeps working, and Bo.
struct Team<'a> {
    workspaces: &'a Workspaces,
    workspace: String,
    workspace_id: WorkspaceId,
    actions: Served,
    ada: DeviceCert,
    bo: DeviceCert,
    as_ada: Caller,
    /// Ada, by her person id.
    ada_id: String,
    /// The caller a turn of Scout's for Ada acts as.
    scout: Caller,
}

impl<'a> Team<'a> {
    async fn on(workspaces: &'a Workspaces, owner: &DeviceCert) -> Self {
        workspaces.call_private_model_addresses(true);
        let (workspace, workspace_id, actions) = governed(workspaces).await;
        let ada = admit(workspaces, &workspace, owner, 1).await;
        let bo = admit(workspaces, &workspace, owner, 2).await;
        Self {
            as_ada: caller(&ada),
            ada_id: hex::encode(ada.user_public_key),
            scout: on_turn(workspaces, &ada, &workspace, SCOUT),
            workspaces,
            workspace,
            workspace_id,
            actions,
            ada,
            bo,
        }
    }

    /// Who the team is, for the server that is started again on what this
    /// one kept.
    fn members(&self) -> Members {
        Members {
            workspace: self.workspace.clone(),
            workspace_id: self.workspace_id,
            ada: self.ada.clone(),
            bo: self.bo.clone(),
        }
    }

    /// The team `members` on `workspaces`, a server started again on what
    /// the one before it kept: no workspace is made and nobody is admitted.
    fn back(workspaces: &'a Workspaces, members: Members) -> Self {
        workspaces.call_private_model_addresses(true);
        let actions = Served::new();
        actions.attach(workspaces.clone());
        Self {
            as_ada: caller(&members.ada),
            ada_id: hex::encode(members.ada.user_public_key),
            scout: on_turn(workspaces, &members.ada, &members.workspace, SCOUT),
            workspaces,
            workspace: members.workspace,
            workspace_id: members.workspace_id,
            actions,
            ada: members.ada,
            bo: members.bo,
        }
    }

    /// A turn of Scout's for Ada, with nothing that narrows the agent.
    fn turn(&self) -> HostedTurn<'_> {
        self.turn_of(&self.scout)
    }

    /// The turn `caller` acts as, with nothing that narrows its agent.
    const fn turn_of<'t>(&self, caller: &'t Caller) -> HostedTurn<'t> {
        HostedTurn {
            workspace: self.workspace_id,
            caller,
            bound: None,
        }
    }

    /// Scout's turn for Ada as an agent that `bound` narrows.
    fn narrowed<'t>(&'t self, caller: &'t Caller, bound: &'t [Rule]) -> HostedTurn<'t> {
        HostedTurn {
            bound: Some(bound),
            ..self.turn_of(caller)
        }
    }

    async fn takes(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Taken, Refused> {
        self.workspaces.connections_for_turn(turn, request).await
    }

    /// The actions `turn` finds of its person's connections in its listing.
    async fn finds(&self, turn: &HostedTurn<'_>) -> Vec<(String, String)> {
        offered(&self.workspaces.connection_rows_for_turn(turn).await)
    }

    /// What a member is answered by the workspace's providers, asked from a
    /// device of theirs through its action service.
    async fn hers(&self, target: RemoteActionTarget, argument: Value) -> Value {
        let her = (&self.ada, &self.as_ada);
        ask_at(&self.actions, self.workspace_id, her, target, argument)
            .await
            .expect("Ada is answered on her own device")
    }

    /// The connections the workspace keeps, as a member reads them on a
    /// device of theirs.
    async fn her_connections(&self) -> Value {
        let listed = self.hers(person_connections::target(LIST), json!({})).await;
        listed["connections"].clone()
    }

    /// What Ada reads the connection `id` was used for.
    async fn uses(&self, id: &str) -> Value {
        let used = self
            .hers(person_connections::target(USES), json!({"connection": id}))
            .await;
        used["uses"].clone()
    }

    /// Ada connects `service` from her own device: the connection's id.
    async fn connects(&self, service: &StandIn) -> String {
        let added = self
            .hers(
                person_connections::target(ADD),
                json!({"address": service.address()}),
            )
            .await;
        service.seen();
        added["id"].as_str().expect("an id").to_owned()
    }

    /// The ask the server keeps while the question about `asks` waits, kept
    /// as the code that puts the question keeps it.
    fn keeps(&self, asks: &Asks) -> KeptAsk {
        let new = NewAsk {
            workspace: self.workspace.clone(),
            agent: SCOUT.to_owned(),
            name: "Scout".to_owned(),
            carrier: person_connections::CARRIER.to_owned(),
            kept: asks.kept.clone(),
            always: asks.always.clone(),
            words: asks.named.clone(),
            asked_in: Value::Null,
        };
        self.workspaces
            .keep_ask(&asks.person, new)
            .unwrap()
            .expect("nothing the same waits already")
    }

    /// Ada's answer to the question `waits` is kept for, carried out for
    /// `turn`.
    async fn answered(
        &self,
        turn: &HostedTurn<'_>,
        waits: &KeptAsk,
        leave: Option<Leave>,
    ) -> Result<(), String> {
        self.workspaces
            .connection_answered(turn, waits, leave)
            .await
            .map(drop)
    }
}

/// The turn lists the workspace's connection and finds in its listing what
/// an agent does with connections and each command of the connection, with
/// whether it asks first. Adding is no row.
async fn the_turn_reads_what_the_workspace_connected(team: &Team<'_>, service: &StandIn, id: &str) {
    let turn = team.turn();
    let listed = done(team.takes(&turn, &of(LIST, json!({}))).await);
    assert_eq!(listed["connections"][0]["id"], id);
    assert_eq!(listed["connections"][0]["name"], "Orchard Tools");
    let commands = listed["connections"][0]["commands"].as_array();
    assert_eq!(commands.map(Vec::len), Some(3));

    let rows = team.workspaces.connection_rows_for_turn(&turn).await;
    let its = person_connections::connection_resource(id);
    let names = offered(&rows);
    for action in [LIST, LOOK, REFRESH, CALL, PROPOSE] {
        let row = (person_connections::RESOURCE.to_owned(), action.to_owned());
        assert!(names.contains(&row), "{action}: {names:?}");
    }
    let her_own = [ADD, REMOVE, USES, BEGIN_SIGN_IN, FINISH_SIGN_IN];
    assert!(
        names
            .iter()
            .all(|(_, action)| !her_own.contains(&action.as_str())),
        "{names:?}"
    );
    let row_of = |command: &str| {
        rows.iter()
            .filter_map(|row| row.action.as_ref())
            .find(|action| action.address.resource == its && action.address.action == command)
            .unwrap_or_else(|| panic!("no row for {command}: {names:?}"))
    };
    let (finds, plants) = (row_of("find_trees"), row_of("plant_tree"));
    assert_eq!(finds.resource_label, "Orchard Tools");
    assert_eq!(finds.action_label, "Find trees");
    let says = finds.description.as_deref().unwrap();
    assert!(
        says.starts_with("Only reads.") && says.contains(&service.site()),
        "{says}"
    );
    assert!(
        says.ends_with("The service says of it: Lists the trees of an orchard."),
        "{says}"
    );
    assert_eq!(finds.params.len(), 1);
    assert!(finds.params[0].name == "orchard" && finds.params[0].required);
    let says = plants.description.as_deref().unwrap();
    assert!(says.starts_with("Asks your person first."), "{says}");
}

/// The turn calls a command that only reads, by the connections' own
/// action and by the command's own row, and the arguments go to the service
/// as given. What only the person does is not a turn's.
async fn the_turn_calls_what_only_reads(team: &Team<'_>, service: &StandIn, id: &str) {
    let turn = team.turn();
    let finding = calling(id, "find_trees", json!({"orchard": "north"}));
    let found = done(team.takes(&turn, &finding).await);
    assert_eq!(found["structured"]["trees"], 3);
    let by_its_row = RemoteActionTarget {
        provider: person_connections::PROVIDER.to_owned(),
        address: ActionAddress::new(person_connections::connection_resource(id), "count_apples"),
    };
    let counted = done(team.takes(&turn, &request(by_its_row, json!({}))).await);
    assert_eq!(counted["isError"], false);
    assert_eq!(
        calls(&service.seen()),
        [
            ("find_trees".to_owned(), json!({"orchard": "north"})),
            ("count_apples".to_owned(), json!({})),
        ]
    );

    for (action, argument) in [
        (ADD, json!({"address": "https://elsewhere.example/mcp"})),
        (REMOVE, json!({"connection": id})),
        (USES, json!({"connection": id})),
        (
            BEGIN_SIGN_IN,
            json!({"connection": id, "returnTo": "https://app.example/signed-in"}),
        ),
        (
            FINISH_SIGN_IN,
            json!({"connection": id, "returned": "https://app.example/signed-in?code=c&state=s"}),
        ),
    ] {
        let refused = team.takes(&turn, &of(action, argument)).await;
        assert_eq!(refused, Err(Refused::UnknownAction), "{action}");
    }
    assert_eq!(
        team.her_connections().await.as_array().map(Vec::len),
        Some(1)
    );
}

/// Another member's agent, acting for them, reaches the connections the
/// workspace keeps as this one does: they are the workspace's, not Ada's.
/// The agent Ada asks reaches them when Bo asks it too, and a caller that is
/// no turn is told nothing.
async fn another_members_turn_reaches_what_the_workspace_connected(
    team: &Team<'_>,
    service: &StandIn,
    id: &str,
) {
    let its = person_connections::connection_resource(id);
    let finding = calling(id, "find_trees", json!({"orchard": "north"}));
    let bos_agent = on_turn(team.workspaces, &team.bo, &team.workspace, "bos-agent");
    let for_bo = team.turn_of(&bos_agent);
    let theirs = done(team.takes(&for_bo, &of(LIST, json!({}))).await);
    assert_eq!(theirs["connections"][0]["id"], id, "{theirs}");
    let found = done(team.takes(&for_bo, &finding).await);
    assert_eq!(found["structured"]["trees"], 3);
    let theirs = team.finds(&for_bo).await;
    assert!(
        theirs.iter().any(|(resource, _)| *resource == its),
        "{theirs:?}"
    );

    let bo_asks_scout = on_turn(team.workspaces, &team.bo, &team.workspace, SCOUT);
    let for_bo = team.turn_of(&bo_asks_scout);
    let theirs = done(team.takes(&for_bo, &of(LIST, json!({}))).await);
    assert_eq!(theirs["connections"][0]["id"], id, "{theirs}");

    // The person's own device is no turn.
    let strange = team.turn_of(&team.as_ada);
    for asked in [
        of(LIST, json!({})),
        finding.clone(),
        of(PROPOSE, json!({"address": service.address()})),
    ] {
        let refused = team.takes(&strange, &asked).await;
        assert_eq!(refused, Err(Refused::UnknownAction));
    }
    assert!(team.finds(&strange).await.is_empty());
    assert!(
        !calls(&service.seen()).is_empty(),
        "the workspace's connection was called for its other member's agent"
    );
}

/// With no device of its person's on, a turn their server carries out lists
/// the workspace's connections, finds each command in its listing with
/// whether it asks first, and calls a command that only reads. What only a
/// person does is not a turn's, another member's turn reaches the same
/// connections, and no caller that is not the turn's own is answered.
#[tokio::test]
async fn a_turn_with_no_device_reads_the_workspaces_connections_and_no_other_caller_does() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let service = StandIn::start("plain");
    let id = team.connects(&service).await;
    the_turn_reads_what_the_workspace_connected(&team, &service, &id).await;
    the_turn_calls_what_only_reads(&team, &service, &id).await;
    another_members_turn_reaches_what_the_workspace_connected(&team, &service, &id).await;
    started.shutdown().await;
}

/// A call of a command that changes something calls nothing and answers
/// what its person is asked: the service by the site of its address, the
/// command and its arguments whole. What a question could not show whole
/// is refused before anybody is asked. What is asked.
async fn it_asks_and_calls_nothing(team: &Team<'_>, service: &StandIn, id: &str) -> Asks {
    let turn = team.turn();
    let planting = calling(id, "plant_tree", json!({"kind": "apple", "row": 3}));
    let asks = asked(team.takes(&turn, &planting).await);
    assert!(
        calls(&service.seen()).is_empty(),
        "nothing is called before the answer"
    );
    assert_eq!(asks.person, team.ada.user_public_key);
    assert!(
        asks.words.contains("plant_tree")
            && asks.words.contains(&service.site())
            && asks.words.contains(r#"{"kind":"apple","row":3}"#),
        "{}",
        asks.words
    );
    assert_eq!(
        asks.kept["shown"]["call"],
        json!({
            "connection": {"id": id, "address": service.address(), "name": "Orchard Tools"},
            "command": "plant_tree",
            "arguments": {"kind": "apple", "row": 3},
        })
    );
    assert_eq!(asks.kept["words"], asks.words.as_str());
    let always = asks.always.clone().expect("always is offered");
    assert_eq!(always.agent, SCOUT);
    assert_eq!(always.workspace, team.workspace);
    let on = always.on.expect("one command of one connection");
    assert_eq!(on.document, id);
    assert!(on.resource.starts_with("commands/plant_tree@"), "{on:?}");
    assert_eq!(asks.answers, None);
    // Asked the same again, it asks the same: one question stands.
    let again = asked(team.takes(&turn, &planting).await);
    assert_eq!(again.kept, asks.kept);

    let large = calling(id, "plant_tree", json!({"kind": "a".repeat(3_000)}));
    let refused = team.takes(&turn, &large).await;
    assert!(
        refused_with(&refused, "larger than a question shows whole"),
        "{refused:?}"
    );
    asks
}

/// An answer is carried out for the agent that asked, acting for the person
/// asked, and for nobody else: not for another member's ask of the same
/// agent, or another agent of the person's; and
/// what was kept for another carrier is no connection's to carry out.
async fn an_answer_for_anybody_else_does_nothing(team: &Team<'_>, waits: &KeptAsk) {
    let bos_ask = on_turn(team.workspaces, &team.bo, &team.workspace, SCOUT);
    let another = on_turn(team.workspaces, &team.ada, &team.workspace, "another-agent");
    for (who, stranger) in [
        ("another member's ask of the same agent", &bos_ask),
        ("another agent of the person's", &another),
    ] {
        let strange = team.turn_of(stranger);
        let carried = team.answered(&strange, waits, Some(Leave::Once)).await;
        assert!(carried.is_err(), "{who}: {carried:?}");
    }
    let other_carrier = KeptAsk {
        carrier: "mail-send".to_owned(),
        ..waits.clone()
    };
    let carried = team
        .answered(&team.turn(), &other_carrier, Some(Leave::Once))
        .await;
    assert!(carried.is_err(), "{carried:?}");
}

/// A command that changes something is not called: its person is asked. A
/// no calls nothing. An answer is carried out only for the agent that asked
/// acting for the person asked, and then the command is called with what
/// they were shown, once: the next call asks again.
#[tokio::test]
async fn a_command_that_changes_something_asks_and_only_its_persons_answer_calls_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let service = StandIn::start("plain");
    let id = team.connects(&service).await;
    let turn = team.turn();

    let asks = it_asks_and_calls_nothing(&team, &service, &id).await;
    let waits = team.keeps(&asks);
    an_answer_for_anybody_else_does_nothing(&team, &waits).await;
    team.answered(&turn, &waits, None)
        .await
        .expect("a no is carried out by doing nothing");
    assert!(calls(&service.seen()).is_empty(), "nothing was called");

    let _chat_only = super::super::turn_use_tests::on_turn_for_apps(
        team.workspaces,
        &team.ada,
        &team.workspace,
        SCOUT,
        &["so.tilo.chat"],
    );
    let denied = team.answered(&turn, &waits, Some(Leave::Always)).await;
    assert!(
        denied.is_err(),
        "the pending call lost its Connections grant"
    );
    assert!(calls(&service.seen()).is_empty());
    let _restored = on_turn(team.workspaces, &team.ada, &team.workspace, SCOUT);

    // Their yes calls it with what they were shown, by the leave they gave.
    team.answered(&turn, &waits, Some(Leave::Once))
        .await
        .expect("the person's yes calls the command");
    assert_eq!(
        calls(&service.seen()),
        [("plant_tree".to_owned(), json!({"kind": "apple", "row": 3}))]
    );
    let used = team.uses(&id).await;
    assert_eq!(used.as_array().map(Vec::len), Some(1), "{used}");
    assert_eq!(used[0]["command"], "plant_tree");
    assert_eq!(used[0]["by"], "Scout");
    assert_eq!(used[0]["leave"], "once");
    assert_eq!(used[0]["ended"], "ok");
    // Once is once: the next call asks again.
    let planting = calling(&id, "plant_tree", json!({"kind": "apple", "row": 3}));
    asked(team.takes(&turn, &planting).await);
    assert!(calls(&service.seen()).is_empty());
    started.shutdown().await;
}

/// What waits when the server stops: one question nobody answered, and one
/// whose answer a reader claimed and had done nothing about yet.
async fn two_questions_wait(team: &Team<'_>, id: &str) -> (KeptAsk, KeptAsk) {
    let turn = team.turn();
    let apple = calling(id, "plant_tree", json!({"kind": "apple", "row": 3}));
    let waits = team.keeps(&asked(team.takes(&turn, &apple).await));
    let pear = calling(id, "plant_tree", json!({"kind": "pear", "row": 4}));
    let begun = team.keeps(&asked(team.takes(&turn, &pear).await));
    assert_ne!(waits.ask, begun.ask, "each is a question of its own");
    let claimed = team
        .workspaces
        .claim_ask(&begun.ask)
        .expect("the first reader of its answer");
    assert!(claimed.started);
    (waits, begun)
}

/// A question about a command outlives its server's restart. What was asked
/// is read back whole, and its person's yes after the restart calls the
/// command with what they were shown, by one reader of their answer: a
/// second reader does nothing. A question whose answer was being carried
/// out when the server stopped is carried out by nobody afterwards.
#[tokio::test]
async fn a_question_about_a_command_outlives_a_restart_and_its_answer_is_carried_out_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let service = StandIn::start("plain");
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let id = team.connects(&service).await;
    let (waits, begun) = two_questions_wait(&team, &id).await;
    let members = team.members();
    assert!(
        calls(&service.seen()).is_empty(),
        "nothing is called before an answer"
    );
    started.shutdown().await;

    let started = start_serving(dir.path(), &key, &owner).await;
    let workspaces = started.workspaces();
    let team = Team::back(workspaces, members);
    let kept = workspaces.open_asks(&team.workspace);
    assert_eq!(kept.len(), 2, "{kept:?}");
    let still = kept.iter().find(|ask| ask.ask == waits.ask).unwrap();
    assert!(!still.started && !still.interrupted, "{still:?}");
    assert_eq!(still.kept, waits.kept, "what was asked is read back whole");
    assert_eq!(still.always, waits.always);
    assert_eq!(still.person, hex::encode(team.ada.user_public_key));

    // Her yes, read after the restart: one reader claims it and carries it
    // out, and the command is called with what she was shown.
    let turn = team.turn();
    let claimed = workspaces
        .claim_ask(&waits.ask)
        .expect("the first reader of her answer");
    team.answered(&turn, &claimed, Some(Leave::Once))
        .await
        .expect("her yes calls the command after the restart");
    assert_eq!(
        calls(&service.seen()),
        [("plant_tree".to_owned(), json!({"kind": "apple", "row": 3}))]
    );
    assert_eq!(
        workspaces.claim_ask(&waits.ask),
        Err(CARRIED_OUT.to_owned()),
        "a second reader of the same answer does nothing"
    );
    let used = team.uses(&id).await;
    assert_eq!(used.as_array().map(Vec::len), Some(1), "{used}");
    assert_eq!(used[0]["command"], "plant_tree");
    assert_eq!(used[0]["leave"], "once");

    // The one whose answer was being carried out when the server stopped.
    let was_begun = kept.iter().find(|ask| ask.ask == begun.ask).unwrap();
    assert!(was_begun.started && was_begun.interrupted, "{was_begun:?}");
    assert_eq!(
        workspaces.claim_ask(&begun.ask),
        Err(CARRIED_OUT.to_owned()),
        "it is not carried out a second time"
    );
    assert!(calls(&service.seen()).is_empty(), "nothing more is called");
    started.shutdown().await;
}

/// After her "always" that command of that connection is called unasked,
/// and Ada's exact approval is kept in workspace governance.
async fn always_lets_it_go_ahead_unasked(team: &Team<'_>, service: &StandIn, id: &str) -> GrantId {
    let turn = team.turn();
    let planting = calling(id, "plant_tree", json!({"kind": "apple"}));
    let asks = asked(team.takes(&turn, &planting).await);
    let waits = team.keeps(&asks);
    team.answered(&turn, &waits, Some(Leave::Always))
        .await
        .expect("the person's always calls the command");
    team.workspaces.forget_ask(&waits);

    let planting = calling(id, "plant_tree", json!({"kind": "pear"}));
    let planted = done(team.takes(&turn, &planting).await);
    assert_eq!(planted["isError"], false);
    assert_eq!(
        calls(&service.seen()),
        [
            ("plant_tree".to_owned(), json!({"kind": "apple"})),
            ("plant_tree".to_owned(), json!({"kind": "pear"})),
        ]
    );
    let scope = asks.always.expect("the command offers lasting approval");
    let grant = team
        .workspaces
        .workspace_grants(&team.workspace)
        .await
        .unwrap()
        .into_iter()
        .find(|grant| {
            grant.node == policy_node(&scope)
                && grant.grantee == Grantee::agent(SCOUT)
                && grant.granted_by == Grantee::person(team.ada_id.clone())
        })
        .expect("the workspace keeps the precise approval");
    assert_eq!(grant.answer, Answer::Allow);
    assert_eq!(grant.kinds, KindSet::OUTSIDE);
    let used = team.uses(id).await;
    assert_eq!(used.as_array().map(Vec::len), Some(2), "{used}");
    assert!(used[0]["leave"] == "always" && used[1]["leave"] == "always");
    grant.id
}

/// Their "always" lets one command of one connection go ahead unasked for
/// the agent that asked. It covers no other connection and no other agent
/// of theirs, and once they take it back the command asks again.
#[tokio::test]
async fn always_covers_one_command_of_one_connection_until_its_person_takes_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let (service, another) = (StandIn::start("plain"), StandIn::start("plain"));
    let id = team.connects(&service).await;
    let other = team.connects(&another).await;
    let permission = always_lets_it_go_ahead_unasked(&team, &service, &id).await;

    let members = team.members();
    started.shutdown().await;
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::back(started.workspaces(), members);
    let turn = team.turn();
    let planting = calling(&id, "plant_tree", json!({"kind": "peach"}));
    let planted = done(team.takes(&turn, &planting).await);
    assert_eq!(planted["isError"], false, "the approval survives restart");
    assert_eq!(calls(&service.seen()).len(), 1);

    // It covers nothing wider: the same command of another connection, and
    // another agent of the same person, ask.
    let elsewhere = calling(&other, "plant_tree", json!({"kind": "plum"}));
    asked(team.takes(&turn, &elsewhere).await);
    let second = on_turn(team.workspaces, &team.ada, &team.workspace, "another-agent");
    let for_second = team.turn_of(&second);
    let planting = calling(&id, "plant_tree", json!({"kind": "plum"}));
    asked(team.takes(&for_second, &planting).await);

    let accepted = team
        .workspaces
        .workspace_grants(&team.workspace)
        .await
        .unwrap()
        .into_iter()
        .find(|grant| grant.id == permission)
        .unwrap();
    let blocked = Grant::new(
        "no more planting",
        accepted.grantee,
        accepted.node,
        KindSet::OUTSIDE,
        Answer::Deny,
        Grantee::person(hex::encode(owner.user_public_key)),
    );
    team.workspaces
        .put_workspace_grants(&team.workspace, &owner, [0x53; 32], vec![blocked.clone()])
        .await
        .unwrap();
    assert!(refused_with(
        &team.takes(&turn, &planting).await,
        "workspace policy denies this action for the agent",
    ));
    assert!(
        calls(&service.seen()).is_empty(),
        "the deny takes precedence"
    );
    team.workspaces
        .remove_workspace_grants(&team.workspace, &owner, [0x54; 32], vec![blocked.id])
        .await
        .unwrap();

    // Taken back, it asks again from the very next call.
    team.workspaces
        .remove_workspace_grants(&team.workspace, &owner, [0x51; 32], vec![permission])
        .await
        .expect("the workspace's grant was revoked");
    asked(team.takes(&turn, &planting).await);
    assert!(calls(&service.seen()).is_empty());
    assert!(calls(&another.seen()).is_empty());
    started.shutdown().await;
}

/// What its person allowed is the command as they were shown it. A service
/// that defines the command otherwise before the answer is carried out is
/// not called, also after "always": the call is written down as refused,
/// the connection says the command changed, and the next call asks again,
/// about the command as it is defined now. A command the service adds and
/// marks as only reading asks too.
#[tokio::test]
async fn a_command_its_service_defines_anew_asks_again_also_after_always() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    // This service lists `plant_tree` as changing something the first
    // time, and from then on as only reading, beside a command it adds.
    let service = StandIn::start("lying");
    let id = team.connects(&service).await;
    let turn = team.turn();

    let planting = calling(&id, "plant_tree", json!({"kind": "apple"}));
    let asks = asked(team.takes(&turn, &planting).await);
    let waits = team.keeps(&asks);
    let carried = team.answered(&turn, &waits, Some(Leave::Always)).await;
    assert_eq!(carried, Err(DEFINED_ANEW.to_owned()));
    team.workspaces.forget_ask(&waits);
    assert!(
        calls(&service.seen()).is_empty(),
        "a command defined otherwise than its person was shown is not called"
    );
    let used = team.uses(&id).await;
    assert_eq!(used[0]["ended"], "refused", "{used}");
    assert_eq!(used[0]["why"], DEFINED_ANEW, "{used}");

    // The connection says the command changed, and it asks again: what
    // stands for the definition its person saw does not cover this one.
    let listed = done(team.takes(&turn, &of(LIST, json!({}))).await);
    let commands = listed["connections"][0]["commands"].as_array().unwrap();
    let plant = commands
        .iter()
        .find(|command| command["name"] == "plant_tree")
        .expect("the command is still listed");
    assert_eq!(plant["readOnly"], true, "the service now marks it so");
    assert_eq!(plant["changed"], true);
    assert_eq!(plant["asks"], true);
    let again = asked(team.takes(&turn, &planting).await);
    assert_ne!(
        again.always, asks.always,
        "another definition, another thing"
    );
    // A command the service adds and marks as only reading asks as well.
    asked(
        team.takes(&turn, &calling(&id, "fell_all", json!({})))
            .await,
    );
    assert!(calls(&service.seen()).is_empty());
    started.shutdown().await;
}

/// The rule that names the connection `id` for calling its commands.
fn naming(id: &str, answer: Answer) -> Rule {
    tilo_automation::action_rule(
        person_connections::PROVIDER,
        person_connections::CONNECTION_KIND,
        Some(&person_connections::connection_resource(id)),
        CALL,
        KindSet::of(ActionKind::Outside),
        answer,
    )
}

/// Rules that name no connection, another one, or this one only to deny
/// it: each with why it lets a narrowed agent reach nothing of `id`.
fn rules_that_do_not_reach(id: &str, other: &str) -> Vec<(&'static str, Vec<Rule>)> {
    let outside = KindSet::of(ActionKind::Outside);
    let provider = TreePath::root()
        .any(Level::Workspace)
        .app(person_connections::PROVIDER);
    let this_connection = provider
        .clone()
        .kind(person_connections::CONNECTION_KIND)
        .resource(person_connections::connection_resource(id));
    let every_connection = tilo_automation::action_rule(
        person_connections::PROVIDER,
        person_connections::CONNECTION_KIND,
        None,
        CALL,
        outside,
        Answer::Allow,
    );
    vec![
        ("no rule at all", Vec::new()),
        (
            "a rule for the whole provider",
            vec![Rule::new(provider, outside, Answer::Allow)],
        ),
        ("a rule for every connection", vec![every_connection]),
        (
            "a rule that names another connection",
            vec![naming(other, Answer::Allow)],
        ),
        (
            "a rule that names it and one that denies calling it",
            vec![
                Rule::new(this_connection, outside, Answer::Allow),
                naming(id, Answer::Deny),
            ],
        ),
    ]
}

/// An agent that anything narrows (what its entry bounds it to, or a
/// sub-agent) reaches a connection only where a rule of its own names it:
/// not without one, not by a rule for the whole provider or for every
/// connection, not by one that names another connection, and not where a
/// rule denies it. A rule is no leave of its person's: with one, a command
/// that changes something still asks.
#[tokio::test]
async fn a_narrowed_agent_reaches_only_a_connection_a_rule_of_its_own_names() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let (service, another) = (StandIn::start("plain"), StandIn::start("plain"));
    let id = team.connects(&service).await;
    let other = team.connects(&another).await;
    let sub = on_turn(team.workspaces, &team.ada, &team.workspace, "sub-agent");
    let its = person_connections::connection_resource(&id);
    let finding = calling(&id, "find_trees", json!({"orchard": "north"}));

    for (why, bound) in rules_that_do_not_reach(&id, &other) {
        let turn = team.narrowed(&sub, &bound);
        let refused = team.takes(&turn, &finding).await;
        assert_eq!(refused, Err(Refused::NotPermitted), "{why}");
        let refreshing = of(REFRESH, json!({"connection": id}));
        let refused = team.takes(&turn, &refreshing).await;
        assert_eq!(refused, Err(Refused::NotPermitted), "{why}");
        let listed = done(team.takes(&turn, &of(LIST, json!({}))).await);
        let listed = listed["connections"].as_array().unwrap();
        assert!(
            listed.iter().all(|listed| listed["id"] != id.as_str()),
            "{why}: it does not read what it would be refused"
        );
        let rows = team.finds(&turn).await;
        assert!(
            rows.iter().all(|(resource, _)| *resource != its),
            "{why}: {rows:?}"
        );
    }
    assert!(calls(&service.seen()).is_empty(), "nothing was called");
    with_a_rule_that_names_it_it_reads_and_still_asks(&team, &sub, (&id, &other), &service).await;
    assert!(calls(&another.seen()).is_empty());
    started.shutdown().await;
}

/// With a rule that names the connection, a narrowed agent lists that one
/// connection and calls what only reads; what changes something still asks,
/// and nothing else of the provider is its to take.
async fn with_a_rule_that_names_it_it_reads_and_still_asks(
    team: &Team<'_>,
    sub: &Caller,
    (id, other): (&str, &str),
    service: &StandIn,
) {
    let bound = vec![naming(id, Answer::Allow)];
    let turn = team.narrowed(sub, &bound);
    let listed = done(team.takes(&turn, &of(LIST, json!({}))).await);
    let listed = listed["connections"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "the one connection its rule names");
    assert_eq!(listed[0]["id"], id);
    let finding = calling(id, "find_trees", json!({"orchard": "north"}));
    let found = done(team.takes(&turn, &finding).await);
    assert_eq!(found["isError"], false);
    let planting = calling(id, "plant_tree", json!({"kind": "apple"}));
    asked(team.takes(&turn, &planting).await);
    assert_eq!(
        calls(&service.seen()).len(),
        1,
        "a rule of its own is no leave of its person's: what changes something still asks"
    );
    for (action, argument) in [
        (REFRESH, json!({"connection": other})),
        (LOOK, json!({"address": service.address()})),
        (PROPOSE, json!({"address": "https://elsewhere.example/mcp"})),
    ] {
        let refused = team.takes(&turn, &of(action, argument)).await;
        assert_eq!(refused, Err(Refused::NotPermitted), "{action}");
    }
    let rows = team.finds(&turn).await;
    assert!(
        rows.iter()
            .all(|(_, action)| action != LOOK && action != PROPOSE),
        "{rows:?}"
    );
    assert!(calls(&service.seen()).is_empty());
}

/// An agent's proposal adds nothing: its person is asked, shown the
/// service by the site of its address and what a card of it needs, and the
/// agent is answered that it asked. What is asked.
async fn a_proposal_asks_and_adds_nothing(team: &Team<'_>, service: &StandIn) -> Asks {
    let proposing = of(PROPOSE, json!({"address": service.address()}));
    let asks = asked(team.takes(&team.turn(), &proposing).await);
    assert_eq!(asks.person, team.ada.user_public_key);
    assert_eq!(asks.always, None, "a connection is never added unasked");
    let card = &asks.kept["shown"]["connection"];
    assert_eq!(
        *card,
        json!({
            "address": service.address(), "name": "Orchard Tools", "needsSignIn": false,
            "commands": 3,
        })
    );
    assert!(
        asks.words.contains(&service.site())
            && asks.words.contains("3 commands")
            && !asks.words.contains("Orchard"),
        "the system's own words, and the service by the site of its address: {}",
        asks.words
    );
    let told = asks.answers.clone().expect("the agent is answered");
    assert_eq!(told["connection"], *card);
    let says = told["told"].as_str().unwrap_or_default();
    assert!(
        says.contains("Nothing is connected before they say yes"),
        "{told}"
    );
    assert_eq!(
        team.her_connections().await,
        json!([]),
        "an agent's call alone adds nothing"
    );
    asks
}

/// An agent proposes a connection from an address, and nothing is added by
/// its call. Their no adds nothing, an answer carried out for anybody else
/// adds nothing, and their own yes adds the connection as theirs. A service
/// that wants a sign-in says so on its card, and an address inside the
/// server's own network is not looked at unless its operator lets it.
#[tokio::test]
async fn an_agent_proposes_a_connection_and_only_its_persons_yes_adds_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let (service, signing) = (StandIn::start("plain"), StandIn::start("signing-in"));
    let turn = team.turn();
    let asks = a_proposal_asks_and_adds_nothing(&team, &service).await;

    let waits = team.keeps(&asks);
    team.answered(&turn, &waits, None)
        .await
        .expect("a no is carried out by doing nothing");
    let bos_agent = on_turn(team.workspaces, &team.bo, &team.workspace, SCOUT);
    let strange = team.turn_of(&bos_agent);
    let carried = team.answered(&strange, &waits, Some(Leave::Once)).await;
    assert!(carried.is_err(), "{carried:?}");
    assert_eq!(team.her_connections().await, json!([]));

    team.answered(&turn, &waits, Some(Leave::Once))
        .await
        .expect("the person's yes adds the connection");
    let hers = team.her_connections().await;
    assert_eq!(hers.as_array().map(Vec::len), Some(1));
    assert_eq!(
        hers[0]["address"], asks.kept["shown"]["connection"]["address"],
        "a client finds the connection it was asked about by its address"
    );
    assert_eq!(hers[0]["state"], "ready");
    let proposing = of(PROPOSE, json!({"address": service.address()}));
    let refused = team.takes(&turn, &proposing).await;
    assert!(refused_with(&refused, "connected already"), "{refused:?}");

    // A service that wants a sign-in says so on its card, with where its
    // person signs in: signing in stays their own act.
    let proposing = of(PROPOSE, json!({"address": signing.address()}));
    let asks = asked(team.takes(&turn, &proposing).await);
    let card = &asks.kept["shown"]["connection"];
    assert_eq!(card["needsSignIn"], true, "{card}");
    assert_eq!(card["signInAt"], signing.issuer_site().as_str(), "{card}");
    assert!(asks.words.contains("sign in"), "{}", asks.words);

    // An address inside the server's own network is not looked at unless
    // its operator lets it.
    team.workspaces.call_private_model_addresses(false);
    signing.seen();
    let refused = team.takes(&turn, &proposing).await;
    assert!(
        matches!(&refused, Err(Refused::Refused { .. })),
        "{refused:?}"
    );
    assert!(signing.seen().is_empty(), "the service was asked nothing");
    started.shutdown().await;
}

/// Ada connects `service`, which wants a sign-in, and signs in on her own
/// device by the service's own flow: the connection's id.
async fn she_signs_in(team: &Team<'_>, service: &StandIn) -> String {
    let added = team
        .hers(
            person_connections::target(ADD),
            json!({"address": service.address()}),
        )
        .await;
    let id = added["id"].as_str().expect("an id").to_owned();
    assert_eq!(added["state"], "needs-sign-in");
    let step = team
        .hers(
            person_connections::target(BEGIN_SIGN_IN),
            json!({"connection": id, "returnTo": "https://app.example/signed-in"}),
        )
        .await;
    let returned = service.browse(step["open"].as_str().expect("where her browser goes"));
    let signed = team
        .hers(
            person_connections::target(FINISH_SIGN_IN),
            json!({"connection": id, "returned": returned}),
        )
        .await;
    assert_eq!(signed["state"], "ready");
    assert_eq!(signed["signIn"], "signed-in");
    service.seen();
    id
}

/// Everything a turn, its person and whoever puts the question read of the
/// connection `id` while the turn lists it, reads its rows, calls what only
/// reads, and is allowed a command that changes something.
async fn what_a_turn_and_its_person_read(team: &Team<'_>, id: &str) -> Vec<String> {
    let turn = team.turn();
    let mut read = Vec::new();
    let listed = done(team.takes(&turn, &of(LIST, json!({}))).await);
    read.push(listed.to_string());
    let rows = team.workspaces.connection_rows_for_turn(&turn).await;
    read.push(format!("{rows:?}"));
    let finding = calling(id, "find_trees", json!({"orchard": "north"}));
    let found = done(team.takes(&turn, &finding).await);
    assert_eq!(found["structured"]["trees"], 3);
    read.push(found.to_string());
    let planting = calling(id, "plant_tree", json!({"kind": "apple"}));
    let asks = asked(team.takes(&turn, &planting).await);
    read.push(format!("{asks:?}"));
    let waits = team.keeps(&asks);
    read.push(serde_json::to_string(&waits).unwrap());
    team.answered(&turn, &waits, Some(Leave::Once))
        .await
        .expect("the person's yes calls the command");
    read
}

/// A connection its person signed in to is used by a turn with the sign-in
/// the vault keeps, and nothing the service's issuer handed out is anywhere
/// a model or a reader looks: in no answer to the turn, no row of its
/// listing, nothing its person is asked or the server keeps for the
/// question, no line that was logged, and no file of the server in the
/// clear.
#[tokio::test]
async fn no_token_of_a_connection_is_anywhere_a_turn_a_question_or_a_reader_looks() {
    let lines = Lines::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(lines.clone())
        .finish();
    let _logged = tracing::subscriber::set_default(subscriber);
    tracing::info!("the log is being read");

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let service = StandIn::start("signing-in");
    let id = she_signs_in(&team, &service).await;
    let read = what_a_turn_and_its_person_read(&team, &id).await;

    // Both calls reached the service with the sign-in it took.
    let seen = service.seen();
    let called: Vec<&Value> = seen
        .iter()
        .filter(|asked| asked["method"] == "tools/call")
        .collect();
    assert_eq!(called.len(), 2, "{seen:?}");
    assert!(
        called
            .iter()
            .all(|asked| asked["signedIn"] == true && asked["taken"] == true),
        "{called:?}"
    );

    let logged = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("the log is being read"));
    for secret in ["orchard-access", "orchard-renewal", "orchard-code"] {
        assert!(
            read.iter().all(|read| !read.contains(secret)),
            "what a turn or its person reads holds what a sign-in handed out"
        );
        assert!(
            files_holding(dir.path(), secret).is_empty(),
            "a file of the server holds what a sign-in handed out"
        );
        assert!(
            !logged.contains(secret),
            "a log line holds what a sign-in handed out"
        );
    }
    started.shutdown().await;
}
