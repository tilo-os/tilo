//! A person's connections on their server: who is answered. The person adds
//! a service from their own device, reads it back and calls its commands;
//! nobody else in the workspace reaches it; an agent acting for them reads,
//! looks and calls what only reads, and no call of an agent's adds one,
//! removes one or signs in to one; and a server asks no address inside its
//! own network unless its operator lets it.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the serving device a test binds is kept until the test ends; dropping it earlier closes what the test drives"
)]

use collab_types::WorkspaceId;
use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::person_connections::{
    self, ADD, BEGIN_SIGN_IN, CALL, FINISH_SIGN_IN, LIST, LOOK, REFRESH, REMOVE, USES,
};
use tilo_actions::remote::{
    ActionHost as _, ActionHostError as Refused, Caller, CatalogueQuery, InvokeArgument,
    InvokeRequest, ReceiptStatus,
};
use tilo_user_identity::DeviceCert;

use super::super::HostWorkspaceActions as Served;
use super::super::tests::{admit, caller, governed, settled};
use crate::connections::ASKS_FIRST;
use crate::connections::stand_in::StandIn;
use crate::workspaces::tests::{governor, test_host as start_serving};

const NOT_KEPT: &str = "That connection is not kept here";
const NOT_CALLED: &str = "That address is not called from here";

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

    /// `action` of a person's connections: what it answered, or why not.
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
        };
        let receipt = settled(
            self.actions
                .in_workspace_as(self.workspace_id, self.device)
                .invoke(self.caller, request),
        )
        .await?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            other => panic!("a person's connections answer at once: {other:?}"),
        }
    }

    /// The actions of a person's connections this caller finds in its
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
}

fn refused_with(refused: &Result<Value, Refused>, why: &str) -> bool {
    matches!(refused, Err(Refused::Refused { reason }) if reason == why)
}

/// What only the person does with the connection `id`, and with the service
/// at `address`: none of it is an agent's.
fn the_persons_own(id: &Value, address: &str) -> [(&'static str, Value); 5] {
    [
        (ADD, json!({"address": address})),
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
    ]
}

/// The person calls a command of their connection `id` and reads what it
/// was used for.
async fn the_person_calls_and_reads_its_uses(ada: Asking<'_>, id: &str) {
    let planting =
        json!({"connection": id, "command": "plant_tree", "arguments": {"kind": "apple"}});
    let planted = ada
        .ask(CALL, planting)
        .await
        .expect("Ada calls a command of her connection");
    assert_eq!(planted["isError"], false);
    let used = ada.ask(USES, json!({"connection": id})).await.unwrap();
    assert_eq!(used["uses"].as_array().map(Vec::len), Some(1));
    assert_eq!(used["uses"][0]["command"], "plant_tree");
    assert_eq!(used["uses"][0]["leave"], "person");
    assert_eq!(used["uses"][0]["ended"], "ok");
    let listed = ada.ask(LIST, json!({})).await.unwrap();
    assert!(listed["connections"][0]["lastUsedAtMs"].as_u64().is_some());
}

/// A person connects a service from their own device, reads it back with
/// its name, its commands and how it stands, and calls a command of it.
/// Another member, the workspace's owner included, lists nothing of it,
/// reads nothing of it again, calls nothing of it and removes nothing of
/// it, and the service is asked nothing for them.
#[tokio::test]
async fn a_person_connects_a_service_and_nobody_else_reaches_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let workspaces = started.workspaces();
    workspaces.call_private_model_addresses(true);
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (as_ada, as_bo, as_owner) = (caller(&ada), caller(&bo), caller(&owner));
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
    the_person_calls_and_reads_its_uses(ada_asks, &id).await;
    assert!(!service.seen().is_empty());

    for other in [ada_asks.by(&bo, &as_bo), ada_asks.by(&owner, &as_owner)] {
        let theirs = other.ask(LIST, json!({})).await.unwrap();
        assert_eq!(theirs["connections"], json!([]), "{theirs}");
        let finding = json!({"connection": id, "command": "find_trees"});
        for (action, argument) in [
            (REFRESH, json!({"connection": id})),
            (REMOVE, json!({"connection": id})),
            (USES, json!({"connection": id})),
            (CALL, finding),
        ] {
            let refused = other.ask(action, argument).await;
            assert!(refused_with(&refused, NOT_KEPT), "{action}: {refused:?}");
        }
    }
    assert!(
        service.seen().is_empty(),
        "the service was asked for someone it is not connected for"
    );

    let own = workspaces.person_settings_root(&ada.user_public_key);
    assert!(
        own.join("connections").join(format!("{id}.json")).is_file(),
        "the connection is kept with Ada's own things"
    );
    let still = ada_asks.ask(LIST, json!({})).await.unwrap();
    assert_eq!(still["connections"][0]["id"], added["id"]);
    let remaining = ada_asks
        .ask(REMOVE, json!({"connection": id}))
        .await
        .expect("Ada removes her connection");
    assert_eq!(remaining["connections"], json!([]));
    started.shutdown().await;
}

/// What an agent acting for the person, with a device of theirs behind it,
/// does with the connection `added`: it reads the list, looks at the
/// service's address, reads the commands again and calls a command that
/// only reads. A command that changes something it does not call, and
/// what only the person does it is told does not exist.
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
    let planting =
        json!({"connection": id, "command": "plant_tree", "arguments": {"kind": "apple"}});
    let changing = agent.ask(CALL, planting).await;
    assert!(refused_with(&changing, ASKS_FIRST), "{who}: {changing:?}");
    agent
        .is_answered_nothing(who, &the_persons_own(id, &another.address()))
        .await;
    assert_eq!(agent.offered().await, [LIST, LOOK, REFRESH, CALL], "{who}");
}

/// No call of an agent's adds a connection, removes one or signs in to one.
/// An agent acting for the person, with a device of theirs behind it, reads
/// and calls what only reads, and that is all; every call it made or was
/// refused is written down for the person, by the agent's name.
#[tokio::test]
async fn an_agent_acting_for_the_person_reads_and_calls_and_adds_no_reach() {
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

    let hers: [(&str, Caller); 2] = [
        (
            "an agent certified under her key",
            Caller::delegated_agent("assistant", ada.user_public_key, ada.device_public_key),
        ),
        (
            "an agent her own device handed its connection to",
            caller(&ada).delegated_to("assistant"),
        ),
    ];
    for (who, agent) in &hers {
        let agent = ada_asks.by(&ada, agent);
        an_agent_reads_and_calls_what_only_reads(agent, who, (&service, &another), &added).await;
    }

    // Nothing was added or removed, and the service nobody added was never
    // asked. The service that is connected was called for what only reads,
    // once by each agent, and never for the command that changes something.
    let hers = ada_asks.ask(LIST, json!({})).await.unwrap();
    assert_eq!(hers["connections"].as_array().map(Vec::len), Some(1));
    assert!(another.seen().is_empty(), "a refused call asked a service");
    let called: Vec<Value> = service
        .seen()
        .iter()
        .filter(|asked| asked["method"] == "tools/call")
        .map(|asked| asked["command"].clone())
        .collect();
    assert_eq!(called, [json!("find_trees"), json!("find_trees")]);
    let used = ada_asks
        .ask(USES, json!({"connection": added["id"]}))
        .await
        .unwrap();
    let used = used["uses"].as_array().expect("the uses");
    assert_eq!(used.len(), 4, "{used:?}");
    for (index, used) in used.iter().enumerate() {
        assert_eq!(used["by"], "assistant", "{used}");
        let (leave, ended) = if index % 2 == 0 {
            (json!("read-only"), "ok")
        } else {
            (Value::Null, "refused")
        };
        assert_eq!((&used["leave"], &used["ended"]), (&leave, &json!(ended)));
    }
    // The person themselves is offered no row: their own device asks the
    // server directly.
    assert!(ada_asks.offered().await.is_empty());
    started.shutdown().await;
}

/// A bot, an app, an agent no device of the person's stands behind, and the
/// person's key on another person's device are told what a caller asking
/// for something that does not exist is told, whatever they ask, and find
/// nothing in their listing.
#[tokio::test]
async fn whoever_is_neither_the_person_nor_their_agent_is_answered_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let started = start_serving(dir.path(), &key, &owner).await;
    let workspaces = started.workspaces();
    workspaces.call_private_model_addresses(true);
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
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
    let nobody: [(&str, &DeviceCert, Caller); 4] = [
        (
            "a bot she installed",
            &ada,
            Caller::bot("bot", ada.user_public_key, ada.device_public_key),
        ),
        (
            "an agent the server admitted itself, acting for her",
            &ada,
            Caller::local_agent("assistant").acting_for(person.clone()),
        ),
        (
            "an app acting for her",
            &ada,
            Caller::local_app("so.tilo.chat").acting_for(person),
        ),
        ("her key on another person's device", &bo, caller(&ada)),
    ];
    let reading = [
        (LIST, json!({})),
        (LOOK, json!({"address": another.address()})),
        (REFRESH, json!({"connection": id})),
        (CALL, json!({"connection": id, "command": "find_trees"})),
    ];
    for (who, device, asking) in &nobody {
        let asking = ada_asks.by(device, asking);
        asking.is_answered_nothing(who, &reading).await;
        asking
            .is_answered_nothing(who, &the_persons_own(id, &another.address()))
            .await;
        assert!(asking.offered().await.is_empty(), "{who}");
    }

    let hers = ada_asks.ask(LIST, json!({})).await.unwrap();
    assert_eq!(hers["connections"][0]["id"], *id);
    assert!(
        service.seen().is_empty(),
        "a refused call asked her service"
    );
    assert!(another.seen().is_empty(), "a refused call asked a service");
    started.shutdown().await;
}

/// A server asks no address inside its own network for a person unless its
/// operator lets it: such a service is neither looked at nor added, and one
/// added while the server was let is not asked after the leave is taken
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
