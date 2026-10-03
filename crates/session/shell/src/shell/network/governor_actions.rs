//! Asking a governed room's governor: invoking an action it publishes, as
//! this device, over the room's own relay session, and signing what this
//! device says there. The governor decides; this side only asks and waits.
use super::{NetCmd, ShellNetwork, room_topic_id};
use crate::network::actor::governor_lane::Request;
use crate::shell::state::RoomId;
use tilo_actions::remote::Pending;
use tilo_delivery_protocol::peer_actions::{ActionMessage, DirectedAction, ExactActionRoute};
use tokio::sync::mpsc;

/// How long one invocation may take end to end.
const GOVERNOR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// How often an invocation still running is asked about.
const STATUS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Where requests to one room's governor go.
#[derive(Clone)]
struct GovernorRoute {
    ws: u64,
    room: uuid::Uuid,
    source: [u8; 32],
    target: [u8; 32],
    governor_user: [u8; 32],
}

impl GovernorRoute {
    async fn ask(
        &self,
        commands: &mpsc::Sender<NetCmd>,
        message: ActionMessage,
    ) -> Result<ActionMessage, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let action = DirectedAction {
            room: self.room,
            request_id: uuid::Uuid::new_v4(),
            source: self.source,
            target: self.target,
            message,
        };
        if !action.validate() {
            return Err("the request to the governor is malformed".into());
        }
        commands
            .send(NetCmd::GovernorAction {
                ws: self.ws,
                room: self.room,
                request: Box::new(Request {
                    action,
                    governor_user: self.governor_user,
                    reply,
                }),
            })
            .await
            .map_err(|_| "the network is not running")?;
        answer.await.map_err(|_| "the governor lane closed")?
    }
}

/// The published surface whose actions include `resource`/`action`.
fn surface_for(catalogue: &serde_json::Value, resource: &str, action: &str) -> Option<u64> {
    catalogue
        .get("surfaces")?
        .as_array()?
        .iter()
        .find(|surface| {
            surface["actions"].as_array().is_some_and(|actions| {
                actions
                    .iter()
                    .any(|entry| entry["resource"] == resource && entry["action"] == action)
            })
        })?
        .get("peer_id")?
        .as_u64()
}

async fn invoke(
    commands: mpsc::Sender<NetCmd>,
    route: GovernorRoute,
    app_id: String,
    resource: String,
    action: String,
    argument: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let scope = ExactActionRoute {
        app_id: app_id.clone(),
        record_id: String::new(),
        resource: resource.clone(),
        action: action.clone(),
    };
    let ActionMessage::BrokerCatalogue { host, catalogue } = route
        .ask(
            &commands,
            ActionMessage::CatalogueRefresh { scope: Some(scope) },
        )
        .await?
    else {
        return Err("the governor does not offer that".into());
    };
    let peer =
        surface_for(&catalogue, &resource, &action).ok_or("the governor does not offer that")?;
    let invocation = tilo_actions::InvocationId::new();
    let mut reply = route
        .ask(
            &commands,
            ActionMessage::InvokeBound {
                host,
                invocation,
                app_peer: peer.to_string(),
                app_id,
                record_id: String::new(),
                resource,
                action,
                argument,
                confirmation: None,
                revision: None,
            },
        )
        .await?;
    loop {
        match reply {
            ActionMessage::Observation {
                invocation: id,
                update: Some(update),
                ..
            } if id == invocation && update.is_terminal() => {
                return match update {
                    tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                        Ok(output.value.unwrap_or(serde_json::Value::Null))
                    }
                    tilo_actions::InvocationUpdate::Failed { error, .. } => Err(error.message),
                    _ => Err("the governor did not finish it".into()),
                };
            }
            ActionMessage::Observation { invocation: id, .. } if id == invocation => {
                tokio::time::sleep(STATUS_INTERVAL).await;
                reply = route
                    .ask(&commands, ActionMessage::Status { invocation })
                    .await?;
            }
            ActionMessage::Rejected { code } => return Err(code),
            _ => return Err("the governor answered something else".into()),
        }
    }
}

impl ShellNetwork {
    fn governor_route(&self, room: RoomId) -> Result<GovernorRoute, String> {
        let owner = self.native_owner()?;
        self.check_native_custody(&owner, room)?;
        let room_uuid = uuid::Uuid::parse_str(&room.to_string()).map_err(|e| e.to_string())?;
        owner.with_current(room, |view| {
            Ok(GovernorRoute {
                ws: room_topic_id(room),
                room: room_uuid,
                source: view.device().device_public_key,
                target: view.governor().device_public_key,
                governor_user: view.governor().user_public_key,
            })
        })
    }

    /// Whether a device other than this one governs `room`: its windows
    /// and records are created by that governor.
    pub(crate) fn governed_elsewhere(&self, room: RoomId) -> bool {
        self.governor_route(room)
            .is_ok_and(|route| route.source != route.target)
    }

    /// Invoke `resource`/`action` of `app_id` on `room`'s governor with
    /// `argument`, as this device, and answer with its result.
    pub(crate) fn invoke_governor(
        &self,
        room: RoomId,
        app_id: &str,
        resource: &str,
        action: &str,
        argument: Option<serde_json::Value>,
    ) -> Pending<Result<serde_json::Value, String>> {
        let (completer, answer) = tilo_actions::remote::pending();
        let route = match self.governor_route(room) {
            Ok(route) => route,
            Err(reason) => {
                completer.complete(Err(reason));
                return answer;
            }
        };
        let Some(runtime) = self.runtime.as_ref() else {
            completer.complete(Err("the network is not running".into()));
            return answer;
        };
        let (commands, app_id, resource, action) = (
            self.cmd_tx.clone(),
            app_id.to_owned(),
            resource.to_owned(),
            action.to_owned(),
        );
        runtime.spawn(async move {
            let result = tokio::time::timeout(GOVERNOR_TIMEOUT, async {
                // Right after startup the room's relay session may still be dialing.
                loop {
                    let result = invoke(
                        commands.clone(),
                        route.clone(),
                        app_id.clone(),
                        resource.clone(),
                        action.clone(),
                        argument.clone(),
                    )
                    .await;
                    match result {
                        Err(reason)
                            if reason == crate::network::actor::native_ingress::NOT_CONNECTED =>
                        {
                            tokio::time::sleep(STATUS_INTERVAL).await;
                        }
                        result => break result,
                    }
                }
            })
            .await
            .unwrap_or_else(|_| Err("the governor did not answer in time".into()));
            completer.complete(result);
        });
        answer
    }

    /// This device's key for `room`, once its custody is current: what
    /// signs proposals and decisions made there.
    pub(crate) fn native_signing_key(
        &self,
        room: RoomId,
    ) -> Result<ed25519_dalek::SigningKey, String> {
        let owner = self.native_owner()?;
        self.check_native_custody(&owner, room)?;
        self.host_identity
            .as_ref()
            .map(|identity| identity.device_signing_key.clone())
            .ok_or_else(|| "this device has no identity".into())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_room_no_other_device_governs_is_opened_here() {
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo().shared();
        let room = tree.read().desktop.workspaces[0].room;
        let (net, _commands) = super::super::shell_network_for_test(tree, 0);
        assert!(!net.governed_elsewhere(room));
    }

    #[test]
    fn a_surface_is_found_by_the_action_it_publishes() {
        let catalogue = serde_json::json!({"version":1,"surfaces":[
            {"peer_id":3,"app_id":"so.tilo.desktop","actions":[{"resource":"desktop:session","action":"command"}]},
            {"peer_id":7,"app_id":"so.tilo.desktop","actions":[{"resource":"desktop:document-import","action":"propose"}]}
        ]});
        assert_eq!(
            super::surface_for(&catalogue, "desktop:document-import", "propose"),
            Some(7)
        );
        assert_eq!(
            super::surface_for(&catalogue, "desktop:document-import", "accept"),
            None
        );
    }
}
