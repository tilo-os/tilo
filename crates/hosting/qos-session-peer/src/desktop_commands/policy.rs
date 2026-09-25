//! One original caller policy check for semantic entry and the pre-write fence.
use super::*;
use qos_action_broker::peer_actions::PolicyTime;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

pub(crate) struct DesktopPolicyFence {
    request: OwnerCommitRequest,
    time: PolicyTime,
    purpose: Purpose,
}
enum Purpose {
    Mutation(Vec<(String, String)>),
    Observation { app: String, record: String },
}
impl DesktopPolicyFence {
    pub(crate) fn mutation(
        request: &OwnerCommitRequest,
        time: &PolicyTime,
        routes: Vec<(String, String)>,
    ) -> Self {
        Self {
            request: request.clone(),
            time: time.clone(),
            purpose: Purpose::Mutation(routes),
        }
    }
    pub(crate) fn observation(
        request: &OwnerCommitRequest,
        time: &PolicyTime,
        app: &str,
        record: &str,
    ) -> Self {
        Self {
            request: request.clone(),
            time: time.clone(),
            purpose: Purpose::Observation {
                app: app.into(),
                record: record.into(),
            },
        }
    }
    pub(crate) fn policy_time(&self) -> &PolicyTime {
        &self.time
    }
    pub(crate) fn revalidate(&self, owner: &SessionOwner) -> Result<(), String> {
        let now = crate::owner::policy_fence::sample(Some(&self.time))?;
        let (caller, command) = current_caller(owner, &self.request, now)?;
        match &self.purpose {
            Purpose::Mutation(routes) => {
                topology(owner, caller, command, now)?;
                for (app, record) in routes {
                    crate::canonical_records::authorize_creation(
                        owner.peer(),
                        caller,
                        app,
                        record,
                        now,
                    )?;
                }
            }
            Purpose::Observation { app, record } => {
                owner
                    .peer()
                    .check_document_read(caller.device_public_key, app, record, now)?
            }
        }
        Ok(())
    }
}
pub(super) fn current_caller<'a>(
    owner: &SessionOwner,
    request: &'a OwnerCommitRequest,
    now: SystemTime,
) -> Result<(&'a DeviceCert, &'a BoundCommand), String> {
    let CommitOperation::DesktopCommand {
        caller, command, ..
    } = &request.operation
    else {
        return Err("not a desktop command".into());
    };
    if request.room != owner.peer().room()
        || !owner.permits_publication()
        || !owner.peer().certificates().contains(caller)
    {
        return Err("caller or room is no longer admitted".into());
    }
    owner.peer().permissions(caller.device_public_key, now)?;
    owner
        .peer()
        .permissions(owner.peer().authority().device_public_key, now)?;
    owner.peer().validated_media_policy()?;
    Ok((caller, command))
}
pub(super) fn topology(
    owner: &SessionOwner,
    caller: &DeviceCert,
    command: &BoundCommand,
    now: SystemTime,
) -> Result<(), String> {
    use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
    let (role, caps, person) = owner.peer().permissions(caller.device_public_key, now)?;
    let actor = Actor::person(Some(role), person).in_room(owner.peer().room());
    let target = ResourceRef::Action {
        app: APP_ID.into(),
        action: ACTION.into(),
    };
    let verb = if matches!(command.command, DesktopCommand::RenameWorkspace { .. }) {
        Verb::Admin
    } else {
        Verb::Write
    };
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "desktop policy time predates epoch")?
        .as_secs();
    if decide_capability(&actor, &target, verb, &caps, Some(seconds)) != CapDecision::Allow {
        return Err(if verb == Verb::Admin {
            "workspace naming requires room administration"
        } else {
            "caller cannot change desktop topology"
        }
        .into());
    }
    Ok(())
}
