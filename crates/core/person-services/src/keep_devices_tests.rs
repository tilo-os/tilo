//! A secret an app keeps is a resource of the workspace, not of the device it was kept on: the
//! same app's held connection on another device of the same person reads it back under the same
//! rule, with nobody asked.
#![allow(
    clippy::unwrap_used,
    clippy::large_futures,
    clippy::significant_drop_tightening,
    reason = "a fixture failing is its test failing, and each test keeps both devices to the end"
)]

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::json;
use tilo_actions::InvocationId;
use tilo_actions::remote::{InvokeArgument, InvokeRequest};
use tilo_user_identity::DeviceCapabilities;
use tilo_workspace_records::vault as workspace_vault;

use crate::fixtures::device_cert;
use crate::fixtures::{member_service, shared_with};
use crate::vault_release::AppProcess;
use tilo_workspace_service::work::{HostConnections, Order, StartRefusal, Turn, takes_turn_in};

const BRIDGE: &str = "so.tilo.bridge";
/// A made-up bot token: no service ever issued it.
const TOKEN: &str = "made-up-bot-token-0123456789abcdef";

/// The bridge, installed on the laptop; its process is the daemon's, so none runs here.
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
    fn start<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), StartRefusal>> + Send + 'a>>
    {
        Box::pin(async { Ok(()) })
    }
    fn runs(&self, _: &str, _: &str) -> bool {
        false
    }
    fn stop(&self, _: &str, _: &str) {}
}

fn request(
    target: tilo_actions::remote::RemoteActionTarget,
    argument: InvokeArgument,
) -> InvokeRequest {
    InvokeRequest {
        asks: false,
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

/// The bridge's own tilolang process on the desktop keeps its token; the bridge's held
/// connection on the laptop, the same person's other device, finds it by its purpose and reads
/// it back unasked, as the bridge kept it itself.
#[tokio::test]
async fn a_secret_an_app_keeps_on_one_device_is_its_own_on_the_persons_other_device() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let laptop = device_cert(
        &SigningKey::from_bytes(&[1; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let (desktop, laptop, created, invitation) = shared_with(one.path(), "Kept", 61, laptop).await;
    let laptop = member_service(two.path(), laptop).await;
    laptop.workspaces().join_member(&invitation).await.unwrap();
    let workspace = created.workspace;

    let keep = request(
        workspace_vault::target(workspace_vault::KEEP),
        InvokeArgument::Json(
            json!({"purpose": "bot-token", "value": TOKEN})
                .as_object()
                .cloned()
                .unwrap(),
        ),
    );
    let kept = &crate::PersonServices::of(desktop.workspaces())
        .keep_for((BRIDGE, &workspace), &AppProcess::OwnTilolang, &keep)
        .await
        .expect("the desktop keeps the bridge's token");

    // The vault is the workspace's: the laptop reads the item once the change reaches it.
    let laptop_workspaces = &crate::PersonServices::of(laptop.workspaces());
    let reading = workspace.clone();
    let found = tilo_test_env::within("the kept item reaches the laptop", async move {
        loop {
            if let Ok(Some(found)) = laptop_workspaces
                .item_tagged(&reading, BRIDGE, "bot-token")
                .await
            {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(found, kept.reference);

    laptop.workspaces().run_connections(Arc::new(Bridge));
    takes_turn_in(
        &crate::PersonServices::of(laptop.workspaces()),
        &workspace,
        &Turn::connection(BRIDGE, Vec::new()),
        Order {
            server_grace: Duration::ZERO,
            other_grace: Duration::ZERO,
        },
    )
    .await
    .expect("the laptop takes the bridge's connection");
    let released = &crate::PersonServices::of(laptop.workspaces())
        .release_to(
            (BRIDGE, &workspace),
            &AppProcess::connection(BRIDGE),
            &request(
                workspace_vault::ItemRef::target(&kept.reference, workspace_vault::RELEASE),
                InvokeArgument::None,
            ),
        )
        .await
        .expect("the bridge's connection on the laptop reads what the bridge kept");
    assert_eq!(released.secret, TOKEN);
    assert!(
        laptop
            .workspaces()
            .asked_store()
            .is_none_or(|store| store.waiting_in(&workspace).is_empty()),
        "nobody was asked"
    );
}

/// The app's own tilolang process both keeps a secret and finds it again on the same device: the
/// Rust host keeps a token it obtained and reads it back later, over the same connection.
#[tokio::test]
async fn an_apps_own_process_finds_the_item_it_kept_and_reads_it_back() {
    let one = tempfile::tempdir().unwrap();
    let laptop = device_cert(
        &SigningKey::from_bytes(&[2; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let (desktop, _laptop, created, _invitation) =
        shared_with(one.path(), "Kept", 62, laptop).await;
    let workspace = created.workspace;
    let keeping = request(
        workspace_vault::target(workspace_vault::KEEP),
        InvokeArgument::Json(
            json!({"purpose": "bot-token", "value": TOKEN})
                .as_object()
                .cloned()
                .unwrap(),
        ),
    );
    let kept = &crate::PersonServices::of(desktop.workspaces())
        .keep_for((BRIDGE, &workspace), &AppProcess::OwnTilolang, &keeping)
        .await
        .expect("the bridge keeps its token");
    let finding = |purpose: &str| {
        request(
            workspace_vault::target(workspace_vault::FIND),
            InvokeArgument::Json(
                json!({"app": BRIDGE, "purpose": purpose})
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
    };
    let found = &crate::PersonServices::of(desktop.workspaces())
        .find_tagged(
            (BRIDGE, &workspace),
            &AppProcess::OwnTilolang,
            &finding("bot-token"),
        )
        .await
        .expect("its own process finds what it kept");
    assert_eq!(found.reference, Some(kept.reference.clone()));
    assert_eq!(
        crate::PersonServices::of(desktop.workspaces())
            .find_tagged(
                (BRIDGE, &workspace),
                &AppProcess::OwnTilolang,
                &finding("nothing-kept")
            )
            .await
            .expect("a purpose it kept nothing for")
            .reference,
        None
    );
    let released = &crate::PersonServices::of(desktop.workspaces())
        .release_to(
            (BRIDGE, &workspace),
            &AppProcess::OwnTilolang,
            &request(
                workspace_vault::ItemRef::target(&kept.reference, workspace_vault::RELEASE),
                InvokeArgument::None,
            ),
        )
        .await
        .expect("its own process reads back what it kept");
    assert_eq!(released.secret, TOKEN);
}
