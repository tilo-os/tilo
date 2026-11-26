//! The key a device derives for an app's own purpose ([`workspace_vault::DEVICE_KEY`]), as the app's
//! own process asks it ([`crate::vault_release`]): the same on every ask on this
//! device, another for every other purpose, app, workspace and device, kept nowhere, and refused
//! to everyone else.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::large_futures,
    reason = "in a test these are the assertion, and each test keeps its host to the end"
)]

use std::fmt::Write as _;
use std::sync::Arc;

use serde_json::json;
use tilo_actions::InvocationId;
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest};
use tilo_workspace_service::workspace_vault::{self, DeviceKey};

use crate::fixtures::{caller, hosted};
use crate::fixtures::{hosting_device, test_host};
use crate::hosted_use::turn_use_tests::on_turn;
use crate::person_vault::vault_tests::{ask_at, files_holding};
use crate::vault_release::{self as release, AppProcess};
use tilo_workspace_service::work::{HostConnections, Order, Turn, takes_turn_in};

/// The app whose own process asks for its key.
const BRIDGE: &str = "so.tilo.bridge";
/// Another app.
const READER: &str = "so.tilo.reader";
/// What the bridge asks its key for.
const PURPOSE: &str = "grayjay-noise";

/// The order here: this is the only device that runs the bridge, so it waits for no other.
const AT_ONCE: Order = Order {
    server_grace: std::time::Duration::ZERO,
    other_grace: std::time::Duration::ZERO,
};

/// The bridge, installed here; its process is the daemon's, so none runs.
struct Bridge;

impl HostConnections for Bridge {
    fn connections(&self) -> Vec<(String, Vec<String>)> {
        vec![(BRIDGE.to_owned(), Vec::new())]
    }
    fn installed(&self, app: &str) -> bool {
        app == BRIDGE
    }
    fn gives(&self, _: &str, _: &str, _: &str) -> bool {
        true
    }
    fn on_power(&self) -> bool {
        true
    }
    fn start(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn runs(&self, _: &str, _: &str) -> bool {
        false
    }
    fn stop(&self, _: &str, _: &str) {}
}

/// The ask for the key of `purpose`, as the app's own process calls it.
fn device_key_of(purpose: &str) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: workspace_vault::target(workspace_vault::DEVICE_KEY),
        argument: InvokeArgument::Json(json!({ "purpose": purpose }).as_object().cloned().unwrap()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// `app`'s key for `purpose` in `workspace`, asked by `process`.
async fn key_of(
    workspaces: &crate::PersonServices,
    (app, workspace): (&str, &str),
    process: &AppProcess,
    purpose: &str,
) -> Result<DeviceKey, ActionCallError> {
    workspaces
        .device_key_for((app, workspace), process, &device_key_of(purpose))
        .await
}

/// The reason `taken` was refused with.
fn refusal<T: std::fmt::Debug>(taken: Result<T, ActionCallError>) -> String {
    match taken {
        Err(ActionCallError::Refused { reason }) => reason,
        other => panic!("refused: {other:?}"),
    }
}

/// The bridge's own process is answered one key per purpose, the same on every ask, and so is
/// its held connection once this device holds it. Another purpose, app or workspace answers
/// another key, which is not the device's own. No record of a call, vault item or file holds it.
/// A process no turn placed, a tilolang process apps share, a connection another device holds, a
/// person, an agent and an app's code reaching the vault itself are refused.
#[tokio::test]
async fn a_device_holds_one_key_per_app_and_purpose_answered_to_the_apps_own_process_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let records = Arc::new(tilo_invocations::Invocations::default());
    records.keep_no_secrets(workspaces.secret_parameters());
    workspaces.serve_invocations(Arc::clone(&records));
    let own = AppProcess::OwnTilolang;

    let first = key_of(workspaces, (BRIDGE, &workspace), &own, PURPOSE)
        .await
        .expect("the bridge's own process is answered its key");
    assert_eq!(first.key.len(), 64, "32 bytes, as hex");
    assert_eq!(
        key_of(workspaces, (BRIDGE, &workspace), &own, PURPOSE)
            .await
            .unwrap(),
        first,
        "the same app and purpose answer the same key"
    );
    let elsewhere = format!("{workspace}-elsewhere");
    for ((app, workspace), purpose, what) in [
        ((BRIDGE, workspace.as_str()), "signing", "purpose"),
        ((READER, workspace.as_str()), PURPOSE, "app"),
        ((BRIDGE, elsewhere.as_str()), PURPOSE, "workspace"),
    ] {
        let other = key_of(workspaces, (app, workspace), &own, purpose)
            .await
            .unwrap();
        assert_ne!(other, first, "another {what} answers another key");
    }
    assert_ne!(first.key, hex::encode(key.to_bytes()));
    assert_ne!(first.key, hex::encode(key.verifying_key().to_bytes()));

    // The held connection is answered the same key, once this device holds it.
    workspaces.run_connections(Arc::new(Bridge));
    let held = AppProcess::connection(BRIDGE);
    assert_eq!(
        refusal(key_of(workspaces, (BRIDGE, &workspace), &held, PURPOSE).await),
        release::held_elsewhere(BRIDGE)
    );
    takes_turn_in(
        workspaces,
        &workspace,
        &Turn::connection(BRIDGE, Vec::new()),
        AT_ONCE,
    )
    .await
    .expect("this device takes the bridge's connection");
    assert_eq!(
        key_of(workspaces, (BRIDGE, &workspace), &held, PURPOSE)
            .await
            .unwrap(),
        first
    );

    for (process, why) in [
        (
            AppProcess::Unplaced,
            release::from_an_unplaced_process(BRIDGE),
        ),
        (
            AppProcess::Tilolang,
            release::from_a_tilolang_process(BRIDGE),
        ),
    ] {
        assert_eq!(
            refusal(key_of(workspaces, (BRIDGE, &workspace), &process, PURPOSE).await),
            why
        );
    }
    assert_eq!(
        refusal(key_of(workspaces, (BRIDGE, &workspace), &own, " ").await),
        release::NOT_A_PURPOSE
    );

    // Whoever reaches the vault itself is refused: a person, an agent, and an app's code, which
    // calls through its broker as a process apps share, for its own app or another.
    let through_the_vault = |caller: Caller| {
        let (service, owner) = (&service, &owner);
        async move {
            ask_at(
                service,
                workspace_id,
                (owner, &caller),
                workspace_vault::target(workspace_vault::DEVICE_KEY),
                json!({ "purpose": PURPOSE }),
            )
            .await
        }
    };
    assert_eq!(
        refusal(through_the_vault(caller(&owner).in_workspace(&workspace)).await),
        release::BY_A_PERSON
    );
    assert_eq!(
        refusal(through_the_vault(on_turn(&owner, &workspace, "scout")).await),
        release::BY_AN_AGENT
    );
    for app in [BRIDGE, READER] {
        let code = Caller::local_app(app)
            .acting_for(hex::encode(owner.user_public_key))
            .in_workspace(&workspace);
        assert_eq!(
            refusal(through_the_vault(code).await),
            release::from_a_shared_process(app)
        );
    }

    // Each ask is in the device's record as the bridge's, keeping only that a key was given.
    let listed = records.listed(&workspace);
    let asked: Vec<_> = listed
        .iter()
        .filter(|recorded| {
            recorded.request.target == workspace_vault::target(workspace_vault::DEVICE_KEY)
        })
        .collect();
    assert!(!asked.is_empty(), "the asks are recorded");
    assert!(asked.iter().all(|recorded| recorded.secret));
    assert!(asked.iter().all(|recorded| {
        recorded
            .caller
            .acting()
            .app()
            .map(std::string::ToString::to_string)
            == Some(BRIDGE.to_owned())
            || recorded
                .caller
                .acting()
                .app()
                .map(std::string::ToString::to_string)
                == Some(READER.to_owned())
    }));
    let mut recorded = String::new();
    for record in &listed {
        write!(recorded, "{:?}", record.state).unwrap();
    }
    assert!(!recorded.contains(&first.key), "a record holds the key");
    assert!(
        workspaces
            .open_vault(&workspace)
            .await
            .unwrap()
            .items()
            .facts()
            .is_empty(),
        "the key is no vault item"
    );
    let raw: Vec<u8> = (0..32)
        .map(|at| u8::from_str_radix(&first.key[at * 2..at * 2 + 2], 16).unwrap())
        .collect();
    assert!(files_holding(dir.path(), &raw).is_empty());
    assert!(files_holding(dir.path(), first.key.as_bytes()).is_empty());
}

/// Two devices of one person hold another key each for the same app, purpose and workspace: the
/// key is the device's own, never a resource of the workspace that syncs.
#[tokio::test]
async fn another_device_holds_another_key() {
    let (dir, other_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (key, owner) = hosting_device();
    let (other_key, other_owner) = hosting_device();
    assert_eq!(owner.user_public_key, other_owner.user_public_key);
    let server = test_host(dir.path(), &key, &owner).await;
    let other = test_host(other_dir.path(), &other_key, &other_owner).await;
    let (workspace, _, _) = hosted(&crate::PersonServices::of(server.workspaces())).await;
    let own = AppProcess::OwnTilolang;
    let here = key_of(
        &crate::PersonServices::of(server.workspaces()),
        (BRIDGE, &workspace),
        &own,
        PURPOSE,
    )
    .await
    .unwrap();
    let there = key_of(
        &crate::PersonServices::of(other.workspaces()),
        (BRIDGE, &workspace),
        &own,
        PURPOSE,
    )
    .await
    .unwrap();
    assert_ne!(here, there);
}
