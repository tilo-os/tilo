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

use crate::workspaces::actions::host::turn_use_tests::narrowing;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use collab_types::WorkspaceId;
use serde_json::{Map, Value, json};
use tilo_action_policy::{ActionKind, Answer, Grant, GrantId, Grantee, KindSet, Rule, TreePath};
use tilo_actions::remote::asked::{AskedFacts, asked_person};
use tilo_actions::remote::person_connections::{
    self, ADD, BEGIN_SIGN_IN, CALL, FINISH_SIGN_IN, LIST, LOOK, PROPOSE, REFRESH, REMOVE, USES,
};
use tilo_actions::remote::{
    Caller, CatalogueRow, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_user_identity::DeviceCert;

use super::super::turn_use_tests::{approved, asks, dispatched, fact, narrowed_elsewhere, on_turn};
use super::super::vault_tests::ask_at;
use super::{Refused, Served, admit, caller, hosted, hosting_device, start_serving};
use crate::connections::DEFINED_ANEW;
use crate::connections::stand_in::StandIn;
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::{TestGroupKeys, test_host_holding};
use crate::workspaces::hosted_use::HostedTurn;

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
fn done(taken: Result<Value, Refused>) -> Value {
    match taken {
        Ok(value) => value,
        other => panic!("it is carried out: {other:?}"),
    }
}

/// What a turn's request shows its person, where it waits for their yes.
fn asked(taken: Result<Value, Refused>) -> AskedFacts {
    asks(taken)
}

fn refused_with(refused: &Result<Value, Refused>, why: &str) -> bool {
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
        let (workspace, workspace_id, actions) = hosted(workspaces).await;
        let ada = admit(workspaces, &workspace, owner, 1).await;
        let bo = admit(workspaces, &workspace, owner, 2).await;
        Self {
            as_ada: caller(&ada),
            ada_id: hex::encode(ada.user_public_key),
            scout: on_turn(&ada, &workspace, SCOUT),
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
        let actions = super::super::super::tests::host_dispatch(workspaces);
        Self {
            as_ada: caller(&members.ada),
            ada_id: hex::encode(members.ada.user_public_key),
            scout: on_turn(&members.ada, &members.workspace, SCOUT),
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
            policy: None,
        }
    }

    /// Scout's turn for Ada, with `policy` narrowing its agent.
    fn narrowed<'t>(
        &'t self,
        caller: &'t Caller,
        policy: &'t tilo_action_policy::WorkspacePolicy,
    ) -> HostedTurn<'t> {
        HostedTurn {
            policy: Some(policy),
            ..self.turn_of(caller)
        }
    }

    async fn takes(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, Refused> {
        dispatched(self.workspaces, turn, request).await
    }

    /// `request` of `turn` again, as its person's approve admits it.
    async fn approve(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, Refused> {
        let approved = approved(request, &self.workspace_id.to_string());
        dispatched(self.workspaces, turn, &approved).await
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

    /// The grant her "always" gives Scout for calling `command` of the connection `id` as its
    /// service defines it now, written in the workspace by its owner `owner`.
    async fn always(&self, owner: &DeviceCert, id: &str, command: &str) -> GrantId {
        let root = self.workspaces.workspace_root(&self.workspace);
        let digest =
            crate::connections::callable(&root, &crate::connections::Reach::of(true), id, command)
                .unwrap()
                .digest;
        let grant = Grant::new(
            format!("test:always:{SCOUT}:{id}:{command}"),
            Grantee::agent(SCOUT, self.ada_id.clone()),
            command_node(&self.workspace, id, (command, &digest)),
            KindSet::OUTSIDE,
            Answer::Allow,
            Grantee::person(self.ada_id.clone()),
        );
        let id = grant.id.clone();
        self.workspaces
            .put_workspace_grants(&self.workspace, owner, rand::random(), vec![grant])
            .await
            .unwrap();
        id
    }
}

/// Where a grant that lets an agent call the command `name` of `connection` in `workspace`, as
/// its service defines it (`digest`), stands in the workspace's policy.
fn command_node(workspace: &str, connection: &str, (name, digest): (&str, &str)) -> TreePath {
    TreePath::workspace(workspace)
        .app(person_connections::PROVIDER)
        .kind(person_connections::COMMAND_KIND)
        .resource(format!(
            "{connection}/{}",
            person_connections::command_resource(name, digest)
        ))
        .action(CALL)
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
    let bos_agent = on_turn(&team.bo, &team.workspace, "bos-agent");
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

    let bo_asks_scout = on_turn(&team.bo, &team.workspace, SCOUT);
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
    let (key, owner) = hosting_device();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let service = StandIn::start("plain");
    let id = team.connects(&service).await;
    the_turn_reads_what_the_workspace_connected(&team, &service, &id).await;
    the_turn_calls_what_only_reads(&team, &service, &id).await;
    another_members_turn_reaches_what_the_workspace_connected(&team, &service, &id).await;
    started.shutdown().await;
}

/// A call of a command that changes something calls nothing and waits for its person: they are
/// shown the service by the site of its address, the command and its arguments whole. What a
/// question could not show whole is refused before anybody is asked. What they are shown.
async fn it_asks_and_calls_nothing(team: &Team<'_>, service: &StandIn, id: &str) -> AskedFacts {
    let turn = team.turn();
    let planting = calling(id, "plant_tree", json!({"kind": "apple", "row": 3}));
    let asks = asked(team.takes(&turn, &planting).await);
    assert!(
        calls(&service.seen()).is_empty(),
        "nothing is called before the answer"
    );
    assert_eq!(asked_person(turn.caller), Some(team.ada_id.clone()));
    let on = asks.on.clone().unwrap_or_default();
    assert!(
        on.contains("plant_tree") && on.contains(&service.site()),
        "{on}"
    );
    assert_eq!(fact(&asks, "Arguments"), r#"{"kind":"apple","row":3}"#);
    assert_eq!(
        asks.plural.as_deref(),
        Some("commands"),
        "always is offered"
    );
    // Asked the same again, it asks the same.
    let again = asked(team.takes(&turn, &planting).await);
    assert_eq!(again, asks);

    let large = calling(id, "plant_tree", json!({"kind": "a".repeat(3_000)}));
    let refused = team.takes(&turn, &large).await;
    assert!(
        refused_with(&refused, "larger than a question shows whole"),
        "{refused:?}"
    );
    asks
}

/// A command that changes something is not called: it waits for its person. Unapproved it
/// calls nothing; their approve calls it with what they were shown, once: the next call asks
/// again.
#[tokio::test]
async fn a_command_that_changes_something_asks_and_only_its_persons_answer_calls_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let service = StandIn::start("plain");
    let id = team.connects(&service).await;
    let turn = team.turn();

    it_asks_and_calls_nothing(&team, &service, &id).await;
    assert!(calls(&service.seen()).is_empty(), "nothing was called");

    // Their yes calls it with what they were shown, once.
    let planting = calling(&id, "plant_tree", json!({"kind": "apple", "row": 3}));
    team.approve(&turn, &planting)
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

/// A question about a command outlives its server's restart: its person's approve after the
/// restart calls the command with what they were shown.
#[tokio::test]
async fn a_question_about_a_command_outlives_a_restart_and_its_answer_is_carried_out_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    // The device keeps its channel keys across the restart, as a device's key service does.
    let keys = Arc::new(TestGroupKeys::default());
    let service = StandIn::start("plain");
    let started = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let id = team.connects(&service).await;
    let apple = calling(&id, "plant_tree", json!({"kind": "apple", "row": 3}));
    asked(team.takes(&team.turn(), &apple).await);
    let members = team.members();
    assert!(
        calls(&service.seen()).is_empty(),
        "nothing is called before an answer"
    );
    started.shutdown().await;

    let started = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let team = Team::back(started.workspaces(), members);
    team.approve(&team.turn(), &apple)
        .await
        .expect("her yes calls the command after the restart");
    assert_eq!(
        calls(&service.seen()),
        [("plant_tree".to_owned(), json!({"kind": "apple", "row": 3}))]
    );
    let used = team.uses(&id).await;
    assert_eq!(used.as_array().map(Vec::len), Some(1), "{used}");
    assert_eq!(used[0]["command"], "plant_tree");
    assert_eq!(used[0]["leave"], "once");
    started.shutdown().await;
}

/// After her "always", a grant of the workspace, that command of that connection is called
/// unasked.
async fn always_lets_it_go_ahead_unasked(
    team: &Team<'_>,
    owner: &DeviceCert,
    (service, id): (&StandIn, &str),
) -> GrantId {
    let turn = team.turn();
    let planting = calling(id, "plant_tree", json!({"kind": "apple"}));
    asked(team.takes(&turn, &planting).await);
    let permission = team.always(owner, id, "plant_tree").await;
    team.approve(&turn, &planting)
        .await
        .expect("the person's always calls the command");

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
    let used = team.uses(id).await;
    assert_eq!(used.as_array().map(Vec::len), Some(2), "{used}");
    assert!(used[0]["leave"] == "always" && used[1]["leave"] == "always");
    permission
}

/// Their "always" lets one command of one connection go ahead unasked for
/// the agent that asked. It covers no other connection and no other agent
/// of theirs, and once they take it back the command asks again.
#[tokio::test]
async fn always_covers_one_command_of_one_connection_until_its_person_takes_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    // The device keeps its channel keys across the restart, as a device's key service does.
    let keys = Arc::new(TestGroupKeys::default());
    let started = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let (service, another) = (StandIn::start("plain"), StandIn::start("plain"));
    let id = team.connects(&service).await;
    let other = team.connects(&another).await;
    let permission = always_lets_it_go_ahead_unasked(&team, &owner, (&service, &id)).await;

    let members = team.members();
    started.shutdown().await;
    let started = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
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
    let second = on_turn(&team.ada, &team.workspace, "another-agent");
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
    let (key, owner) = hosting_device();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    // This service lists `plant_tree` as changing something the first
    // time, and from then on as only reading, beside a command it adds.
    let service = StandIn::start("lying");
    let id = team.connects(&service).await;
    let turn = team.turn();

    let planting = calling(&id, "plant_tree", json!({"kind": "apple"}));
    let asks = asked(team.takes(&turn, &planting).await);
    let carried = team.approve(&turn, &planting).await;
    assert!(refused_with(&carried, DEFINED_ANEW), "{carried:?}");
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
    assert_eq!(
        again, asks,
        "it asks again, about the command as it is defined now"
    );
    // A command the service adds and marks as only reading asks as well.
    asked(
        team.takes(&turn, &calling(&id, "fell_all", json!({})))
            .await,
    );
    assert!(calls(&service.seen()).is_empty());
    started.shutdown().await;
}

/// The rule that names the connection `id` in `workspace` for calling its commands: the one
/// form in which the workspace's list of its agents keeps an outside effect allowed unasked for
/// a connection.
fn naming(workspace: &str, id: &str, answer: Answer) -> Rule {
    Rule::new(
        TreePath::workspace(workspace)
            .app(person_connections::PROVIDER)
            .kind(person_connections::CONNECTION_KIND)
            .resource(person_connections::connection_resource(id))
            .rest()
            .action(CALL),
        KindSet::of(ActionKind::Outside),
        answer,
    )
}

/// Rules in `workspace` that name no connection, another one, or this one only to deny it:
/// each with why it lets a narrowed agent reach nothing of `id`.
fn rules_that_do_not_reach(
    workspace: &str,
    id: &str,
    other: &str,
) -> Vec<(&'static str, Vec<Rule>)> {
    let provider = TreePath::workspace(workspace).app(person_connections::PROVIDER);
    // Calling any connection's commands names none of them.
    let every_connection = Rule::new(
        provider
            .clone()
            .kind(person_connections::CONNECTION_KIND)
            .rest()
            .action(CALL),
        KindSet::of(ActionKind::Outside),
        Answer::Allow,
    );
    vec![
        (
            "a rule for another app alone",
            narrowed_elsewhere(workspace),
        ),
        (
            "a rule for the whole provider's changes",
            vec![Rule::new(
                provider,
                KindSet::of(ActionKind::Undoable),
                Answer::Allow,
            )],
        ),
        ("a rule for every connection", vec![every_connection]),
        (
            "a rule that names another connection",
            vec![naming(workspace, other, Answer::Allow)],
        ),
        (
            "a rule that names it and one that denies calling it",
            vec![
                naming(workspace, id, Answer::Allow),
                naming(workspace, id, Answer::Deny),
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
    let (key, owner) = hosting_device();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let (service, another) = (StandIn::start("plain"), StandIn::start("plain"));
    let id = team.connects(&service).await;
    let other = team.connects(&another).await;
    let sub = on_turn(&team.ada, &team.workspace, "sub-agent");
    let its = person_connections::connection_resource(&id);
    let finding = calling(&id, "find_trees", json!({"orchard": "north"}));

    for (why, bound) in rules_that_do_not_reach(&team.workspace, &id, &other) {
        let policy = narrowing(&team.workspace, &sub, &bound);
        let turn = team.narrowed(&sub, &policy);
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
    let bound = vec![naming(&team.workspace, id, Answer::Allow)];
    let policy = narrowing(&team.workspace, sub, &bound);
    let turn = team.narrowed(sub, &policy);
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

/// An agent's proposal adds nothing: it waits for its person, who is shown the service by the
/// site of its address and what a card of it needs, and nothing is connected before they
/// approve. What they are shown.
async fn a_proposal_asks_and_adds_nothing(team: &Team<'_>, service: &StandIn) -> AskedFacts {
    let proposing = of(PROPOSE, json!({"address": service.address()}));
    let turn = team.turn();
    let asks = asked(team.takes(&turn, &proposing).await);
    assert_eq!(asked_person(turn.caller), Some(team.ada_id.clone()));
    assert_eq!(asks.plural, None, "a connection is never added unasked");
    assert_eq!(asks.on.as_deref(), Some(service.site().as_str()));
    assert_eq!(fact(&asks, "Address"), service.address());
    assert_eq!(fact(&asks, "It calls itself"), "Orchard Tools");
    assert_eq!(fact(&asks, "Commands"), "3");
    assert_eq!(
        team.her_connections().await,
        json!([]),
        "an agent's call alone adds nothing"
    );
    asks
}

/// An agent proposes a connection from an address, and nothing is added by its call or before
/// its person approves; their approve adds the connection as theirs. A service that wants a
/// sign-in says so, and an address inside the server's own network is not looked at unless its
/// operator lets it.
#[tokio::test]
async fn an_agent_proposes_a_connection_and_only_its_persons_yes_adds_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let started = start_serving(dir.path(), &key, &owner).await;
    let team = Team::on(started.workspaces(), &owner).await;
    let (service, signing) = (StandIn::start("plain"), StandIn::start("signing-in"));
    let turn = team.turn();
    a_proposal_asks_and_adds_nothing(&team, &service).await;
    assert_eq!(team.her_connections().await, json!([]));

    let proposing = of(PROPOSE, json!({"address": service.address()}));
    team.approve(&turn, &proposing)
        .await
        .expect("the person's yes adds the connection");
    let hers = team.her_connections().await;
    assert_eq!(hers.as_array().map(Vec::len), Some(1));
    assert_eq!(
        hers[0]["address"],
        service.address(),
        "a client finds the connection it was asked about by its address"
    );
    assert_eq!(hers[0]["state"], "ready");
    let refused = team.takes(&turn, &proposing).await;
    assert!(refused_with(&refused, "connected already"), "{refused:?}");

    // A service that wants a sign-in says so, with where its person signs in: signing in stays
    // their own act.
    let proposing = of(PROPOSE, json!({"address": signing.address()}));
    let asks = asked(team.takes(&turn, &proposing).await);
    assert_eq!(fact(&asks, "Sign in at"), signing.issuer_site().as_str());

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
    team.approve(&turn, &planting)
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
    let (key, owner) = hosting_device();
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
