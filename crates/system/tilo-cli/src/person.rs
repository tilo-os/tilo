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

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{RemoteActionTarget, person_accounts, person_models, person_vault};
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
/// these, no key was named for the command, and this computer holds their
/// root.
pub fn answers(path: &str, keyed: bool) -> bool {
    !keyed && target(path).is_some() && crate::identity::holds_root()
}

/// Ask `path` with `args` in `workspace` (its name or id) as the person, on
/// the device their root certified on this computer: the host's answer, as
/// the API shapes one.
///
/// # Errors
/// This computer holds no person, the site or its host could not be reached,
/// the workspace is none of theirs, or the host refused.
pub fn act(workspace: &str, path: &str, args: Map<String, Value>) -> Result<Value> {
    let Some(target) = target(path) else {
        bail!(
            "{path} is none of the person's own actions: {MAIL_ACCOUNTS}#…, {VAULT}#… or {MODELS}#…"
        );
    };
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
            device
                .ask_host(entered, target, args)
                .await
                .map_err(|refusal| anyhow!("{path}: {refusal}"))
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
        assert!(!answers("notes/Notes#new-note", false));
        assert!(!answers("workspace/vault#entries", true));
    }
}
