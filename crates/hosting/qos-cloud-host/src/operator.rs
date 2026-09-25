//! Local filesystem-operator diagnostics, never a browser authority path.
use anyhow::{Result, anyhow, bail, ensure};
use qos_peer_state::owner_commit::{AgentRuntimeFact, PrivateAgentState};
use serde::Serialize;
use std::path::Path;
mod select;
pub use select::select_private_agent_runtime;
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
/// An actual durable host command phase. `completed` means recipient acceptance,
/// not task completion, process exit, or a product/runtime qualification.
pub struct AgentReceipt {
    version: u8,
    invocation_id: String,
    phase: &'static str,
    agent_id: String,
    task_id: String,
    admission_digest: String,
}
/// Read one exact UUID from an already selected private V2 Store, without
/// provisioning, migration, execution, or exposing private invocation bodies.
pub fn agent_receipt(root: &Path, machine: &str, invocation: &str) -> Result<AgentReceipt> {
    let id = uuid::Uuid::parse_str(invocation)
        .map_err(|_| anyhow!("canonical invocation UUID required"))?;
    ensure!(
        !id.is_nil() && id.to_string() == invocation,
        "canonical nonnil invocation UUID required"
    );
    ensure!(root.is_absolute(), "absolute operator Store root required");
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK);
    }
    let directory = options
        .open(root)
        .map_err(|_| anyhow!("operator Store unavailable"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    let store = loop {
        match crate::delivery::access::try_open_held(root, machine, &directory) {
            Ok(store) => break store,
            Err(crate::delivery::access::AccessError::Busy) => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                ensure!(!remaining.is_zero(), "operator Store busy");
                std::thread::sleep(remaining.min(std::time::Duration::from_millis(5)));
            }
            Err(crate::delivery::access::AccessError::Denied(_)) => {
                bail!("operator Store unavailable")
            }
        }
    };
    ensure!(
        store
            .state
            .private_schema_version()
            .map_err(|_| anyhow!("selected private V2 required"))?
            == crate::AGENT_STATE_VERSION,
        "selected private V2 required"
    );
    let Some(PrivateAgentState::V2(ledger)) = &store.state.private_agent_ledger else {
        bail!("selected private V2 required");
    };
    let admission = ledger
        .facts
        .get(&format!("{invocation}:admitted"))
        .ok_or_else(|| anyhow!("invocation admission unavailable"))?;
    let AgentRuntimeFact::Admitted { ids, intent, .. } = &admission.change.fact else {
        bail!("invocation admission unavailable");
    };
    ensure!(
        admission.change.invocation == invocation
            && intent.scope.machine == machine
            && intent.scope.room == store.state.room
            && intent.scope.host == store.state.room,
        "invocation admission scope invalid"
    );
    let phase = ledger
        .facts
        .values()
        .filter(|fact| fact.change.invocation == invocation)
        .filter_map(|fact| command_phase(&fact.change.fact).map(|phase| (fact.generation, phase)))
        .max_by_key(|(generation, _)| *generation)
        .map(|(_, phase)| phase)
        .ok_or_else(|| anyhow!("invocation command phase unavailable"))?;
    ensure!(
        ids.agent_id.len() <= 128 && ids.task_id.len() <= 128,
        "receipt identity exceeds bound"
    );
    let receipt = AgentReceipt {
        version: 1,
        invocation_id: invocation.to_owned(),
        phase,
        agent_id: ids.agent_id.clone(),
        task_id: ids.task_id.clone(),
        admission_digest: hex::encode(admission.binding.request_digest),
    };
    ensure!(
        serde_json::to_vec(&receipt)?.len() <= 1024,
        "operator receipt exceeds bound"
    );
    Ok(receipt)
}
fn command_phase(fact: &AgentRuntimeFact) -> Option<&'static str> {
    Some(match fact {
        AgentRuntimeFact::Admitted { .. } => "admitted",
        AgentRuntimeFact::LaunchIntended { .. } => "launch_intended",
        AgentRuntimeFact::ProcessBound { .. } => "process_bound",
        AgentRuntimeFact::DispatchIntended { .. } => "dispatch_intended",
        AgentRuntimeFact::Completed { .. } => "completed",
        AgentRuntimeFact::Failed { .. } => "failed",
        AgentRuntimeFact::Unknown { .. } => "unknown",
        AgentRuntimeFact::TaskUnknown { .. }
        | AgentRuntimeFact::TaskFinished { .. }
        | AgentRuntimeFact::ProcessExited { .. }
        | AgentRuntimeFact::TaskStepBound { .. }
        | AgentRuntimeFact::TaskStepCommitted { .. } => return None,
    })
}

#[cfg(test)]
mod tests;
