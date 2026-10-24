//! Asking a governed room's governor: invoking an action it publishes, as
//! this device, over the room's own relay session, and signing what this
//! device says there. The governor decides; this side only asks and waits.
use super::{NetCmd, ShellNetwork, room_topic_id};
use crate::network::actor::governor_lane::Request;
use crate::shell::{native_workspace_owner::NativeWorkspaceOwner, state::RoomId};
use tilo_actions::remote::Pending;
use tilo_delivery_protocol::peer_actions::{ActionMessage, DirectedAction, ExactActionRoute};
use tokio::sync::mpsc;

/// How long one invocation may take end to end.
#[cfg(not(test))]
fn governor_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(30)
}
/// How long one invocation may take end to end: in test builds, whose fixture
/// governor shares a loaded runner with the rest of the suite, the shared test
/// hang bound. Running out reports the outcome unknown and turns the caller's
/// collaboration off, so a slow fixture would otherwise read as a refusal.
#[cfg(test)]
fn governor_timeout() -> std::time::Duration {
    tilo_test_env::hang_bound()
}
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
    /// The governor is a host, which runs no apps: a relay governing the
    /// room as a service, or the room host of the device it is placed on.
    governor_hosts: bool,
}

impl GovernorRoute {
    /// Whether another device governs the room.
    fn elsewhere(&self) -> bool {
        self.source != self.target
    }

    /// Whether the room's governor opens its apps: another device that runs
    /// them, not a host.
    fn opens_apps(&self) -> bool {
        self.elsewhere() && !self.governor_hosts
    }

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
    /// Record a member audit at the exact admitted governor on the existing
    /// certified action lane. No endpoint fallback or caller-selected authority.
    pub(crate) async fn record_effect_at_governor(
        &self,
        room: RoomId,
        invocation: uuid::Uuid,
        redacted: Vec<u8>,
    ) -> Result<(), String> {
        let route = self.governor_route(room)?;
        let invocation = tilo_actions::InvocationId(invocation);
        let redacted = String::from_utf8(redacted).map_err(|_| "invalid effect encoding")?;
        let answer = tokio::time::timeout(governor_timeout(), async {
            loop {
                let answer = route
                    .ask(
                        &self.cmd_tx,
                        ActionMessage::RecordEffectReceipt {
                            invocation,
                            redacted: redacted.clone(),
                        },
                    )
                    .await;
                match answer {
                    Err(reason)
                        if reason == crate::network::actor::native_ingress::NOT_CONNECTED =>
                    {
                        // The actor refused before queueing a directed request.
                        // Wait for the original native session, never a fallback.
                        tokio::time::sleep(STATUS_INTERVAL).await;
                    }
                    answer => break answer,
                }
            }
        })
        .await
        .map_err(|_| "effect governor did not answer in time; outcome unknown")??;
        let fresh = self.governor_route(room)?;
        if fresh.source != route.source
            || fresh.target != route.target
            || fresh.governor_user != route.governor_user
        {
            return Err("effect governor or selected device changed; outcome unknown".into());
        }
        match answer {
            ActionMessage::EffectReceiptCommitted { invocation: got } if got == invocation => {
                Ok(())
            }
            ActionMessage::Rejected { code } => Err(format!("effect governor refused: {code}")),
            _ => Err("effect governor returned an unexpected answer".into()),
        }
    }
    /// Discover one existing resource on the currently governed room's host,
    /// using the same authenticated remote-action transport as the palette.
    pub(crate) async fn discover_resource_view(
        &self,
        room: RoomId,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Result<tilo_actions::remote::CataloguePage, String> {
        use crate::shell::remote_action_link::RemoteActionLink as _;
        use tilo_actions::remote::RemoteActionMessage;
        let route = self.governor_route(room)?;
        let wanted = query
            .view_resource
            .as_ref()
            .ok_or("missing resource view query")?
            .clone();
        let provider = query
            .provider
            .as_ref()
            .ok_or("missing resource provider")?
            .clone();
        let target =
            iroh::EndpointId::from_bytes(&route.target).map_err(|error| error.to_string())?;
        // Fresh ids do not collide with another palette's or room's counter.
        let correlation = uuid::Uuid::new_v4().as_u64_pair().0;
        let pending = self.request(
            route.ws,
            target,
            RemoteActionMessage::CatalogueRequest { correlation, query },
        )?;
        let answer = tokio::time::timeout(governor_timeout(), pending)
            .await
            .map_err(|_| "resource host lookup timed out")?
            .ok_or("resource host lookup ended")?;
        let RemoteActionMessage::CatalogueResponse {
            correlation: got,
            result,
        } = answer
        else {
            return Err("resource host answered another request".into());
        };
        if got != correlation {
            return Err("resource host correlation changed".into());
        }
        let page = result.map_err(|error| error.to_string())?;
        let fresh = self.governor_route(room)?;
        if fresh.source != route.source
            || fresh.target != route.target
            || fresh.governor_user != route.governor_user
        {
            return Err("resource host or selected device changed".into());
        }
        let locator = page
            .locator
            .as_ref()
            .ok_or("resource host has no public browser locator")?;
        if locator.room != room.to_string()
            || locator.host_device != hex::encode(route.target)
            || locator.provider != provider
            || page.rows.iter().any(|row| {
                row.provider != provider
                    || row.resource.id != wanted.id
                    || row.resource.kind != wanted.kind
                    || row
                        .action
                        .as_ref()
                        .is_none_or(|action| action.kind != tilo_actions::OperationKind::View)
            })
        {
            return Err("resource host answered outside the requested source".into());
        }
        Ok(page)
    }

    fn governor_route(&self, room: RoomId) -> Result<GovernorRoute, String> {
        self.governor_route_of(&self.native_owner()?, room)
    }

    /// [`Self::governor_route`] as `owner`, already open, admits it.
    fn governor_route_of(
        &self,
        owner: &NativeWorkspaceOwner,
        room: RoomId,
    ) -> Result<GovernorRoute, String> {
        self.check_native_custody(owner, room)?;
        let room_uuid = uuid::Uuid::parse_str(&room.to_string()).map_err(|e| e.to_string())?;
        owner.with_current(room, |view| {
            Ok(GovernorRoute {
                ws: room_topic_id(room),
                room: room_uuid,
                source: view.device().device_public_key,
                target: view.governor().device_public_key,
                governor_user: view.governor().user_public_key,
                governor_hosts: view.governor().capabilities.can_act_as_host
                    || super::relay_rooms::hosting_governor(view, room)
                        .is_ok_and(|host| host.is_some()),
            })
        })
    }

    /// Whether a device other than this one governs `room`: its windows
    /// and records are created by that governor.
    pub(crate) fn governed_elsewhere(&self, room: RoomId) -> bool {
        self.governor_route(room)
            .is_ok_and(|route| route.elsewhere())
    }

    /// Whether another device governs `room` and it is a host (a home
    /// relay), which creates the room's records on a member's request, as
    /// `owner` holds the room.
    pub(crate) fn host_creates_records(&self, owner: &NativeWorkspaceOwner, room: RoomId) -> bool {
        self.governed_elsewhere(room)
            && owner
                .with_current(room, |view| {
                    Ok(view.governor().capabilities.can_act_as_host)
                })
                .unwrap_or(false)
    }

    /// Whether `room`'s governor opens the apps launched in it, creating
    /// and placing their records. A host governing it runs no apps: this
    /// session opens them itself, as it does its legacy windows.
    pub(crate) fn governor_opens_apps(&self, room: RoomId) -> bool {
        self.governor_route(room)
            .is_ok_and(|route| route.opens_apps())
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
            let result = tokio::time::timeout(governor_timeout(), async {
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

    /// This device's key for `room`, once its custody as `owner` admits it
    /// is current: what signs proposals and decisions made there.
    pub(crate) fn native_signing_key_of(
        &self,
        owner: &NativeWorkspaceOwner,
        room: RoomId,
    ) -> Result<ed25519_dalek::SigningKey, String> {
        self.check_native_custody(owner, room)?;
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
        let room = tree.read().desktop.workspaces[0].room();
        let (net, _commands) = super::super::shell_network_for_test(tree, 0);
        assert!(!net.governed_elsewhere(room));
        assert!(!net.governor_opens_apps(room));
    }

    #[test]
    fn only_a_governing_device_opens_apps_not_a_host() {
        let route = |target: u8, governor_hosts| super::GovernorRoute {
            ws: 1,
            room: uuid::Uuid::nil(),
            source: [1; 32],
            target: [target; 32],
            governor_user: [9; 32],
            governor_hosts,
        };
        assert!(route(2, false).opens_apps(), "a person's other device");
        assert!(!route(2, true).opens_apps(), "a relay governing the room");
        assert!(route(2, true).elsewhere());
        assert!(!route(1, false).opens_apps(), "this device governs it");
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
