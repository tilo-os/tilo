//! A retained app's launch is a separate host effect, not admission of the
//! application action that will only become authoritative after publication.

use super::*;
use crate::collab_broker_capnp::{InvocationCaller, host_action_admission::HostActionAdmission};
use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};
use tilo_actions::{InvocationId, InvocationUpdate};

/// Domain-separated, length-framed derivation: neither room/app boundaries nor
/// the original application invocation can alias this child request.
fn child_id(parent: InvocationId, room: &str, app: &str) -> InvocationId {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo.retained-app-launch.v1\0");
    hash.update(parent.0.as_bytes());
    for part in [room, app] {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    // RFC 9562 application-defined UUID; no random generator or new dependency.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    InvocationId(uuid::Uuid::from_bytes(bytes))
}

fn child_request(
    parent: InvocationId,
    room: &str,
    descriptor: &ActionDescriptor,
    app: &str,
) -> InvokeRequest {
    InvokeRequest {
        invocation: child_id(parent, room, app),
        target: RemoteActionTarget {
            provider: workspace_provider(room),
            address: descriptor.address.clone(),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

impl CapnpBrokerState {
    /// Called on the blocking launch worker, so a dropped waiter cannot leave
    /// an already-started launcher without its terminal signed observation.
    pub(super) fn launch_retained_child(
        &self,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        parent: &InvokeRequest,
        destination: &LaunchDestination,
        place: &LaunchPlace,
    ) -> Result<(), ActionHostError> {
        let record = destination
            .record
            .as_deref()
            .filter(|record| !record.is_empty());
        let Some(origin) = origin else {
            // Native-task execution has independent retained custody and checks.
            // Its origin-less path cannot safely mint a certificate-owned child
            // from actor text. Preserve that path until it supplies typed custody.
            return self
                .launch_retained(&destination.app_id, &destination.workspace, record, place)
                .map_err(refusal);
        };
        if parent.invocation.0.is_nil() {
            return Err(ActionHostError::NotPermitted);
        }
        let room = destination.workspace.room.to_string();
        self.check_launch_origin(caller, Some(origin), &room)?;
        let app = self
            .services
            .launcher
            .as_ref()
            .and_then(|launcher| {
                launcher
                    .applications()
                    .into_iter()
                    .find(|app| app.package_id == destination.app_id && !app.unconfined)
            })
            .ok_or(ActionHostError::UnknownAction)?;
        let descriptor = app_open_descriptor(&app);
        let request = child_request(parent.invocation, &room, &descriptor, &app.package_id);
        let mut child_origin = origin.clone();
        if let InvocationCaller::NativePerson { invocation, .. } = &mut child_origin {
            // Only retained, protected native evidence may be rebound, and only
            // after checking it belongs to this exact parent invocation.
            if *invocation != parent.invocation {
                return Err(ActionHostError::NotPermitted);
            }
            *invocation = request.invocation;
        }
        let admitted = self.admit_host_action_with_start(
            caller,
            Some(&child_origin),
            HostActionAdmission {
                room: &room,
                app_id: &app.package_id,
                descriptor: &descriptor,
                request: &request,
            },
            Some(serde_json::json!({
                "parent_invocation": parent.invocation,
                "record": record,
                "place": match place {
                    LaunchPlace::Focus => serde_json::json!({ "focus": true }),
                    LaunchPlace::Beside { app_id } => serde_json::json!({ "beside": app_id }),
                },
            })),
        )?;
        let workspace = self.reauthorize_launch_effect(
            (caller, Some(&child_origin), &room),
            &admitted,
            &app.package_id,
        )?;
        let result = self.launch_retained(&app.package_id, &workspace, record, place);
        let update = match &result {
            Ok(()) => InvocationUpdate::Succeeded {
                invocation: request.invocation,
                output: Default::default(),
            },
            Err(reason) => InvocationUpdate::Failed {
                invocation: request.invocation,
                error: tilo_actions::ActionFailure {
                    code: "launch_failed".into(),
                    message: reason.clone(),
                    details: None,
                    retryable: false,
                },
            },
        };
        self.complete_host_action(&admitted, update)?;
        result.map_err(refusal)
    }
}

#[cfg(test)]
mod tests;
