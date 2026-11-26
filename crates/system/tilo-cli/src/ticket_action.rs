//! The local device actions a ticket's CLI calls while signing as this computer.
//! The key-holding process stays undumpable. A fresh copy of the CLI makes the
//! local socket call, subject to the daemon's ordinary account and namespace checks.

use std::io::{Read as _, Write as _};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tilo_api::daemon::DaemonPlace;
use tilo_device_identity::login::{CertifiedAction, Direction, RESOURCE, Report, Ticket};
use tilo_user_identity::DeviceCert;

use crate::{api::Client, login};

const INPUT_LIMIT: usize = 64 * 1024;

pub(crate) fn parse(name: &str) -> Result<CertifiedAction, &'static str> {
    CertifiedAction::at(RESOURCE, name).ok_or("unknown certified device ticket action")
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Waiting {
    ticket: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    ticket: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cert: Option<DeviceCert>,
    label: String,
}

fn arguments<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<Value> {
    let request: T =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("invalid device ticket action input"))?;
    Ok(serde_json::to_value(request)?)
}

/// Run the same CLI, giving it the existing typed action arguments over a private pipe.
pub(crate) fn call(action: CertifiedAction, arguments: &impl Serialize) -> Result<Value> {
    let input = serde_json::to_vec(arguments)?;
    if input.len() > INPUT_LIMIT {
        bail!("device ticket action input is too large");
    }
    let mut child = Command::new(std::env::current_exe()?)
        .args(["device", "ticket-action", action.name()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting this computer's ticket action")?;
    let written = child
        .stdin
        .take()
        .context("opening the ticket action's input")
        .and_then(|mut pipe| pipe.write_all(&input).context("writing the ticket action"));
    let output = child
        .wait_with_output()
        .context("waiting for the ticket action")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    written?;
    serde_json::from_slice(&output.stdout).context("reading the ticket action's reply")
}

/// The child reads the public login to select the device and workspace that own the action.
pub(crate) fn serve(action: CertifiedAction) -> Result<ExitCode> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(INPUT_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > INPUT_LIMIT {
        bail!("device ticket action input is too large");
    }
    let (argument, direction, title) = match action {
        CertifiedAction::Reported => (
            arguments::<Report>(&bytes)?,
            Direction::Login,
            "Report a login's step",
        ),
        CertifiedAction::Pending => (
            arguments::<Waiting>(&bytes)?,
            Direction::SignIn,
            "Sign a browser in",
        ),
        CertifiedAction::Approve => (
            arguments::<Approval>(&bytes)?,
            Direction::SignIn,
            "Approve a browser's sign-in",
        ),
    };
    let text = argument["ticket"].as_str().context("a ticket is needed")?;
    let ticket = Ticket::parse(text).map_err(|_| anyhow!("invalid device ticket"))?;
    if ticket.direction != direction {
        bail!("that ticket is for another device action");
    }
    let place = login::Place::here(crate::identity::paths())?;
    let here = login::load_public(&place)?.ok_or_else(|| anyhow!(login::NOT_LOGGED_IN))?;
    let daemon =
        DaemonPlace::of(place.dir(), &here.cert.device_public_key).map_err(anyhow::Error::msg)?;
    let client = Client::session(daemon.api());
    let reply = login::runtime()?
        .block_on(client.act(&here.workspace, RESOURCE, action.name(), &argument, title))
        .map_err(|error| {
            anyhow!(
                format!("{error:#}")
                    .replace(text, "[ticket]")
                    .replace(&ticket.secret_hex(), "[secret]")
            )
        })?;
    let answer = match action {
        CertifiedAction::Pending => {
            let result = &reply["result"];
            let device = result["device"]
                .as_str()
                .context("this computer's daemon answered no browser")?;
            json!({"device": device, "label": result["label"], "capability": result["capability"]})
        }
        CertifiedAction::Reported | CertifiedAction::Approve => json!({}),
    };
    serde_json::to_writer(std::io::stdout().lock(), &answer)?;
    Ok(ExitCode::SUCCESS)
}
