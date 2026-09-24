//! Pure retained-data validation. No function here issues an execution capability.
use super::*;
use qos_agent_control_protocol::{AgentRequest, canonical_request, decode_request};
use std::collections::BTreeSet;

/// Current agent package id, plus the retired ids of the same first-party
/// family retained in private runtime facts persisted before the rename.
fn is_agent_app(id: &str) -> bool {
    collab_types::first_party_app_ids::lookup_compatible_first_party_app_ids(id, AGENT_APP_ID)
}

/// Agent package id written into every new private runtime fact.
const AGENT_APP_ID: &str = "so.tilo.agent";

pub(super) fn encoded_size(value: &impl Serialize) -> Result<usize, String> {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("private size overflow"))?;
            if self.0 > MAX_PRIVATE_AGENT_LEDGER_BYTES {
                return Err(std::io::Error::other("private ledger budget exceeded"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value).map_err(|e| e.to_string())?;
    Ok(count.0)
}
fn label(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        return Err("invalid runtime label".into());
    }
    Ok(())
}
fn opaque(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value.is_ascii()
        || value.trim() != value
        || value.bytes().any(|b| b.is_ascii_control())
    {
        return Err("invalid retained grant identity".into());
    }
    Ok(())
}
impl AgentIntentV2 {
    pub fn validate(&self, room: &str) -> Result<AgentRequest, String> {
        exact_uuid(room)?;
        exact_uuid(&self.scope.host)?;
        if self.scope.room != room
            || self.scope.machine.is_empty()
            || self.scope.caller.capabilities.can_act_as_host
        {
            return Err("invalid retained invocation scope".into());
        }
        tilo_user_identity::verify_device_cert(&self.scope.caller).map_err(|e| e.to_string())?;
        exact_hash(&self.descriptor_revision)?;
        let r = &self.route;
        let peer = r
            .app_peer
            .parse::<u64>()
            .map_err(|_| "invalid provider peer")?;
        if peer == 0
            || peer.to_string() != r.app_peer
            || !is_agent_app(&r.app_id)
            || r.cluster_id != room
            || !r.record_id.is_empty()
            || r.resource != format!("workspace:{room}:agents")
            || r.resource_kind != "workspace.agents"
        {
            return Err("invalid registered agent route".into());
        }
        let decoded = decode_request(self.request.as_bytes()).map_err(|e| e.to_string())?;
        if canonical_request(&decoded).map_err(|e| e.to_string())? != self.request.as_bytes()
            || !matches!(
                (&decoded, r.action.as_str()),
                (AgentRequest::Spawn { .. }, "spawn") | (AgentRequest::Message { .. }, "message")
            )
        {
            return Err("noncanonical or mismatched agent mutation intent".into());
        }
        if self.grants.is_empty() || self.grants.len() > 64 {
            return Err("invalid retained grant count".into());
        }
        let mut grants = BTreeSet::new();
        for g in &self.grants {
            for value in [&g.app_id, &g.record_id, &g.action] {
                opaque(value)?;
            }
            qos_agent_control_protocol::validate_grant_resource(&g.resource)
                .map_err(|error| error.to_string())?;
            if !grants.insert((&g.app_id, &g.record_id, &g.resource, &g.action)) {
                return Err("duplicate retained grant".into());
            }
        }
        Ok(decoded)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Admitted,
    Launching,
    Bound,
    Dispatching,
    Completed,
    Failed,
    Unknown,
}
pub(super) struct Operation<'a> {
    ids: &'a AgentExecutionIdsV2,
    intent: &'a AgentIntentV2,
    parent: Option<&'a str>,
    phase: Phase,
    credit: [usize; 3],
    task_finished: bool,
    task_unknown: bool,
    exited: bool,
    facts: usize,
    /// Steps bound so far; the last one is `open` until its result commits.
    steps: u32,
    open: bool,
}
impl Operation<'_> {
    fn spawn(&self) -> bool {
        self.parent.is_none()
    }
    fn terminal(&self) -> bool {
        matches!(self.phase, Phase::Completed | Phase::Failed)
    }
}
fn runtime(runtime: &ManagedRuntimeFactsV2) -> Result<(), String> {
    exact_uuid(&runtime.runtime_epoch)?;
    if !is_agent_app(&runtime.package_id) {
        return Err("wrong managed package".into());
    }
    for h in [
        &runtime.package_hash,
        &runtime.executable_hash,
        &runtime.runtime_hash,
    ] {
        exact_hash(h)?;
    }
    Ok(())
}
fn process_identity(p: &ProcessIdentityV2) -> Result<(), String> {
    if p.pid == 0 || p.start == 0 {
        return Err("invalid kernel process identity".into());
    }
    Ok(())
}
fn binding(
    binding: &ManagedBindingFactsV2,
    room: &str,
    ids: &AgentExecutionIdsV2,
    generation: u64,
) -> Result<(), String> {
    if binding.room != room
        || !is_agent_app(&binding.app)
        || binding.agent != ids.agent_id
        || binding.task != ids.task_id
        || binding.run != ids.run_id
        || binding.launch != ids.launch_id
        || binding.generation != generation
    {
        return Err("managed launch binding differs from admission".into());
    }
    Ok(())
}
fn prior<'a>(
    ledger: &'a PrivateAgentLedgerV2,
    key: &str,
    before: u64,
) -> Result<&'a AgentRuntimeFactReceipt, String> {
    let receipt = ledger
        .facts
        .get(key)
        .ok_or("referenced runtime fact missing")?;
    if receipt.generation >= before {
        return Err("runtime fact reference is not prior".into());
    }
    Ok(receipt)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u16,
    message_id: String,
    task_id: String,
    run_id: String,
    launch_id: String,
    runtime_epoch: String,
    recipient_agent: String,
    channel_lease: String,
    task: Task,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    version: u16,
    task_id: String,
    title: String,
    run_id: u64,
    lease: String,
    planning: String,
    action_context: Value,
}
// Owner-neutral strict shape of tilo_actions::ActionContext. Decode separately
// from the retained Value so accepted optional/default field spelling and exact
// canonical envelope bytes remain unchanged. No live authority comes from it.
#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ActionContext {
    action_label: String,
    app: Option<ActionApp>,
    resource: Option<ActionGroup>,
    purpose: Option<String>,
    group: Option<ActionGroup>,
    facets: Vec<ActionFacet>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionApp {
    id: String,
    label: String,
}
// ActionResource and ActionGroup have the same three string fields.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionGroup {
    id: String,
    kind: String,
    label: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionFacet {
    key: String,
    value: String,
}
fn envelope(
    bytes: &str,
    op: &Operation<'_>,
    process: &ChannelProcessFactsV2,
) -> Result<Envelope, String> {
    if bytes.len() > 128 * 1024 {
        return Err("enrolled frame exceeds budget".into());
    }
    let mut decoder = serde_json::Deserializer::from_str(bytes);
    let raw = Unique::deserialize(&mut decoder)
        .map_err(|e| e.to_string())?
        .0;
    decoder.end().map_err(|e| e.to_string())?;
    let e: Envelope = serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
    let text = match decode_request(op.intent.request.as_bytes()).map_err(|e| e.to_string())? {
        AgentRequest::Spawn { task, .. } => task,
        AgentRequest::Message { text, .. } => text,
        _ => return Err("not an executable task".into()),
    };
    let ctx: ActionContext = serde_json::from_value(e.task.action_context.clone())
        .map_err(|e| format!("invalid private task context: {e}"))?;
    let group = ctx.group.as_ref().ok_or("private task group missing")?;
    if e.version != 1
        || e.task.version != 1
        || e.message_id != op.ids.message_id
        || e.task_id != op.ids.task_id
        || e.run_id != op.ids.run_id
        || e.launch_id != op.ids.launch_id
        || e.runtime_epoch != process.runtime.runtime_epoch
        || e.recipient_agent != op.ids.agent_id
        || e.channel_lease != process.channel_id
        || e.task.task_id != op.ids.task_id
        || e.task.run_id == 0
        || e.task.title != text
        || e.task.planning != "model"
        || ctx.purpose.is_some()
        || group.kind != "task"
        || group.id != op.ids.task_id
        || group.label != op.ids.task_id
        || serde_json::to_value(&e).map_err(|e| e.to_string())? != raw
        || serde_json::to_string(&e).map_err(|e| e.to_string())? != bytes
    {
        return Err("enrolled dispatch does not bind exact private task".into());
    }
    label(&e.task.lease)?;
    Ok(e)
}
fn accepts(a: &RecipientAcceptanceV2, e: &Envelope) -> bool {
    a.version == 1
        && (1..=1024).contains(&a.acceptance_sequence)
        && a.message_id == e.message_id
        && a.task_id == e.task_id
        && a.run_id == e.run_id
        && a.launch_id == e.launch_id
        && a.runtime_epoch == e.runtime_epoch
        && a.recipient_agent == e.recipient_agent
        && a.channel_lease == e.channel_lease
}

pub(super) fn validate(
    ledger: &PrivateAgentLedgerV2,
    room: &str,
    generation: u64,
) -> Result<(), String> {
    if ledger.version != 2 || ledger.writer_floor != 2 {
        return Err("unsupported private executor writer floor".into());
    }
    ledger.legacy_v1.validate(room, generation)?;
    if ledger.facts.len()
        > MAX_PRIVATE_AGENT_ADMISSIONS
            * (MAX_AGENT_RUNTIME_FACTS + 2 * MAX_AGENT_TASK_STEPS as usize)
    {
        return Err("private runtime fact count exhausted".into());
    }
    let legacy_last = ledger
        .legacy_v1
        .mutations
        .values()
        .map(|r| r.generation)
        .max()
        .unwrap_or(0);
    let mut ordered = ledger.facts.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|(_, r)| r.generation);
    let mut previous = legacy_last;
    let mut operations: BTreeMap<&str, Operation<'_>> = BTreeMap::new();
    let mut tasks = ledger
        .legacy_v1
        .records
        .values()
        .map(|r| r.ids.task.as_str())
        .collect::<BTreeSet<_>>();
    let mut runs = ledger
        .legacy_v1
        .records
        .values()
        .map(|r| r.ids.run.as_str())
        .collect::<BTreeSet<_>>();
    let mut messages = BTreeSet::new();
    let mut agents = ledger
        .legacy_v1
        .records
        .values()
        .map(|r| r.ids.agent.as_str())
        .collect::<BTreeSet<_>>();
    let mut launches = BTreeSet::new();
    let mut channels = BTreeSet::new();
    let mut exited_launches = BTreeSet::new();
    let mut recipient_budget: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut transport_runs = BTreeSet::new();
    let mut task_leases = BTreeSet::new();
    let mut acceptance_sequences: BTreeMap<String, u64> = BTreeMap::new();
    let mut step_children = BTreeSet::new();
    let legacy_seats = ledger
        .legacy_v1
        .records
        .values()
        .filter(|r| !r.phase.terminal())
        .count();
    for (key, receipt) in ordered {
        let c = &receipt.change;
        c.validate()?;
        if key != &c.mutation
            || c.expected_generation.checked_add(1) != Some(receipt.generation)
            || receipt.generation <= previous
            || receipt.generation > generation
        {
            return Err("invalid runtime receipt generation/order".into());
        }
        previous = receipt.generation;
        let request = OwnerCommitRequest {
            room: room.into(),
            operation: CommitOperation::AgentRuntime {
                change: Box::new(c.clone()),
            },
        };
        if receipt.binding != request.binding()? || encoded_size(receipt)? > c.fact.receipt_limit()
        {
            return Err("runtime receipt binding/size differs".into());
        }
        if let AgentRuntimeFact::Admitted {
            ids,
            intent,
            parent_invocation,
            reservation,
            ..
        } = &c.fact
        {
            let request = intent.validate(room)?;
            for id in [
                &ids.agent_id,
                &ids.task_id,
                &ids.run_id,
                &ids.launch_id,
                &ids.message_id,
            ] {
                exact_uuid(id)?;
            }
            if operations.contains_key(c.invocation.as_str())
                || ledger.legacy_v1.records.contains_key(&c.invocation)
                || ledger
                    .legacy_v1
                    .records
                    .values()
                    .any(|r| r.ids.invocation == c.invocation)
                || !tasks.insert(&ids.task_id)
                || !runs.insert(&ids.run_id)
                || !messages.insert(&ids.message_id)
                || operations.len() + ledger.legacy_v1.records.len() >= MAX_PRIVATE_AGENT_ADMISSIONS
            {
                return Err("private runtime identity/capacity conflict".into());
            }
            let spawn = matches!(request, AgentRequest::Spawn { .. });
            if *reservation != AgentReservationV2::new(spawn) {
                return Err("invalid retained terminal reservation".into());
            }
            if spawn {
                if parent_invocation.is_some()
                    || !agents.insert(&ids.agent_id)
                    || !launches.insert(&ids.launch_id)
                    || legacy_seats
                        + operations
                            .values()
                            .filter(|o| o.spawn() && !o.exited && o.phase != Phase::Failed)
                            .count()
                        >= 4
                {
                    return Err("invalid spawn parent/identity or process capacity".into());
                }
            } else {
                let parent_id = parent_invocation
                    .as_deref()
                    .ok_or("message parent missing")?;
                let parent = operations.get(parent_id).ok_or("message parent absent")?;
                if !parent.spawn()
                    || parent.phase != Phase::Completed
                    || parent.exited
                    || parent.ids.agent_id != ids.agent_id
                    || parent.ids.launch_id != ids.launch_id
                    || parent.intent.scope != intent.scope
                    || parent.intent.grants != intent.grants
                    || !matches!(request, AgentRequest::Message { ref agent_id, .. } if agent_id == &ids.agent_id)
                    || operations.values().any(|o| {
                        o.ids.agent_id == ids.agent_id
                            && !o.task_finished
                            && o.phase != Phase::Failed
                    })
                {
                    return Err("message does not bind an available existing recipient".into());
                }
            }
            operations.insert(
                &c.invocation,
                Operation {
                    ids,
                    intent,
                    parent: parent_invocation.as_deref(),
                    phase: Phase::Admitted,
                    credit: [
                        reservation.control_bytes as usize,
                        reservation.task_bytes as usize,
                        reservation.process_bytes as usize,
                    ],
                    task_finished: false,
                    task_unknown: false,
                    exited: false,
                    facts: 1,
                    steps: 0,
                    open: false,
                },
            );
            continue;
        }
        let op = operations
            .get_mut(c.invocation.as_str())
            .ok_or("runtime admission missing")?;
        if c.fact.is_task_step() {
            // Step facts have their own per-task bound and are covered by the
            // global retained budget below; they never consume the reserved
            // singleton slots or credits of later lifecycle facts.
            step_fact(ledger, key, receipt, op, &mut step_children)?;
            continue;
        }
        op.facts += 1;
        if op.facts > MAX_AGENT_RUNTIME_FACTS {
            return Err("runtime fact slots exhausted".into());
        }
        let category = match c.fact {
            AgentRuntimeFact::TaskUnknown { .. } | AgentRuntimeFact::TaskFinished { .. } => 1,
            AgentRuntimeFact::ProcessExited { .. } => 2,
            _ => 0,
        };
        // A singleton map minus one byte includes worst-case key/colon/comma overhead.
        let charge = encoded_size(&BTreeMap::from([(key, receipt)]))?
            .checked_sub(1)
            .ok_or("invalid map charge")?;
        op.credit[category] = op.credit[category]
            .checked_sub(charge)
            .ok_or("reserved runtime credit exhausted")?;
        match &c.fact {
            AgentRuntimeFact::LaunchIntended {
                binding: b,
                runtime: r,
            } => {
                if !op.spawn() || op.phase != Phase::Admitted {
                    return Err("invalid launch transition".into());
                }
                binding(b, room, op.ids, receipt.generation)?;
                runtime(r)?;
                op.phase = Phase::Launching;
            }
            AgentRuntimeFact::ProcessBound {
                launch_fact,
                process,
            } => {
                if op.phase != Phase::Launching || op.exited {
                    return Err("invalid process binding transition".into());
                }
                let launch = prior(ledger, launch_fact, receipt.generation)?;
                let AgentRuntimeFact::LaunchIntended { binding, runtime } = &launch.change.fact
                else {
                    return Err("process launch reference differs".into());
                };
                if launch.change.invocation != c.invocation
                    || process.binding != *binding
                    || process.runtime != *runtime
                    || !channels.insert(&process.channel_id)
                {
                    return Err("process binding lost exact launch/runtime/channel".into());
                }
                exact_uuid(&process.channel_id)?;
                process_identity(&process.process)?;
                process_identity(&process.supervisor)?;
                op.phase = Phase::Bound;
            }
            AgentRuntimeFact::DispatchIntended {
                process_fact,
                envelope: bytes,
            } => {
                if op.phase
                    != if op.spawn() {
                        Phase::Bound
                    } else {
                        Phase::Admitted
                    }
                    || op.exited
                    || exited_launches.contains(op.ids.launch_id.as_str())
                {
                    return Err("invalid dispatch transition".into());
                }
                let p = prior(ledger, process_fact, receipt.generation)?;
                let AgentRuntimeFact::ProcessBound { process, .. } = &p.change.fact else {
                    return Err("dispatch process reference differs".into());
                };
                if p.change.invocation != op.parent.unwrap_or(&c.invocation) {
                    return Err("dispatch changed parent launch".into());
                }
                let e = envelope(bytes, op, process)?;
                let budget = recipient_budget.entry(process_fact).or_default();
                budget.0 += 1;
                budget.1 = budget
                    .1
                    .checked_add(bytes.len())
                    .ok_or("recipient byte overflow")?;
                if budget.0 > 1024
                    || budget.1 > 8 * 1024 * 1024
                    || !transport_runs.insert((process_fact, e.task.run_id))
                    || !task_leases.insert((process_fact, e.task.lease))
                {
                    return Err("recipient retained capacity/lease conflict".into());
                }
                op.phase = Phase::Dispatching;
            }
            AgentRuntimeFact::Completed {
                dispatch_fact,
                acceptance,
                ..
            } => {
                if !matches!(op.phase, Phase::Dispatching | Phase::Unknown)
                    || op.exited
                    || exited_launches.contains(op.ids.launch_id.as_str())
                {
                    return Err("invalid qualified completion transition".into());
                }
                let d = prior(ledger, dispatch_fact, receipt.generation)?;
                let AgentRuntimeFact::DispatchIntended {
                    process_fact,
                    envelope: bytes,
                } = &d.change.fact
                else {
                    return Err("completion dispatch reference differs".into());
                };
                if d.change.invocation != c.invocation {
                    return Err("completion changed invocation".into());
                }
                let p = prior(ledger, process_fact, d.generation)?;
                let AgentRuntimeFact::ProcessBound { process, .. } = &p.change.fact else {
                    return Err("completion process reference differs".into());
                };
                let e = envelope(bytes, op, process)?;
                let previous = acceptance_sequences
                    .entry(acceptance.channel_lease.clone())
                    .or_default();
                // This ledger retains the complete producer history without eviction;
                // missing acceptances cannot be inferred from a later sequence.
                if !accepts(acceptance, &e)
                    || previous.checked_add(1) != Some(acceptance.acceptance_sequence)
                {
                    return Err("recipient acceptance differs or is not next".into());
                }
                *previous = acceptance.acceptance_sequence;
                op.phase = Phase::Completed;
            }
            AgentRuntimeFact::Failed { error, .. } => {
                qos_agent_control_protocol::validate_operation_diagnostic(error)
                    .map_err(|e| e.to_string())?;
                if op.phase != Phase::Admitted {
                    return Err("failure cannot resolve an uncertain effect".into());
                }
                op.phase = Phase::Failed;
            }
            AgentRuntimeFact::Unknown { error, .. } => {
                qos_agent_control_protocol::validate_operation_diagnostic(error)
                    .map_err(|e| e.to_string())?;
                if op.terminal() || op.phase == Phase::Unknown {
                    return Err("uncertainty cannot regress a terminal fact".into());
                }
                op.phase = Phase::Unknown;
            }
            AgentRuntimeFact::TaskUnknown { error, .. } => {
                qos_agent_control_protocol::validate_operation_diagnostic(error)
                    .map_err(|e| e.to_string())?;
                if op.task_finished
                    || op.task_unknown
                    || !matches!(
                        op.phase,
                        Phase::Dispatching | Phase::Completed | Phase::Unknown
                    )
                {
                    return Err("invalid private task uncertainty".into());
                }
                op.task_unknown = true;
            }
            AgentRuntimeFact::TaskFinished {
                completed_fact,
                acceptance_sequence,
                update_sequence,
                outcome,
                text,
                ..
            } => {
                if op.open && *outcome == TaskOutcomeV2::Succeeded {
                    return Err("task cannot succeed with a bound uncommitted step".into());
                }
                let complete = prior(ledger, completed_fact, receipt.generation)?;
                let AgentRuntimeFact::Completed { acceptance, .. } = &complete.change.fact else {
                    return Err("task completion reference differs".into());
                };
                if op.phase != Phase::Completed
                    || op.task_finished
                    || op.exited
                    || exited_launches.contains(op.ids.launch_id.as_str())
                    || complete.change.invocation != c.invocation
                    || *acceptance_sequence != acceptance.acceptance_sequence
                    || *update_sequence == 0
                    || text.is_empty()
                    || text.len() > 32000
                    || text.chars().count() > 8000
                {
                    return Err("invalid private task result".into());
                }
                op.task_finished = true;
            }
            AgentRuntimeFact::ProcessExited {
                launch_fact, exit, ..
            } => {
                if !op.spawn()
                    || op.exited
                    || !matches!(op.phase, Phase::Completed | Phase::Unknown)
                {
                    return Err("invalid process exit history".into());
                }
                let launch = prior(ledger, launch_fact, receipt.generation)?;
                let AgentRuntimeFact::LaunchIntended { binding, runtime } = &launch.change.fact
                else {
                    return Err("exit launch reference differs".into());
                };
                if launch.change.invocation != c.invocation
                    || exit.binding != *binding
                    || exit.runtime != *runtime
                    || !matches!(
                        (exit.exit_code, exit.signal),
                        (Some(0..=255), None) | (None, Some(1..=127))
                    )
                {
                    return Err("exit evidence differs from launch".into());
                }
                process_identity(&exit.supervisor)?;
                if let Some(bound) = ledger.facts.get(&format!("{}:process_bound", c.invocation)) {
                    let AgentRuntimeFact::ProcessBound { process, .. } = &bound.change.fact else {
                        return Err("invalid bound process fact".into());
                    };
                    if process.supervisor != exit.supervisor
                        || bound.generation >= receipt.generation
                    {
                        return Err("exit supervisor differs".into());
                    }
                }
                op.exited = true;
                exited_launches.insert(op.ids.launch_id.as_str());
            }
            AgentRuntimeFact::Admitted { .. } => unreachable!(),
            AgentRuntimeFact::TaskStepBound { .. } | AgentRuntimeFact::TaskStepCommitted { .. } => {
                unreachable!()
            }
        }
    }
    let mut reserved = 0usize;
    for op in operations.values() {
        if !op.terminal() {
            reserved = reserved
                .checked_add(op.credit[0])
                .ok_or("control credit overflow")?;
        }
        if !op.task_finished && op.phase != Phase::Failed {
            reserved = reserved
                .checked_add(op.credit[1])
                .ok_or("task credit overflow")?;
        }
        if op.spawn() && !op.exited && op.phase != Phase::Failed {
            reserved = reserved
                .checked_add(op.credit[2])
                .ok_or("process credit overflow")?;
        }
    }
    if encoded_size(ledger)?
        .checked_add(reserved)
        .is_none_or(|n| n > MAX_PRIVATE_AGENT_LEDGER_BYTES)
    {
        return Err("private ledger retained/reserved budget exhausted".into());
    }
    Ok(())
}

fn lower_hex(value: &str, bytes: usize) -> Result<Vec<u8>, String> {
    if value.len() != bytes * 2
        || !value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err("invalid exact step hex".into());
    }
    hex::decode(value).map_err(|e| e.to_string())
}
fn step_fact<'a>(
    ledger: &'a PrivateAgentLedgerV2,
    key: &'a str,
    receipt: &'a AgentRuntimeFactReceipt,
    op: &mut Operation<'_>,
    children: &mut BTreeSet<&'a str>,
) -> Result<(), String> {
    let c = &receipt.change;
    match &c.fact {
        AgentRuntimeFact::TaskStepBound {
            dispatch_fact,
            step,
            child,
            intent,
        } => {
            if op.phase != Phase::Completed
                || op.task_finished
                || op.task_unknown
                || op.exited
                || op.open
                || *step != op.steps
                || *step >= MAX_AGENT_TASK_STEPS
                || *dispatch_fact != format!("{}:dispatch_intended", c.invocation)
            {
                return Err("invalid task step binding transition".into());
            }
            let dispatch = prior(ledger, dispatch_fact, receipt.generation)?;
            if dispatch.change.invocation != c.invocation
                || !matches!(
                    dispatch.change.fact,
                    AgentRuntimeFact::DispatchIntended { .. }
                )
            {
                return Err("task step dispatch reference differs".into());
            }
            let ticket = lower_hex(child, 48)?;
            if ticket.chunks(16).any(|part| part.iter().all(|b| *b == 0))
                || !children.insert(child.as_str())
            {
                return Err("task step child ticket invalid or reused".into());
            }
            lower_hex(intent, 32)?;
            op.steps += 1;
            op.open = true;
        }
        AgentRuntimeFact::TaskStepCommitted {
            bound_fact,
            step,
            result,
            result_json,
            continuation,
        } => {
            if !op.open
                || op.steps.checked_sub(1) != Some(*step)
                || *bound_fact != format!("{}:task_step_bound:{step}", c.invocation)
            {
                return Err("invalid task step commit transition".into());
            }
            let bound = prior(ledger, bound_fact, receipt.generation)?;
            if bound.change.invocation != c.invocation
                || !matches!(&bound.change.fact, AgentRuntimeFact::TaskStepBound { step: s, .. } if s == step)
            {
                return Err("task step commit reference differs".into());
            }
            lower_hex(result, 32)?;
            if result_json.len() > MAX_AGENT_STEP_RESULT_BYTES
                || continuation.len() > MAX_AGENT_STEP_CONTINUATION_BYTES * 2
                || continuation.len() % 2 != 0
            {
                return Err("task step result or continuation exceeds budget".into());
            }
            let mut decoder = serde_json::Deserializer::from_str(result_json);
            Unique::deserialize(&mut decoder).map_err(|e| e.to_string())?;
            decoder.end().map_err(|e| e.to_string())?;
            lower_hex(continuation, continuation.len() / 2)?;
            op.open = false;
        }
        _ => unreachable!(),
    }
    let _ = key;
    Ok(())
}

#[cfg(test)]
mod agent_app_id_tests {
    #[test]
    fn persisted_agent_ids_resolve_through_the_shared_rename_table() {
        for id in ["so.tilo.agent", "so.tilo.agent", "so.tilo.agent"] {
            assert!(super::is_agent_app(id), "{id}");
        }
        for id in ["so.tilo.chat", "so.tilo.chat", "agent", ""] {
            assert!(!super::is_agent_app(id), "{id}");
        }
    }
}
