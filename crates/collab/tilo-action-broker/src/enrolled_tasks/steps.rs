//! Native owner step metadata and the existing host-private invocation journal.
use super::{Connection, Context, Run, task_actions};
use crate::collab_broker_capnp::{
    AdmissionProvenance, DurableContext, DurableInvocation, InvocationInputs, InvocationKey,
    PublishedAction, PublishedSurface, durable_local_invocation::LocalCaller,
};
use serde_json::{Value, json};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_desktop_model::{WorkspaceTaskStep, WorkspaceTaskStepOutcome, WorkspaceTaskStepState};
use tilo_peer_state::{
    owner_commit::{CommitOperation, OwnerCommitRequest},
    workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand},
};

pub(in crate::collab_broker_capnp) struct Admission<'a> {
    pub invocation: InvocationId,
    pub intent: Value,
    handle: &'a Context,
    numeric: u64,
    lease: &'a str,
    index: u32,
    offered: (PublishedSurface, PublishedAction),
}
impl Admission<'_> {
    pub(in crate::collab_broker_capnp) fn current(
        &self,
        offered: &(PublishedSurface, PublishedAction),
    ) -> Result<(), String> {
        let (run, _) = task_actions::observed(self.handle, self.numeric, self.lease)?;
        if offered != &self.offered {
            return Err("task step target publication changed".into());
        }
        let step = owner_step(self.handle, &run.envelope.task_id, self.index)?
            .ok_or("task step preparation absent")?;
        if step.invocation != self.invocation
            || step.run_epoch != run.epoch.ok_or("accepted epoch absent")?
            || step.intent_digest != digest(&self.intent)?
            || step.state != WorkspaceTaskStepState::Prepared
        {
            return Err("task step no longer owns its exact prepared admission".into());
        }
        Ok(())
    }
}

pub(in crate::collab_broker_capnp) struct Reply {
    pub wire: u64,
    pub update: String,
    pub continuation: Vec<u8>,
}

fn child(room: &str, task: &str, epoch: u64, index: u32) -> InvocationId {
    // Length-prefixed strings and fixed-width integers, versioned independently
    // of process/channel identities, which must not change this replay key.
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo.native-task-step.child.v1\0");
    for value in [room, task] {
        hash.update(&(value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    hash.update(&epoch.to_le_bytes());
    hash.update(&index.to_le_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    InvocationId(uuid::Uuid::from_bytes(bytes))
}

struct BoundedBytes(Vec<u8>);
impl std::io::Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > 256 * 1024)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task step commitment exceeds the private bound",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn digest(value: &Value) -> Result<[u8; 32], String> {
    // Bound before cloning, and sort recursively even if another dependency
    // enables serde_json's preserve_order feature. Arrays retain their order.
    let mut bytes = BoundedBytes(Vec::new());
    serde_json::to_writer(&mut bytes, value).map_err(|_| "task step commitment exceeds bound")?;
    let mut canonical = value.clone();
    canonical.sort_all_objects();
    bytes.0.clear();
    serde_json::to_writer(&mut bytes, &canonical).map_err(|_| "step commitment encoding failed")?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo.native-task-step.intent.v1\0");
    hash.update(&bytes.0);
    Ok(*hash.finalize().as_bytes())
}

fn owner_step(
    handle: &Context,
    task: &str,
    index: u32,
) -> Result<Option<WorkspaceTaskStep>, String> {
    let (_, connection) = handle.connection()?;
    let snapshot = handle
        .state
        .services
        .workspace_memberships
        .as_ref()
        .ok_or("native owner absent")?
        .task_snapshot(&connection.route.cluster_id)?;
    let control = snapshot
        .controls
        .get(task)
        .ok_or("task execution control absent")?;
    Ok(control
        .steps
        .iter()
        .find(|step| step.run_epoch == control.run_epoch && step.index == index)
        .cloned())
}

fn retained(handle: &Context, key: &InvocationKey) -> Result<Option<DurableInvocation>, String> {
    let guard = handle
        .state
        .journal
        .lock()
        .map_err(|_| "invocation journal unavailable")?;
    guard
        .as_ref()
        .map_err(Clone::clone)?
        .as_ref()
        .ok_or("durable admission unavailable")?
        .lookup(key)
}

fn resolve(
    handle: &Context,
    target: runtime_collab::TaskAction<'_>,
) -> Result<(PublishedSurface, PublishedAction), String> {
    if target
        .argument
        .is_some_and(|argument| argument.len() > 64 * 1024)
    {
        return Err("task step argument exceeds the private bound".into());
    }
    let (_, connection) = handle.connection()?;
    let surface = handle
        .state
        .surfaces
        .lock()
        .map_err(|_| "action surfaces unavailable")?
        .get(&target.peer_id)
        .filter(|surface| surface.route.cluster_id == connection.route.cluster_id)
        .cloned()
        .ok_or("task step target is not published in the same workspace")?;
    let action = surface
        .actions
        .iter()
        .find(|action| action.resource == target.resource && action.action == target.action)
        .cloned()
        .ok_or("task step action no longer published")?;
    if action.contract.confirmation != tilo_actions::ConfirmationPolicy::None {
        return Err("task step requires interactive confirmation".into());
    }
    if action.semantic_identity.is_none() {
        return Err("task step requires a stable semantic resource identity".into());
    }
    Ok((surface, action))
}

struct Request<'a> {
    numeric: u64,
    lease: &'a str,
    index: u32,
    target: runtime_collab::TaskAction<'a>,
}
struct Plan {
    run: Run,
    connection: Connection,
    epoch: u64,
    invocation: InvocationId,
    offered: (PublishedSurface, PublishedAction),
    core: Value,
    key: InvocationKey,
    cancelled: bool,
}
struct Prepared {
    plan: Plan,
    wire: u64,
    intent_digest: [u8; 32],
}

/// The authenticated caller is captured before awaiting; no actor comes from args.
pub(in crate::collab_broker_capnp) async fn invoke(
    caller: LocalCaller,
    handle: &Context,
    numeric: u64,
    lease: &str,
    index: u32,
    target: runtime_collab::TaskAction<'_>,
) -> Result<Reply, String> {
    let request = Request {
        numeric,
        lease,
        index,
        target,
    };
    let prepared = prepare(&caller, handle, &request).await?;
    wait(handle, &prepared.plan.key, prepared.wire, numeric, lease).await?;
    let ordered = handle.state.owner_commit_order.lock().await;
    task_actions::observed(handle, numeric, lease)?;
    let record = retained(handle, &prepared.plan.key)?.ok_or("retained terminal absent")?;
    let reply = settlement(&record)?;
    commit_step(
        handle,
        &prepared.plan.connection.route.cluster_id,
        WorkspaceTaskAuthority::Host {
            attestor: prepared.plan.connection.certificate,
        },
        WorkspaceTaskCommand::CommitStep {
            task_id: prepared.plan.run.envelope.task_id,
            run_epoch: prepared.plan.epoch,
            index,
            invocation: prepared.plan.invocation,
            intent_digest: prepared.intent_digest,
            outcome: reply.0,
            output_digest: reply.1,
        },
    )
    .await?;
    drop(ordered);
    Ok(Reply {
        wire: prepared.wire,
        update: serde_json::to_string(&record.update).map_err(|_| "terminal encoding failed")?,
        continuation: reply.2,
    })
}

fn plan(caller: &LocalCaller, handle: &Context, request: &Request<'_>) -> Result<Plan, String> {
    let (run, cancelled) = task_actions::observed(handle, request.numeric, request.lease)?;
    let (_, connection) = handle.connection()?;
    let epoch = run.epoch.ok_or("accepted native epoch absent")?;
    let invocation = child(
        &connection.route.cluster_id,
        &run.envelope.task_id,
        epoch,
        request.index,
    );
    let offered = resolve(handle, request.target)?;
    let actor = caller.admitted_in().1.broker_actor();
    let now = handle.state.policy_now_ns()? / 1_000_000_000;
    handle.state.authorize_invocation_permission(
        &connection.route.cluster_id,
        &offered.0.app_id,
        &offered.1.action,
        offered.1.contract.permission,
        &actor,
        now,
    )?;
    let core = json!({"version":1,"task":run.envelope.task_id,"run":run.envelope.run_id,
        "epoch":epoch,"index":request.index,"source":{
            "route":{"app":connection.route.app_id,"room":connection.route.cluster_id,
                "record":connection.route.record_id},"owner":caller.owner()},
        "target":{"route":{"app":offered.0.route.app_id,"room":offered.0.route.cluster_id,
            "record":offered.0.route.record_id},"app":offered.0.app_id,
            "identity":offered.1.semantic_identity,"action":offered.1.action,
            "contract":offered.1.contract,"params":offered.1.params,
            "operation_kind":offered.1.operation_kind},
        "argument":request.target.argument,"context":run.envelope.task.action_context});
    let key = InvocationKey {
        room: connection.route.cluster_id.clone(),
        host: handle.state.invocation_host()?,
        owner: caller.owner(),
        invocation,
    };
    Ok(Plan {
        run,
        connection,
        epoch,
        invocation,
        offered,
        core,
        key,
        cancelled,
    })
}

fn intent(
    plan: &Plan,
    handle: &Context,
    previous: Option<&DurableInvocation>,
    prepared: Option<&WorkspaceTaskStep>,
) -> Result<Value, String> {
    if let Some(record) = previous {
        let request = record
            .intent
            .get("request")
            .ok_or("retained step intent absent")?;
        if request.get("core") != Some(&plan.core) {
            return Err(
                "task step replay changed its canonical caller, target or arguments".into(),
            );
        }
        Ok(request.clone())
    } else {
        if prepared.is_some() {
            return Err(
                "task step prepared without a retained dispatch record; outcome uncertain".into(),
            );
        }
        if plan.cancelled {
            return Err("cancelled task cannot admit a new step".into());
        }
        let facts = plan.connection.process.facts()?;
        Ok(json!({"core":plan.core,"original":{
            "scope":plan.run.scope,"peer":handle.peer,"launch":facts.binding.launch,
            "runtime_epoch":facts.runtime.runtime_epoch,"channel":facts.channel_id,
            "package_id":facts.runtime.package_id,"package_hash":facts.runtime.package_hash,
            "executable_hash":facts.runtime.executable_hash,"runtime_hash":facts.runtime.runtime_hash,
            "process":{"pid":facts.process.pid,"start":facts.process.start},
            "supervisor":{"pid":facts.supervisor.pid,"start":facts.supervisor.start}}}))
    }
}

async fn prepare(
    caller: &LocalCaller,
    handle: &Context,
    request: &Request<'_>,
) -> Result<Prepared, String> {
    let ordered = handle.state.owner_commit_order.lock().await;
    let plan = plan(caller, handle, request)?;
    let previous = retained(handle, &plan.key)?;
    let prepared = owner_step(handle, &plan.run.envelope.task_id, request.index)?;
    let intent = intent(&plan, handle, previous.as_ref(), prepared.as_ref())?;
    let intent_digest = digest(&intent)?;
    if let Some(step) = &prepared {
        if step.invocation != plan.invocation || step.intent_digest != intent_digest {
            return Err("retained private intent does not match owner step commitment".into());
        }
    } else {
        if previous.is_some() {
            return Err("dispatch record has no matching durable owner preparation".into());
        }
        commit_step(
            handle,
            &plan.connection.route.cluster_id,
            WorkspaceTaskAuthority::Host {
                attestor: plan.connection.certificate.clone(),
            },
            WorkspaceTaskCommand::PrepareStep {
                task_id: plan.run.envelope.task_id.clone(),
                run_epoch: plan.epoch,
                index: request.index,
                invocation: plan.invocation,
                intent_digest,
            },
        )
        .await?;
    }
    let wire = if let Some(record) = previous {
        if record.uncertain {
            return Err("task step dispatch outcome is uncertain; it cannot be repeated".into());
        }
        record.wire_id
    } else {
        dispatch(caller, handle, request, &plan, intent)?
    };
    drop(ordered);
    Ok(Prepared {
        plan,
        wire,
        intent_digest,
    })
}

fn dispatch(
    caller: &LocalCaller,
    handle: &Context,
    request: &Request<'_>,
    plan: &Plan,
    intent: Value,
) -> Result<u64, String> {
    let admission = Admission {
        invocation: plan.invocation,
        intent,
        handle,
        numeric: request.numeric,
        lease: request.lease,
        index: request.index,
        offered: plan.offered.clone(),
    };
    let context = serde_json::from_value(plan.run.envelope.task.action_context.clone())
        .map_err(|_| "retained task context invalid")?;
    let actor = caller.admitted_in().1.broker_actor();
    let guard = handle
        .state
        .admission_lock
        .lock()
        .map_err(|_| "invocation admission unavailable")?;
    let job = handle.state.dispatch_invocation_from_offered_owned(
        request.target.peer_id,
        plan.offered.clone(),
        InvocationInputs {
            requested: Some(plan.invocation),
            caller: None,
            execution: None,
            argument: request.target.argument,
            actor: &actor,
            confirmation_token: None,
            context: Some(context),
        },
        AdmissionProvenance {
            durable: Some(DurableContext::Enrolled {
                caller,
                step: &admission,
            }),
            revision: None,
            origin: None,
            retained_time: None,
        },
    )?;
    drop(guard);
    drop(job.completion);
    Ok(job.id)
}

async fn wait(
    handle: &Context,
    key: &InvocationKey,
    wire: u64,
    numeric: u64,
    lease: &str,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(900))
        .ok_or("task step deadline overflow")?;
    loop {
        task_actions::observed(handle, numeric, lease)?;
        handle.state.expire_stale_invocations();
        let record = retained(handle, key)?.ok_or("task step dispatch record absent")?;
        if record.uncertain || record.wire_id != wire {
            return Err("task step dispatch outcome uncertain or changed".into());
        }
        if record.update.is_terminal() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("task step is still running; retry the same step".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

fn settlement(
    record: &DurableInvocation,
) -> Result<(WorkspaceTaskStepOutcome, [u8; 32], Vec<u8>), String> {
    if record.uncertain || record.update.invocation() != record.key.invocation {
        return Err("uncertain or mismatched terminal cannot settle a task step".into());
    }
    let (outcome, continuation) = match &record.update {
        InvocationUpdate::Succeeded { output, .. } => (
            WorkspaceTaskStepOutcome::Succeeded,
            json!({"succeeded":output}),
        ),
        InvocationUpdate::Failed { error, .. } => (
            WorkspaceTaskStepOutcome::Failed,
            json!({"failed":error.code}),
        ),
        InvocationUpdate::Cancelled { .. } => (
            WorkspaceTaskStepOutcome::Cancelled,
            json!({"cancelled":true}),
        ),
        _ => return Err("task step has no canonical terminal".into()),
    };
    let output = digest(&json!({"update":record.update,"continuation":continuation}))?;
    let bytes = serde_json::to_vec(&continuation).map_err(|_| "continuation encoding failed")?;
    Ok((outcome, output, bytes))
}

async fn commit_step(
    handle: &Context,
    room: &str,
    authority: WorkspaceTaskAuthority,
    command: WorkspaceTaskCommand,
) -> Result<(), String> {
    let (invocation, kind) = match &command {
        WorkspaceTaskCommand::PrepareStep { invocation, .. } => {
            (*invocation, b"prepare".as_slice())
        }
        WorkspaceTaskCommand::CommitStep { invocation, .. } => (*invocation, b"commit".as_slice()),
        _ => return Err("not a native step owner command".into()),
    };
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo.native-task-step.owner-command.v1\0");
    hash.update(invocation.0.as_bytes());
    hash.update(kind);
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let request = OwnerCommitRequest {
        room: room.into(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::from_bytes(bytes),
            authority,
            command,
        },
    };
    let receipt = handle
        .state
        .services
        .workspace_memberships
        .as_ref()
        .ok_or("native owner absent")?
        .execute(request.clone())
        .await?;
    receipt.validate_for(&request)?;
    Ok(())
}
