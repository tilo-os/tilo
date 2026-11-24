//! A member device that keeps a copy of a workspace makes its built-in records (its emoji, its
//! agents, its contacts) on that copy through its own dispatch, its host reached or not: the
//! change names the record's own app, so the copy takes it, and carries it to the host later.
//! Every member reads these records, so the device seals them under the workspace's group, whose
//! keys it already holds: nothing about a new record waits for the host.

use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::ReadDoc as _;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{
    Caller, InvocationReceipt, InvokeArgument, InvokeRequest, RESOURCE_PROVIDER, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_desktop_model::WorkspaceId;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, UserIdentity};

use crate::device::ticket_tests::{Studio, enrol, login_to, studio};
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

    /// Every key and string `device`'s copy of `workspace` reads in this record, opened with its
    /// keys; `Err` says why it reads none.
    async fn texts(
        &self,
        device: &ServiceWorkspaces,
        workspace: &str,
    ) -> Result<Vec<String>, String> {
        let bytes = device
            .copies()
            .resource_document(workspace, self.app, &self.id())
            .await
            .map_err(|error| error.to_string())?
            .ok_or("the copy holds none of the record")?;
        let document = automerge::Automerge::load(&bytes).map_err(|error| error.to_string())?;
        let mut texts = Vec::new();
        texts_in(&document, &automerge::ROOT, &mut texts);
        Ok(texts)
    }

    /// Whether `device`'s copy of `workspace`, opened with its keys, holds `text` in this record.
    async fn reads(&self, device: &ServiceWorkspaces, workspace: &str, text: &str) -> bool {
        self.texts(device, workspace)
            .await
            .is_ok_and(|texts| texts.iter().any(|kept| kept.contains(text)))
    }
}

/// Every key and string `document` holds under `object`, at any depth.
fn texts_in(document: &automerge::Automerge, object: &automerge::ObjId, texts: &mut Vec<String>) {
    let children: Vec<_> = match document.object_type(object) {
        Ok(automerge::ObjType::Map | automerge::ObjType::Table) => document
            .keys(object)
            .filter_map(|key| {
                let child = document.get(object, key.as_str()).ok().flatten();
                texts.push(key);
                child
            })
            .collect(),
        Ok(automerge::ObjType::List) => (0..document.length(object))
            .filter_map(|index| document.get(object, index).ok().flatten())
            .collect(),
        Ok(automerge::ObjType::Text) => {
            texts.extend(document.text(object).ok());
            return;
        }
        Err(_) => return,
    };
    for (value, id) in children {
        match value {
            automerge::Value::Object(_) => texts_in(document, &id, texts),
            automerge::Value::Scalar(scalar) => {
                if let automerge::ScalarValue::Str(text) = scalar.as_ref() {
                    texts.push(text.to_string());
                }
            }
        }
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

/// A device's own service, opened in `dir` for the server `login` names.
async fn service_of(
    dir: &std::path::Path,
    (key, cert): (&SigningKey, &DeviceCert),
    login: &tilo_device_identity::login::Device,
) -> Arc<ServiceWorkspaces> {
    ServiceWorkspaces::open_for_test(
        dir,
        key,
        cert,
        &tilo_api::daemon::enter::server_of(login).await.unwrap(),
    )
    .await
    .expect("the device's service opens")
}

/// [`service_of`], entered into the workspace `login` names and keeping its copy.
async fn copy_of(
    dir: &std::path::Path,
    (key, cert): (&SigningKey, &DeviceCert),
    login: &tilo_device_identity::login::Device,
) -> Arc<ServiceWorkspaces> {
    let device = service_of(dir, (key, cert), login).await;
    let entered = tilo_test_env::within(
        "the device enters the workspace",
        tilo_api::daemon::enter::connect(key, login, Some(device.lent())),
    )
    .await
    .expect("the device enters");
    tilo_test_env::within("the device keeps its copy", device.hold(&entered)).await;
    device
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one member's records made offline, synced and read by others, read top to bottom"
)]
async fn a_member_device_makes_its_workspaces_built_in_records_offline_and_members_read_them() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let Studio {
        server,
        workspace,
        laptop_key,
        laptop_cert,
        login,
        server_key,
        server_cert,
        ..
    } = studio(&root, state.path()).await;
    let laptop = copy_of(
        &state.path().join("laptop"),
        (&laptop_key, &laptop_cert),
        &login,
    )
    .await;
    assert!(laptop.keeps(WorkspaceId::parse(&workspace).unwrap()).await);
    // A member device holds the keys of the workspace's group once it is admitted to it: what
    // every member reads is sealed under them.
    laptop
        .keys()
        .group_keys(
            &workspace,
            collab_types::EVERY_MEMBER,
            true,
            Duration::from_secs(30),
        )
        .await
        .expect("the laptop takes part in the workspace's group")
        .expect("the laptop holds the workspace's keys");

    // The host goes away before any of the records exists; the laptop makes each on its copy,
    // and each write is taken at once.
    server.shutdown().await;
    let made: [(&Record, &str, Value, &str); 3] = [
        (
            &EMOJI,
            "add-emoji",
            json!({"name": "offline_party", "image": PNG}),
            "offline_party",
        ),
        (&AGENTS, "create-agent", json!({"name": "Ranger"}), "Ranger"),
        (
            &CONTACTS,
            "project-contact",
            json!({"person": uuid::Uuid::new_v4().to_string(), "label": "Grace"}),
            "Grace",
        ),
    ];
    for (record, action, argument, text) in &made {
        let started = Instant::now();
        let receipt = act(
            &laptop,
            &laptop_cert,
            &workspace,
            record,
            action,
            argument.clone(),
        )
        .await;
        assert!(
            matches!(receipt.status, ReceiptStatus::Completed { .. }),
            "the laptop's copy takes {} with its host gone: {receipt:?}",
            record.kind
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{} is made at once, not after a wait for the host: {:?}",
            record.kind,
            started.elapsed()
        );
        assert!(
            record.reads(&laptop, &workspace, text).await,
            "the laptop reads the {} it made offline: {:?}",
            record.kind,
            record.texts(&laptop, &workspace).await
        );
    }

    // The laptop is closed while offline; the host comes back, the laptop is opened again and
    // reaches it, and carries what it made there. Another member's device then reads all three.
    laptop.shutdown().await;
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig::loopback(state.path().join("server")),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .expect("the host starts again");
    let hosted = server
        .workspaces()
        .expect("the host holds workspaces")
        .clone();
    let login = login_to(&server, &workspace, (&laptop_cert, &server_cert));
    let laptop = copy_of(
        &state.path().join("laptop"),
        (&laptop_key, &laptop_cert),
        &login,
    )
    .await;
    let (grace, _) = UserIdentity::generate("Grace", 1);
    let (grace_key, grace_cert) = enrol(&grace, "grace-laptop", DeviceCapabilities::primary());
    hosted
        .add_member(
            &workspace,
            &server_cert,
            tilo_workspace_service::workspaces::AddMemberRequest {
                request_id: [7; 32],
                user_public_key: grace_cert.user_public_key,
                place: tilo_workspace_service::workspaces::MemberPlace::Role(
                    tilo_server_protocol::workspaces::Role::Editor,
                ),
                name: Some("Grace".into()),
            },
        )
        .await
        .expect("the owner adds Grace");
    hosted
        .enter(&workspace, &grace_cert)
        .await
        .expect("Grace's device enters the workspace");
    let grace_login = login_to(&server, &workspace, (&grace_cert, &server_cert));
    let grace_device = copy_of(
        &state.path().join("grace"),
        (&grace_key, &grace_cert),
        &grace_login,
    )
    .await;
    tilo_test_env::within("Grace's device reads what the laptop made offline", async {
        loop {
            let mut all = true;
            for (record, _, _, text) in &made {
                all &= record.reads(&grace_device, &workspace, text).await;
            }
            if all {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;

    // Someone who is no member reads none of it: their device is not let in, so it holds no copy
    // and none of the workspace's keys.
    let (carol, _) = UserIdentity::generate("Carol", 1);
    let (carol_key, carol_cert) = enrol(&carol, "carol-laptop", DeviceCapabilities::primary());
    assert!(
        hosted.enter(&workspace, &carol_cert).await.is_err(),
        "the host lets no one in who is no member"
    );
    let carol_login = login_to(&server, &workspace, (&carol_cert, &server_cert));
    let stranger = service_of(
        &state.path().join("carol"),
        (&carol_key, &carol_cert),
        &carol_login,
    )
    .await;
    let entered = tokio::time::timeout(
        Duration::from_secs(20),
        tilo_api::daemon::enter::connect(&carol_key, &carol_login, Some(stranger.lent())),
    )
    .await;
    assert!(
        !matches!(entered, Ok(Ok(_))),
        "a device of no member does not enter the workspace"
    );
    assert!(
        !stranger
            .keeps(WorkspaceId::parse(&workspace).unwrap())
            .await
    );
    assert!(
        !matches!(
            stranger
                .keys()
                .group_keys(
                    &workspace,
                    collab_types::EVERY_MEMBER,
                    false,
                    Duration::ZERO
                )
                .await,
            Some(Ok(_))
        ),
        "a device of no member holds none of the workspace's keys"
    );
    for (record, _, _, text) in &made {
        assert!(
            !record.reads(&stranger, &workspace, text).await,
            "Carol's device opens no {}",
            record.kind
        );
    }
    stranger.shutdown().await;
    grace_device.shutdown().await;
    laptop.shutdown().await;
    server.shutdown().await;
}
