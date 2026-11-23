//! The workspace's vault, mail accounts and model providers, asked from a
//! computer that holds a person's identity (`tilo login --new-identity`) as
//! that person.
//!
//! The workspace's vault (`workspace/vault`), its mail accounts
//! (`workspace/mail-accounts`) and the model providers its agents run on
//! (`workspace/models`) are kept on the host that runs the workspace,
//! and the host answers them as the workspace's policy decides for whoever
//! asks. A computer that holds the person's root can certify any device of
//! theirs and approve any key, so it asks as their page does: over a session
//! of the device the root certified on it, never marked as an agent's run.
//! The host checks and keeps what it is asked exactly as it does for the
//! page.
//!
//! `tilo do workspace/vault#keep-entry entry='{…}' -w WORKSPACE`, as every
//! other `tilo do`: the action's arguments as `name=value`, its answer
//! printed as the API prints one.
//!
//! Any other action is asked as the person the same way when the command says
//! so (`tilo do --in-person chat/Messages#answer-question …`): what only the
//! person answers themselves, such as a question their agent puts to them, is
//! then answered from this computer. Without it a `tilo do` here acts as the
//! computer's own agent, so a program on it never answers for the person
//! unless it was told to.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    InvokeArgument, ReceiptStatus, RemoteActionTarget, person_accounts, person_models, person_vault,
};
use tilo_actions::tree::{Address, Tree, app_of, pick_action};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_server_protocol::WorkspaceId;
use tilo_session_client::device::Device;

use crate::login;

/// The workspace's mail accounts, as `tilo do` names them.
const MAIL_ACCOUNTS: &str = "workspace/mail-accounts";
/// The workspace's vault, as `tilo do` names it.
const VAULT: &str = "workspace/vault";
/// The workspace's model providers and the model each kind of work runs on, as `tilo do`
/// names them: `workspace/models#add-provider`, `#set-key`, `#add-model`,
/// `#set-assignments` and the rest of [`person_models`].
const MODELS: &str = "workspace/models";

/// The host's action `path` (`workspace/vault#keep-entry`) names, when it names
/// one of the workspace's vault, mail accounts or model providers.
fn target(path: &str) -> Option<RemoteActionTarget> {
    let (resource, action) = path.rsplit_once('#')?;
    match resource {
        // Named as the API names it (`workspace/mail-accounts#add`).
        MAIL_ACCOUNTS if action == "add" => {
            Some(person_accounts::target(person_accounts::ADD_MAIL))
        }
        MAIL_ACCOUNTS => Some(person_accounts::target(action)),
        VAULT => Some(person_vault::target(action)),
        MODELS => Some(person_models::target(action)),
        _ => None,
    }
}

/// Whether `tilo do path` is asked here as the person: `path` is one of
/// these (or the command asked for it `in_person`), no key was named for the
/// command, and this computer holds their root.
pub fn answers(path: &str, keyed: bool, in_person: bool) -> bool {
    !keyed && (in_person || target(path).is_some()) && crate::identity::holds_root()
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

/// `path` (`chat/Messages#answer-question`) done with `args` in `workspace` as the person
/// `device` is: the path resolved as an agent's `tilo_do` resolves it, in what the workspace
/// offers the person, and the action followed until it ends.
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
    let descriptor =
        pick_action(action, &resolved.actions, &resolved.path).map_err(|why| anyhow!("{why}"))?;
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
    let host = target(path);
    let person = crate::identity::held()?
        .ok_or_else(|| anyhow!("this computer holds no identity: tilo login --new-identity"))?;
    // The person is reached where this computer's login reaches them: the server it entered
    // through, signed in as the device their root certified here.
    let Some((_, here)) = login::load(&login::Place::here(crate::identity::paths())?)? else {
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
                .personal_list(
                    &tilo_server_session::workspaces::WorkspaceLabels::new(),
                    None,
                )
                .await
                .map_err(|refusal| anyhow!("{refusal}"))?;
            let (id, _) = login::choose_workspace(&listed.workspaces, Some(workspace))?;
            let entered = device
                .enter_workspace(&id, &person.name)
                .await
                .map_err(|refusal| anyhow!("{refusal}"))?;
            match host {
                Some(target) => device
                    .ask_host(entered, target, args)
                    .await
                    .map_err(|refusal| anyhow!("{path}: {refusal}")),
                None => in_person(&device, entered, path, args).await,
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_persons_own_paths_are_asked_as_them() {
        assert_eq!(
            target("workspace/vault#keep-entry"),
            Some(person_vault::target(person_vault::KEEP_ENTRY))
        );
        assert_eq!(
            target("workspace/mail-accounts#add"),
            Some(person_accounts::target(person_accounts::ADD_MAIL))
        );
        assert_eq!(
            target("workspace/models#add-provider"),
            Some(person_models::target(person_models::ADD_PROVIDER))
        );
        assert_eq!(target("workspace/vault"), None);
        assert_eq!(target("notes/Notes#new-note"), None);
        assert!(!answers("notes/Notes#new-note", false, false));
        assert!(!answers("workspace/vault#entries", true, false));
        assert!(!answers("chat/Messages#answer-question", true, true));
    }
}
