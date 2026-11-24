//! A member device that keeps a copy of a workspace changes its built-in records (its emoji, its
//! agents, its contacts) on that copy through its own dispatch, its host reached or not: the
//! change names the record's own app, so the copy takes it, and carries it to the host later.

use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::remote::{
    Caller, InvocationReceipt, InvokeArgument, InvokeRequest, RESOURCE_PROVIDER, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_desktop_model::WorkspaceId;
use tilo_user_identity::DeviceCert;

use crate::device::ticket_tests::{Studio, studio};
use crate::device::workspaces::ServiceWorkspaces;

/// A picture an emoji keeps: a PNG's signature and a few bytes, base64.
const PNG: &str =
    "iVBORw0KGgoBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ==";

/// A workspace's built-in record: the app it is of and its type.
struct Record {
    app: &'static str,
    kind: &'static str,
}

const EMOJI: Record = Record {
    app: tilo_app_id::apps::CHAT,
    kind: "tilo.workspace.emoji",
};
const AGENTS: Record = Record {
    // The workspace's own service keeps its agents (`tilo_workspace_agent::OWNER`).
    app: "so.tilo.workspace",
    kind: "tilo.workspace.agent",
};
const CONTACTS: Record = Record {
    app: tilo_app_id::apps::CONTACTS,
    kind: "tilo.contacts.identities",
};

impl Record {
    fn id(&self) -> String {
        collab_types::workspace_document_id(self.kind)
    }

    /// Whether `device`'s copy of `workspace` holds this record.
    async fn kept(&self, device: &ServiceWorkspaces, workspace: &str) -> bool {
        matches!(
            device
                .copies()
                .resource_document(workspace, self.app, &self.id())
                .await,
            Ok(Some(_))
        )
    }
}

/// Run `action` of `record` in `workspace` through `device`'s own dispatch, as the person whose
/// device `cert` certifies.
async fn act(
    device: &ServiceWorkspaces,
    cert: &DeviceCert,
    workspace: &str,
    record: &Record,
    action: &str,
    argument: Value,
) -> InvocationReceipt {
    let Value::Object(argument) = argument else {
        panic!("an action takes named arguments");
    };
    let pending = device
        .copies()
        .providers()
        .invoke(
            &Caller::authenticated(cert.user_public_key, cert.device_public_key)
                .in_workspace(workspace.to_owned()),
            InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: format!("{RESOURCE_PROVIDER}:{}:{}", record.app, record.id()),
                    address: tilo_actions::ActionAddress::new(record.kind, action),
                },
                argument: InvokeArgument::Json(argument),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: Some(20_000),
                appearance: None,
                signed: None,
            },
        )
        .expect("the device's dispatch takes a built-in record's action");
    tilo_test_env::within("the device answers the action", pending)
        .await
        .expect("the call is answered")
        .expect("the device takes the call")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_device_changes_its_workspaces_built_in_records_on_its_copy_offline() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let Studio {
        server,
        workspace,
        laptop_key,
        laptop_cert,
        login,
        ..
    } = studio(&root, state.path()).await;
    let laptop = ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
    .expect("the laptop's service opens");
    let entered = tilo_test_env::within(
        "the laptop enters its workspace",
        tilo_api::daemon::enter::connect(&laptop_key, &login, Some(laptop.lent())),
    )
    .await
    .expect("the laptop enters");
    tilo_test_env::within("the laptop keeps its copy", laptop.hold(&entered)).await;
    assert!(laptop.keeps(WorkspaceId::parse(&workspace).unwrap()).await);
    // Each record is made while the host is reached: what a record is sealed under is settled
    // with the workspace's host as it is first written.
    let made: [(&Record, &str, Value); 3] = [
        (&EMOJI, "add-emoji", json!({"name": "party", "image": PNG})),
        (&AGENTS, "create-agent", json!({"name": "Scout"})),
        (
            &CONTACTS,
            "project-contact",
            json!({"person": uuid::Uuid::new_v4().to_string(), "label": "Grace"}),
        ),
    ];
    for (record, action, argument) in made {
        tilo_test_env::within("the laptop makes the record with its host reached", async {
            loop {
                let receipt = act(
                    &laptop,
                    &laptop_cert,
                    &workspace,
                    record,
                    action,
                    argument.clone(),
                )
                .await;
                match receipt.status {
                    ReceiptStatus::Completed { .. } => return,
                    // The copy is still taking the workspace in from its host.
                    ReceiptStatus::Failed { error } if error.code.starts_with("not_") => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    status => panic!("{} is made on the laptop's copy: {status:?}", record.kind),
                }
            }
        })
        .await;
    }

    // The host goes away; the laptop goes on changing each record on its copy.
    server.shutdown().await;
    let offline: [(&Record, &str, Value); 3] = [
        (
            &EMOJI,
            "add-emoji",
            json!({"name": "offline_party", "image": PNG}),
        ),
        (&AGENTS, "create-agent", json!({"name": "Ranger"})),
        (
            &CONTACTS,
            "project-contact",
            json!({"person": uuid::Uuid::new_v4().to_string(), "label": "Ada"}),
        ),
    ];
    for (record, action, argument) in offline {
        let receipt = act(&laptop, &laptop_cert, &workspace, record, action, argument).await;
        assert!(
            matches!(receipt.status, ReceiptStatus::Completed { .. }),
            "the laptop's copy takes {} with its host gone: {receipt:?}",
            record.kind
        );
        assert!(
            record.kept(&laptop, &workspace).await,
            "{} is in the laptop's copy",
            record.kind
        );
    }
    laptop.shutdown().await;
}
