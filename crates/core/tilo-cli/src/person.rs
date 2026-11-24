//! `tilo do` on a computer with a person's identity (`tilo login
//! --new-identity`), done as that person when the command says so.
//!
//! The workspace's vault (`workspace/vault`), its mail accounts
//! (`workspace/mail-accounts`) and the model providers its agents run on
//! (`workspace/models`) are ordinary paths of the workspace's tree, like an
//! app's. A computer with the person's root can certify any device of
//! theirs and approve any key, so `--in-person` resolves and invokes a path over
//! a session of the device the root certified on it, never marked as an agent's
//! run: the host checks and keeps what it is asked exactly as it does for the
//! page.
//!
//! `tilo do workspace/vault#add kind=login title=Bank -w WORKSPACE --in-person`, as
//! every other `tilo do`: the action's arguments as `name=value`, its answer
//! printed as the API prints one.
//!
//! Any path is asked as the person the same way when the command says so
//! (`tilo do --in-person invocations/workspace/<invocation>#resolve resolution=approve`):
//! what only the person answers themselves, such as a call of their agent's that
//! waits in its asked record for their yes, is then answered from this computer.
//! Without it a `tilo do` here acts as the computer's own agent, so a program on it
//! never answers for the person unless it was told to.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{InvokeArgument, ReceiptStatus, RemoteActionTarget};
use tilo_actions::tree::{Address, Tree, app_of, pick_action};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_server_protocol::WorkspaceId;
use tilo_session_client::device::Device;

use crate::login;

/// Whether `tilo do path` is asked here as the person: no key was named for the
/// command, it said `in_person`, and this computer has their root.
pub fn answers(keyed: bool, in_person: bool) -> bool {
    !keyed && in_person && crate::identity::holds_root()
}

/// Pauses of the walk that resolves a path, on tokio's clock.
struct Pause(std::time::Instant);

impl tilo_actions::tree::Pause for Pause {
    fn pause(
        &self,
        how_long: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(how_long))
    }

    fn now(&self) -> std::time::Duration {
        self.0.elapsed()
    }
}

/// `path` (`invocations/workspace/<invocation>#resolve`) done with `args` in `workspace`
/// as the person `device` is: the path resolved as an agent's `tilo_do` resolves it, in what
/// the workspace offers the person, and the action followed until it ends.
async fn in_person(
    device: &Device,
    workspace: WorkspaceId,
    path: &str,
    args: Map<String, Value>,
) -> Result<Value> {
    let parsed = Address::parse(path).map_err(|error| anyhow!("{error}"))?;
    let Some(action) = parsed.action.as_deref() else {
        bail!("{path} names no action: write it as <resource>#<action>");
    };
    let tree = Tree::new(
        device.actions(workspace),
        device.caller(),
        workspace.to_string(),
        Arc::new(Pause(std::time::Instant::now())),
    );
    // The app is started first when none runs, as an agent's tilo_do starts it: what a
    // workspace offers of an app that does not run is its name alone.
    let started = tree.started_provider(&app_of(&parsed), None).await;
    let (resolved, _) = match (tree.resolve(&parsed, None).await, started) {
        (Ok(found), _) => found,
        (Err(why), Ok(_)) => bail!("{path}: {why}"),
        (Err(why), Err(not_started)) => bail!("{path}: {why} ({not_started})"),
    };
    let descriptor = match pick_action(action, &resolved.actions, &resolved.path) {
        Ok(descriptor) => descriptor,
        // An action the app declares that the person is not offered is told so, as every
        // surface tells it; a name the app does not declare is no action of it.
        Err(why) => {
            let surface = tilo_mcp::offered::published_surface(&tree, &resolved.app_id);
            let caller = tilo_mcp::offered::caller_named(&device.caller(), None);
            let refusal =
                tilo_mcp::offered::not_offered(&tree, &resolved.app_id, action, &caller, surface)
                    .await;
            bail!(
                "{}",
                refusal.unwrap_or_else(|| format!("unknown_action: {why}"))
            );
        }
    };
    let target = RemoteActionTarget {
        provider: resolved.provider.clone(),
        address: descriptor.address.clone(),
    };
    let called = tree
        .invoke(
            target,
            InvokeArgument::Json(args),
            InvocationId::new(),
            Some(resolved.revision.clone()),
            None,
        )
        .await
        .map_err(|why| anyhow!("{path}: {why}"))?;
    match (&called.receipt.status, called.update()) {
        (ReceiptStatus::Completed { output }, _)
        | (_, Some(InvocationUpdate::Succeeded { output, .. })) => {
            Ok(output.value.clone().unwrap_or_default())
        }
        (ReceiptStatus::Failed { error }, _)
        | (_, Some(InvocationUpdate::Failed { error, .. })) => bail!("{path}: {}", error.message),
        (ReceiptStatus::Rejected { error }, _) => bail!("{path}: {error}"),
        _ => bail!("{path} did not finish while it was waited for"),
    }
}

/// Ask `path` with `args` in `workspace` (its name or id) as the person, on
/// the device their root certified on this computer: the host's answer, as
/// the API shapes one.
///
/// # Errors
/// This computer holds no person, the site or its host could not be reached,
/// the workspace is none of theirs, or the host refused.
pub fn act(workspace: &str, path: &str, args: Map<String, Value>) -> Result<Value> {
    let person = crate::identity::held()?
        .ok_or_else(|| anyhow!("this computer holds no identity: tilo login --new-identity"))?;
    // The person is reached where this computer's login reaches them: the server it entered
    // through, signed in as the device their root certified here.
    let Some(here) = login::home() else {
        bail!("{}", login::NOT_LOGGED_IN);
    };
    if here.cert.user_public_key != person.public_key() {
        bail!("this computer is logged in as someone else than the person whose identity it holds");
    }
    let address = here
        .server_address
        .as_ref()
        .ok_or_else(|| anyhow!(login::LOGIN_ENDED))?;
    let mut signed = login::signed_over_iroh(&here, address)?;
    signed.agent = String::new();
    signed.label.clone_from(&person.cert.device_label);
    signed.cert = person.cert.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let device = Device::connect(&signed, Arc::new(person.device.clone()));
        let answered = async {
            device
                .connected()
                .await
                .map_err(|refusal| anyhow!("{refusal}"))?;
            let listed = device
                .person_workspaces(
                    &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
                    None,
                )
                .await
                .map_err(|refusal| anyhow!("{refusal}"))?;
            let (id, _) = login::choose_workspace(&listed.workspaces, Some(workspace))?;
            let entered = device
                .enter_workspace(&id, &person.name)
                .await
                .map_err(|refusal| anyhow!("{refusal}"))?;
            in_person(&device, entered, path, args).await
        }
        .await;
        device.close();
        answered.map(|result| json!({"status": "ok", "path": path, "result": result}))
    })
}

/// `tilo do path args -w workspace` as the person ([`act`]), said as `tilo do`
/// says an answer.
pub fn run(workspace: &str, path: &str, args: &[String], json: bool) -> std::process::ExitCode {
    crate::log_to_stderr(tracing_subscriber::filter::LevelFilter::OFF);
    let answered = tilo_control::front::arguments(args)
        .map_err(anyhow::Error::msg)
        .and_then(|args| act(workspace, path, args))
        .map_err(|error| format!("tilo: {error:#}"));
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let code = tilo_control::front::say(
        answered,
        json,
        &mut tilo_control::front::Io {
            out: &mut stdout.lock(),
            err: &mut stderr.lock(),
        },
    );
    std::process::ExitCode::from(code)
}
