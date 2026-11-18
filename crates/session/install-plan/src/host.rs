//! What the person's Servers workspace holds of the machines they planned,
//! asked of the running session: the host actions
//! (`tilo.host`: `machine.plan`, `machine.list`, `machine.show`,
//! `machine.admit`) the command and the installer's window both reach, so one
//! list is read and written in one place.
//!
//! The session's own action API answers them, the same one `tilo do` speaks:
//! this module is only the client half, over the control socket the session
//! serves its person at this device.

use std::path::Path;

use install_profile_model::plan::{Plan, WaitingPlan};
use serde_json::Value;

/// The provider a host answers its own operations under.
const HOST_PROVIDER: &str = "tilo.host";
/// How long a request waits for the session's answer: long enough for a plan
/// written into a workspace that is still synchronising, short of leaving the
/// person hanging.
const WAIT_MS: u64 = 30_000;

/// Plan the machine `plan` describes, under `label`, naming the `token_file`
/// the medium carries and `installer_url` for the command it answers with.
/// Answers what the person is told: the plan's id and how it reaches the
/// machine.
///
/// # Errors
///
/// No session is running here, or it refused the plan.
pub fn plan(
    plan: &Plan,
    label: &str,
    token_file: &Path,
    installer_url: &str,
) -> Result<String, String> {
    let answer = ask(
        "machine.plan",
        &serde_json::json!({
            "plan": serde_json::to_string(plan).map_err(|error| error.to_string())?,
            "label": label,
            "token_file": token_file.display().to_string(),
            "installer_url": installer_url,
        }),
    )?;
    answer
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "the session answered no plan".to_owned())
}

/// The machines the person planned and has not installed, as the Servers
/// workspace keeps them.
///
/// # Errors
///
/// No session is running here, or its Servers workspace could not be read.
pub fn waiting() -> Result<Vec<WaitingPlan>, String> {
    serde_json::from_value(ask("machine.list", &Value::Null)?)
        .map_err(|error| format!("the session's plans do not read: {error}"))
}

/// The planned machine whose plan is `plan`.
///
/// # Errors
///
/// No session is running here, no machine is planned under that id, or its
/// entry does not read back as a plan.
pub fn show(plan: &str) -> Result<WaitingPlan, String> {
    serde_json::from_value(ask("machine.show", &serde_json::json!({ "plan": plan }))?)
        .map_err(|error| format!("the session's plan does not read: {error}"))
}

/// Take the machine whose device key is `device` and whose token nonce is
/// `nonce` into its waiting entry, as the device that approved it does.
///
/// # Errors
///
/// No session is running here, this device holds no Servers workspace, or no
/// plan answers that token.
pub fn admit(device: &str, nonce: &str) -> Result<(), String> {
    ask(
        "machine.admit",
        &serde_json::json!({ "device": device, "nonce": nonce }),
    )
    .map(drop)
}

/// Ask the session the host action `id` with `args`, and answer what it
/// answered: its receipt's value, which is no value for an action that
/// answers none.
fn ask(id: &str, args: &Value) -> Result<Value, String> {
    let sockets = tilo_control::client::sockets_from_env();
    let mut desktop = tilo_control::client::Desktop::connect(&sockets)
        .map_err(|_| "no Tilo session is running here; start it to plan a machine".to_owned())?;
    let request = tilo_control::client::invoke_wire(HOST_PROVIDER, id, args, None, Some(WAIT_MS));
    let answer = desktop
        .request(&request)
        .map_err(|error| format!("the session is not answering: {error:?}"))?;
    if let Some(error) = answer.get("err") {
        let reason = error
            .get("reason")
            .or_else(|| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("the session refused it");
        return Err(reason.to_owned());
    }
    let receipt = answer.get("answer").cloned().unwrap_or_default();
    if let Some(state) = receipt
        .pointer("/receipt/update/state")
        .and_then(Value::as_str)
        && state == "failed"
    {
        let reason = receipt
            .pointer("/receipt/update/error/message")
            .and_then(Value::as_str)
            .unwrap_or("the session did not do it");
        return Err(reason.to_owned());
    }
    Ok(receipt
        .pointer("/receipt/update/output/value")
        .or_else(|| receipt.pointer("/receipt/output/value"))
        .cloned()
        .unwrap_or(Value::Null))
}
