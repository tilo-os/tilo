//! A workspace's connections on its server: who is answered. A member adds a
//! service from a device of theirs, reads it back and calls its commands;
//! every member reaches what the workspace connected; nobody outside it
//! reaches any of it; an agent a member certified reads, looks
//! and calls what only reads, and its changes ask its member; and a server
//! asks no address inside its own network unless its operator lets it.

use collab_types::WorkspaceId;
use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::person_connections::{
    self, ADD, BEGIN_SIGN_IN, CALL, FINISH_SIGN_IN, LIST, LOOK, REFRESH, REMOVE, USES,
};
use tilo_actions::remote::{
    ActionCallError as Refused, ActionProvider as _, Caller, CatalogueQuery, InvokeArgument,
    InvokeRequest, ReceiptStatus,
};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions as Served;
use super::super::tests::{admit, agent_caller, caller, governed, settled};
use crate::connections::ASKS_FIRST;
use crate::connections::stand_in::StandIn;
use crate::workspaces::fixtures::test_host as start_serving;
use crate::workspaces::tests::governor;
use crate::workspaces::{AsksPeople, PersonAnswer};

const NOT_KEPT: &str = "That connection is not kept here";
const NOT_CALLED: &str = "That address is not called from here";

/// A person who answers every question one way.
struct Asked {
    answer: Option<ConfirmationAnswer>,
}

impl Asked {
    fn answering(answer: Option<ConfirmationAnswer>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self { answer })
    }
}

impl AsksPeople for Asked {
    fn ask(&self, _: [u8; 32], _: PendingConfirmation) -> PersonAnswer {
        let answer = self
            .answer
            .clone()
            .ok_or_else(|| "nobody answered".to_owned());
        Box::pin(async move { answer })
    }
}

/// A workspace's action service as one caller asks it, on the device the
/// transport certified for them.
#[derive(Clone, Copy)]
struct Asking<'a> {
    actions: &'a Served,
    workspace_id: WorkspaceId,
    device: &'a DeviceCert,
    caller: &'a Caller,
}

impl<'a> Asking<'a> {
    /// The same service, asked by `caller` on `device`.
    const fn by(self, device: &'a DeviceCert, caller: &'a Caller) -> Self {
        Self {
            device,
            caller,
            ..self
        }
    }

    /// `action` of the workspace's connections: what it answered, or why not.
    async fn ask(self, action: &str, argument: Value) -> Result<Value, Refused> {
        let argument = match argument {
            Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
            _ => InvokeArgument::None,
        };
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: person_connections::target(action),
            argument,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let receipt = settled(
            self.actions
                .in_workspace_as(self.workspace_id, self.device)
                .invoke(self.caller, request),
        )
        .await?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            other => panic!("the workspace's connections answer at once: {other:?}"),
        }
    }

    /// The actions of the workspace's connections this caller finds in its
    /// listing.
    async fn offered(self) -> Vec<String> {
        let query = CatalogueQuery {
            provider: Some(person_connections::PROVIDER.into()),
            max_results: 100,
            ..CatalogueQuery::default()
        };
        let page = settled(
            self.actions
                .in_workspace_as(self.workspace_id, self.device)
                .query(self.caller, query),
        )
        .await
        .unwrap();
        page.rows
            .into_iter()
            .filter_map(|row| row.action.map(|action| action.address.action))
            .collect()
    }

    /// Whether this caller is told what one asking for something that does
    /// not exist is told, whatever of `asked` it asks.
    async fn is_answered_nothing(self, who: &str, asked: &[(&str, Value)]) {
        for (action, argument) in asked {
            let refused = self.ask(action, argument.clone()).await;
            assert!(
                matches!(refused, Err(Refused::UnknownAction)),
                "{who}, {action}: {refused:?}"
            );
        }
    }

    /// Whether this caller is refused `asked` because its answer is its
    /// member's to give: unanswered it waits for them.
    async fn asks_its_member(self, who: &str, asked: &[(&str, Value)]) {
        for (action, argument) in asked {
            let unasked = self.ask(action, argument.clone()).await;
            assert!(
                matches!(unasked, Err(Refused::ConfirmationRequired { .. })),
                "{who}, {action} with nobody to ask through: {unasked:?}"
            );
        }
    }
}

fn refused_with(refused: &Result<Value, Refused>, why: &str) -> bool {
    matches!(refused, Err(Refused::Refused { reason }) if reason == why)
}

/// What an agent a member certified is asked about before it
/// changes a connection `id` or the service at `address`: none of it is
/// its own to do unasked.
fn the_changes_an_agent_is_asked_about(id: &Value, address: &str) -> [(&'static str, Value); 4] {
    [
        (ADD, json!({"address": address})),
        (REMOVE, json!({"connection": id})),
        (
            BEGIN_SIGN_IN,
            json!({"connection": id, "returnTo": "https://app.example/signed-in"}),
        ),
        (
            FINISH_SIGN_IN,
            json!({"connection": id, "returned": "https://app.example/signed-in?code=c&state=s"}),
        ),
    ]
}

/// A member calls a command of the workspace's connection `id` and reads
/// what it was used for.
async fn a_member_calls_and_reads_its_uses(ada: Asking<'_>, id: &str) {
    let planting =
        json!({"connection": id, "command": "plant_tree", "arguments": {"kind": "apple"}});
    let planted = ada
        .ask(CALL, planting)
        .await
        .expect("Ada calls a command of the connection");
    assert_eq!(planted["isError"], false);
    let used = ada.ask(USES, json!({"connection": id})).await.unwrap();
    assert_eq!(used["uses"].as_array().map(Vec::len), Some(1));
    assert_eq!(used["uses"][0]["command"], "plant_tree");
    assert_eq!(used["uses"][0]["leave"], "person");
    assert_eq!(used["uses"][0]["ended"], "ok");
    let listed = ada.ask(LIST, json!({})).await.unwrap();
    assert!(listed["connections"][0]["lastUsedAtMs"].as_u64().is_some());
}

/// A member connects a service from a device of theirs, reads it back with
/// its name, its commands and how it stands, and calls a command of it.
/// Every member of the workspace reaches what it connected; a member of
/// another workspace, a device of nobody and a caller on another person's
/// device list nothing of it, read nothing of it again, call nothing of it
/// and remove nothing of it, and the service is asked nothing for them.
#[tokio::test]
async fn a_member_connects_a_service_the_workspace_keeps_and_nobody_outside_reaches_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let workspaces = started.workspaces();
    workspaces.call_private_model_addresses(true);
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (elsewhere, _, _) = governed(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 3).await;
    let (_, stranger) = governor();
    let (as_ada, as_bo) = (caller(&ada), caller(&bo));
    let (as_cy, as_stranger) = (caller(&cy), caller(&stranger));
    let ada_asks = Asking {
        actions: &actions,
        workspace_id,
        device: &ada,
        caller: &as_ada,
    };
    let service = StandIn::start("plain");

    let added = ada_asks
        .ask(ADD, json!({"address": service.address()}))
        .await
        .expect("Ada connects a service");
    let id = added["id"].as_str().expect("an id").to_owned();
    assert_eq!(added["address"], service.address());
    assert_eq!(added["name"], "Orchard Tools");
    assert_eq!(added["state"], "ready");
    assert_eq!(added["signIn"], "none");
    assert_eq!(added["commands"].as_array().map(Vec::len), Some(3));
    assert!(added["addedAtMs"].as_u64().is_some_and(|added| added > 0));
    let hers = ada_asks.ask(LIST, json!({})).await.unwrap();
    assert_eq!(hers["connections"], json!([added]));
    a_member_calls_and_reads_its_uses(ada_asks, &id).await;
    assert!(!service.seen().is_empty());

    // Bo, another member of the workspace, reaches the connection the
    // workspace keeps.
    let bos_asks = ada_asks.by(&bo, &as_bo);
    let his = bos_asks.ask(LIST, json!({})).await.unwrap();
    assert_eq!(
        his["connections"].as_array().map(Vec::len),
        Some(1),
        "{his}"
    );
    assert_eq!(his["connections"][0]["id"], json!(id), "{his}");
    assert_eq!(his["connections"][0]["name"], "Orchard Tools", "{his}");
    let read = bos_asks
        .ask(REFRESH, json!({"connection": id}))
        .await
        .expect("Bo reads the workspace's connection");
    assert_eq!(read["state"], "ready", "{read}");
    service.seen();
    // A connection the workspace keeps not at all is no connection of his.
    let missing = json!({"connection": "no-such-connection"});
    for action in [REFRESH, REMOVE, USES] {
        let refused = bos_asks.ask(action, missing.clone()).await;
        assert!(refused_with(&refused, NOT_KEPT), "{action}: {refused:?}");
    }

    // Outside the workspace, the connection is nobody's to read or call.
    let reading = [
        (LIST, json!({})),
        (REFRESH, json!({"connection": id})),
        (USES, json!({"connection": id})),
        (REMOVE, json!({"connection": id})),
        (CALL, json!({"connection": id, "command": "find_trees"})),
    ];
    for (who, other) in [
        ("a member of another workspace", ada_asks.by(&cy, &as_cy)),
        ("a device of nobody", ada_asks.by(&stranger, &as_stranger)),
        (
            "her key on another person's device",
            ada_asks.by(&bo, &as_ada),
        ),
    ] {
        other.is_answered_nothing(who, &reading).await;
        assert!(other.offered().await.is_empty(), "{who}");
    }
    assert!(
        service.seen().is_empty(),
        "the service was asked for someone the connection is not the workspace's for"
    );

    // The connection is kept with what the workspace keeps on this host.
    let kept = workspaces
        .workspace_root(&workspace_id.to_string())
        .join("connections")
        .join(format!("{id}.json"));
    assert!(
        kept.is_file(),
        "the connection is kept at {}",
        kept.display()
    );
    let still = ada_asks.ask(LIST, json!({})).await.unwrap();
    assert_eq!(still["connections"][0]["id"], added["id"]);
    let remaining = ada_asks
        .ask(REMOVE, json!({"connection": id}))
        .await
        .expect("Ada removes the workspace's connection");
    assert_eq!(remaining["connections"], json!([]));
    assert_eq!(
        bos_asks.ask(LIST, json!({})).await.unwrap()["connections"],
        json!([]),
        "every member reads what the workspace keeps"
    );
    started.shutdown().await;
}

/// What an agent a member certified, with a device of theirs
/// behind it, does with the connection `added`: it reads the list, looks at
/// the service's address, reads the commands again and calls a command that
/// only reads. A command that changes something it does not call, and every
/// change of the connection waits for its member.
async fn an_agent_reads_and_calls_what_only_reads(
    agent: Asking<'_>,
    who: &str,
    (service, another): (&StandIn, &StandIn),
    added: &Value,
) {
    let id = &added["id"];
    let listed = agent
        .ask(LIST, json!({}))
        .await
        .unwrap_or_else(|refused| panic!("{who} reads the list: {refused:?}"));
    assert_eq!(listed["connections"][0]["id"], *id, "{who}");
    let looked = agent
        .ask(LOOK, json!({"address": service.address()}))
        .await
        .unwrap_or_else(|refused| panic!("{who} looks at an address: {refused:?}"));
    assert_eq!(looked["name"], "Orchard Tools", "{who}");
    let read = agent
        .ask(REFRESH, json!({"connection": id}))
        .await
        .unwrap_or_else(|refused| panic!("{who} reads the commands again: {refused:?}"));
    assert_eq!(read["state"], "ready", "{who}");
    let finding =
        json!({"connection": id, "command": "find_trees", "arguments": {"orchard": "north"}});
    let found = agent
        .ask(CALL, finding)
        .await
        .unwrap_or_else(|refused| panic!("{who} calls a command that only reads: {refused:?}"));
    assert_eq!(found["structured"], json!({"trees": 3}), "{who}");
    // What the call was used for is the workspace's to read.
    let uses = agent
        .ask(USES, json!({"connection": id}))
        .await
        .unwrap_or_else(|refused| panic!("{who} reads what it was used for: {refused:?}"));
    assert_eq!(uses["uses"].as_array().map(Vec::len), Some(1), "{who}");
    let planting =
        json!({"connection": id, "command": "plant_tree", "arguments": {"kind": "apple"}});
    let changing = agent.ask(CALL, planting).await;
    assert!(refused_with(&changing, ASKS_FIRST), "{who}: {changing:?}");
    agent
        .asks_its_member(
            who,
            &the_changes_an_agent_is_asked_about(id, &another.address()),
        )
        .await;
    assert_eq!(agent.offered().await, [LIST, LOOK, REFRESH, CALL], "{who}");
}

/// An agent a member certified reads and calls what only
/// reads, and that is all: every change of the connection is put to its
/// member, and only their yes makes it. The person themselves is offered no
/// agent's row: their own device asks the server directly.
#[tokio::test]
async fn a_members_agent_reads_and_calls_and_its_changes_ask_its_member() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let workspaces = started.workspaces();
    workspaces.call_private_model_addresses(true);
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let ada_asks = Asking {
        actions: &actions,
        workspace_id,
        device: &ada,
        caller: &as_ada,
    };
    let (service, another) = (StandIn::start("plain"), StandIn::start("plain"));
    let added = ada_asks
        .ask(ADD, json!({"address": service.address()}))
        .await
        .expect("Ada connects a service");
    let id = &added["id"];

    let agent = agent_caller(&ada, "assistant");
    an_agent_reads_and_calls_what_only_reads(
        ada_asks.by(&ada, &agent),
        "her assistant",
        (&service, &another),
        &added,
    )
    .await;

    // Nothing was added or removed, and the service nobody added was never
    // asked. The service that is connected was called for what only reads,
    // once, and never for the command that changes something.
    let hers = ada_asks.ask(LIST, json!({})).await.unwrap();
    assert_eq!(hers["connections"].as_array().map(Vec::len), Some(1));
    assert!(another.seen().is_empty(), "a refused call asked a service");
    let called: Vec<Value> = service
        .seen()
        .iter()
        .filter(|asked| asked["method"] == "tools/call")
        .map(|asked| asked["command"].clone())
        .collect();
    assert_eq!(called, [json!("find_trees")]);
    let used = ada_asks.ask(USES, json!({"connection": id})).await.unwrap();
    let used = used["uses"].as_array().expect("the uses");
    assert_eq!(used.len(), 2, "{used:?}");
    assert_eq!(used[0]["by"], "assistant", "{used:?}");
    assert_eq!(
        (&used[0]["leave"], &used[0]["ended"]),
        (&json!("read-only"), &json!("ok"))
    );
    assert_eq!(
        (&used[1]["leave"], &used[1]["ended"]),
        (&Value::Null, &json!("refused"))
    );

    // Her no refuses every change; her yes makes it, as the workspace's.
    let searching = ada_asks;
    workspaces.ask_people(Asked::answering(Some(ConfirmationAnswer::Deny)));
    for (action, argument) in [
        (ADD, json!({"address": another.address()})),
        (REMOVE, json!({"connection": id})),
    ] {
        let refused = searching.by(&ada, &agent).ask(action, argument).await;
        assert!(
            matches!(refused, Err(Refused::Refused { .. })),
            "{action}: {refused:?}"
        );
    }
    assert_eq!(
        searching.ask(LIST, json!({})).await.unwrap()["connections"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "nothing changed"
    );
    workspaces.ask_people(Asked::answering(Some(ConfirmationAnswer::AllowOnce)));
    let removed = searching
        .by(&ada, &agent)
        .ask(REMOVE, json!({"connection": id}))
        .await
        .expect("her yes removes it");
    assert_eq!(removed["connections"], json!([]));
    // The person themselves is offered no row: their own device asks the
    // server directly.
    assert!(ada_asks.offered().await.is_empty());
    started.shutdown().await;
}

/// A bot, an app, an agent no device of a member's stands behind, a member
/// of another workspace and the person's key on another person's device are
/// told what a caller asking for something that does not exist is told,
/// whatever they ask, and find nothing in their listing.
#[tokio::test]
async fn whoever_is_neither_a_member_nor_their_agent_is_answered_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let workspaces = started.workspaces();
    workspaces.call_private_model_addresses(true);
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (elsewhere, _, _) = governed(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 3).await;
    let (_, stranger) = governor();
    let as_ada = caller(&ada);
    let ada_asks = Asking {
        actions: &actions,
        workspace_id,
        device: &ada,
        caller: &as_ada,
    };
    let (service, another) = (StandIn::start("plain"), StandIn::start("plain"));
    let added = ada_asks
        .ask(ADD, json!({"address": service.address()}))
        .await
        .expect("Ada connects a service");
    let id = &added["id"];
    service.seen();

    let person = hex::encode(ada.user_public_key);
    let (as_cy, as_stranger) = (caller(&cy), caller(&stranger));
    let bot = Caller::local_bot("bot");
    let local = Caller::local_agent("assistant").acting_for(person.clone());
    let app = Caller::local_app("so.tilo.chat").acting_for(person);
    let nobody: [(&str, &DeviceCert, &Caller); 5] = [
        ("a bot nobody installed for her", &ada, &bot),
        (
            "an agent the server admitted itself, acting for her",
            &ada,
            &local,
        ),
        ("an app acting for her", &ada, &app),
        ("a member of another workspace", &cy, &as_cy),
        ("her key on another person's device", &bo, &as_ada),
    ];
    let reading = [
        (LIST, json!({})),
        (LOOK, json!({"address": another.address()})),
        (REFRESH, json!({"connection": id})),
        (CALL, json!({"connection": id, "command": "find_trees"})),
    ];
    for (who, device, asking) in nobody {
        let asking = ada_asks.by(device, asking);
        asking.is_answered_nothing(who, &reading).await;
        asking
            .is_answered_nothing(
                who,
                &the_changes_an_agent_is_asked_about(id, &another.address()),
            )
            .await;
        assert!(asking.offered().await.is_empty(), "{who}");
    }
    let asking = ada_asks.by(&stranger, &as_stranger);
    asking
        .is_answered_nothing("a device of nobody", &reading)
        .await;
    assert!(asking.offered().await.is_empty(), "a device of nobody");

    let hers = ada_asks.ask(LIST, json!({})).await.unwrap();
    assert_eq!(hers["connections"][0]["id"], *id);
    assert!(
        service.seen().is_empty(),
        "a refused call asked her service"
    );
    assert!(another.seen().is_empty(), "a refused call asked a service");
    started.shutdown().await;
}

/// A server asks no address inside its own network for a workspace unless
/// its operator lets it: such a service is neither looked at nor added, and
/// one added while the server was let is not asked after the leave is taken
/// back.
#[tokio::test]
async fn a_server_asks_no_address_inside_its_network_unless_its_operator_lets_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let workspaces = started.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let ada_asks = Asking {
        actions: &actions,
        workspace_id,
        device: &ada,
        caller: &as_ada,
    };
    let service = StandIn::start("plain");
    let with_tls = service.address().replacen("http://", "https://", 1);

    for (address, why) in [
        (
            service.address(),
            "A service's address begins with https://",
        ),
        (with_tls, NOT_CALLED),
    ] {
        for action in [LOOK, ADD] {
            let refused = ada_asks.ask(action, json!({"address": address})).await;
            assert!(
                refused_with(&refused, why),
                "{action} {address}: {refused:?}"
            );
        }
    }
    assert!(service.seen().is_empty(), "the service was asked");

    workspaces.call_private_model_addresses(true);
    let added = ada_asks
        .ask(ADD, json!({"address": service.address()}))
        .await
        .expect("Ada connects a service while the server is let");
    assert!(!service.seen().is_empty());
    workspaces.call_private_model_addresses(false);
    let stood = ada_asks
        .ask(REFRESH, json!({"connection": added["id"]}))
        .await
        .unwrap();
    assert_eq!(stood["state"], "refused");
    assert_eq!(stood["stateWhy"], NOT_CALLED);
    assert_eq!(stood["commands"], added["commands"]);
    assert!(
        service.seen().is_empty(),
        "the service was asked after the leave was taken back"
    );
    started.shutdown().await;
}

/// The same connections as a turn the server carries out itself reaches them.
mod turn;
