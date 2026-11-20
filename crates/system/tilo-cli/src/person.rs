//! What only the person does, asked from a computer that holds their identity
//! (`tilo login --new-identity`): their vault and their mail accounts.
//!
//! The person's vault (`person/vault`) and their mail accounts
//! (`person/mail-accounts`) are kept on the host that runs their workspace,
//! and the host answers them to the person on a device of theirs alone: a key
//! made for a script and an agent are refused. Their page asks them over its
//! own session with the host, as their device. A computer that holds the
//! person's root can certify any device of theirs and approve any key, so it
//! asks the same as the page does: over a session of the device the root
//! certified on it, never marked as an agent's run. The host checks and keeps
//! what it is asked exactly as it does for the page.
//!
//! `tilo do person/vault#keep-entry entry='{…}' -w WORKSPACE`, as every other
//! `tilo do`: the action's arguments as `name=value`, its answer printed as
//! the API prints one.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{RemoteActionTarget, person_accounts, person_vault};
use tilo_server_client::https::{self, Origin};
use tilo_server_client::login::{PairingLink, SignIn};
use tilo_session_client::device::Device;

use crate::login;

/// The person's mail accounts, as `tilo do` names them.
const MAIL_ACCOUNTS: &str = "person/mail-accounts";
/// The person's vault, as `tilo do` names it.
const VAULT: &str = "person/vault";

/// The host's action `path` (`person/vault#keep-entry`) names, when it names
/// one of the person's own.
fn target(path: &str) -> Option<RemoteActionTarget> {
    let (resource, action) = path.rsplit_once('#')?;
    match resource {
        // Named as the API names it (`person/mail-accounts#add`).
        MAIL_ACCOUNTS if action == "add" => {
            Some(person_accounts::target(person_accounts::ADD_MAIL))
        }
        MAIL_ACCOUNTS => Some(person_accounts::target(action)),
        VAULT => Some(person_vault::target(action)),
        _ => None,
    }
}

/// Whether `tilo do path` is asked here as the person: `path` is one of
/// theirs, no key was named for the command, and this computer holds their
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
        bail!("{path} is none of the person's own actions: {MAIL_ACCOUNTS}#… or {VAULT}#…");
    };
    let person = crate::identity::held()?
        .ok_or_else(|| anyhow!("this computer holds no identity: tilo login --new-identity"))?;
    login::trust_test_certificates()?;
    let site = login::site_origin(None)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let origin = login::home_server(&Origin::new(&login::http(), &site)).await?;
        let info = https::server_info(&Origin::new(&login::http(), &origin))
            .await
            .map_err(|error| anyhow!("asking {origin} who it is: {error}"))?;
        let server = info
            .address
            .as_ref()
            .map(tilo_server_client::ServerAddress::published)
            .ok_or_else(|| anyhow!("{origin} takes no device connections"))?;
        let link = PairingLink::parse(&format!(
            "{site}/#/@{}/w/{}?pair={}",
            login::host_of(&origin),
            uuid::Uuid::nil(),
            person.user()
        ))
        .map_err(|error| anyhow!(error))?;
        let signed = SignIn {
            link,
            agent: String::new(),
            label: person.cert.device_label.clone(),
            server,
            cert: person.cert.clone(),
            server_cert: info.server_cert.clone(),
        };
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
            target("person/vault#keep-entry"),
            Some(person_vault::target(person_vault::KEEP_ENTRY))
        );
        assert_eq!(
            target("person/mail-accounts#add"),
            Some(person_accounts::target(person_accounts::ADD_MAIL))
        );
        assert_eq!(target("person/vault"), None);
        assert_eq!(target("notes/Notes#new-note"), None);
        assert!(!answers("notes/Notes#new-note", false));
        assert!(!answers("person/vault#entries", true));
    }
}
