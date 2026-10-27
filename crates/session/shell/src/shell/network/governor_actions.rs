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
    /// The governor is a host (a relay governing the room as a service),
    /// which runs no apps.
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

/// Check that `page` is the answer to asking `host` for `provider`'s
/// `wanted` resource in `room`: every row is a view of that resource, and
/// the locator it names, if any, is this source's.
///
/// A page that found nothing — an application filter that cannot match the
/// record's own, say — names no page to browse with, so it need not carry a
/// locator. A page carrying rows does, and one naming a locator must name
/// this source whether or not it found anything.
fn resource_view_answer(
    page: &tilo_actions::remote::CataloguePage,
    room: RoomId,
    host: &str,
    provider: &str,
    wanted: &tilo_actions::remote::ViewResourceQuery,
) -> Result<(), String> {
    match page.locator.as_ref() {
        Some(locator)
            if locator.room != room.to_string()
                || locator.host_device != host
                || locator.provider != provider =>
        {
            return Err("resource host answered outside the requested source".into());
        }
        None if !page.rows.is_empty() => {
            return Err("resource host has no public browser locator".into());
        }
        _ => {}
    }
    if page.rows.iter().any(|row| {
        row.provider != provider
            || row.resource.id != wanted.id
            || row.resource.kind != wanted.kind
            || row
                .action
                .as_ref()
                .is_none_or(|action| action.kind != tilo_actions::OperationKind::View)
    }) {
        return Err("resource host answered outside the requested source".into());
    }
    Ok(())
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
        resource_view_answer(&page, room, &hex::encode(route.target), &provider, &wanted)?;
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
                governor_hosts: view.governor().capabilities.can_act_as_host,
            })
        })
    }

    /// Whether a device other than this one governs `room`: its windows
    /// and records are created by that governor.
    pub(crate) fn governed_elsewhere(&self, room: RoomId) -> bool {
        self.governor_route(room)
            .is_ok_and(|route| route.elsewhere())
    }

    /// [`Self::governed_elsewhere`] as `owner`, already open, admits it:
    /// for the frame loop, which never opens one.
    pub(crate) fn governed_elsewhere_by(&self, owner: &NativeWorkspaceOwner, room: RoomId) -> bool {
        self.governor_route_of(owner, room)
            .is_ok_and(|route| route.elsewhere())
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
    use tilo_actions::remote::{
        CataloguePage, CatalogueRow, ResourceViewLocator, ViewResourceQuery,
    };
    use tilo_actions::{
        ActionAddress, ActionContract, ActionDescriptor, ExecutionLocus, InputSpec,
    };

    use super::{RoomId, resource_view_answer};

    const PROVIDER: &str = "record:so.tilo.chat:part-1";
    const HOST: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

    fn room() -> RoomId {
        RoomId::parse("2b2b2b2b-2b2b-2b2b-2b2b-2b2b2b2b2b2b").expect("a room id")
    }

    fn wanted() -> ViewResourceQuery {
        ViewResourceQuery {
            id: "chat.messages.messages:hello".into(),
            kind: "chat.messages.messages".into(),
        }
    }

    /// One view row for `wanted()`, as the host's answer carries it.
    fn view_row() -> CatalogueRow {
        let wanted = wanted();
        CatalogueRow::for_action(
            PROVIDER,
            "so.tilo.chat",
            ActionDescriptor {
                semantic_identity: None,
                address: ActionAddress::new(wanted.id, "open"),
                resource_kind: wanted.kind,
                resource_label: "Hello".into(),
                action_label: "Open".into(),
                description: None,
                locus: ExecutionLocus::default(),
                kind: tilo_actions::OperationKind::View,
                contract: ActionContract::view(),
                input: InputSpec::None,
                params: Vec::new(),
                view: None,
            },
        )
    }

    /// The locator the host issues for `wanted()`'s page in `room()`.
    fn locator() -> ResourceViewLocator {
        ResourceViewLocator {
            origin: "https://room.example".into(),
            room: room().to_string(),
            host_device: HOST.into(),
            provider: PROVIDER.into(),
            web_entry: None,
        }
    }

    fn answer(page: &CataloguePage) -> Result<(), String> {
        resource_view_answer(page, room(), HOST, PROVIDER, &wanted())
    }

    #[test]
    fn a_matching_app_filtered_view_keeps_its_locator() {
        let page = CataloguePage {
            revision: "r1".into(),
            rows: vec![view_row()],
            locator: Some(locator()),
            ..CataloguePage::default()
        };
        assert_eq!(answer(&page), Ok(()));
    }

    #[test]
    fn a_mismatching_app_filtered_view_is_empty_and_names_no_locator() {
        // The host narrowed to an application this record's row does not
        // publish, so it found nothing: a legitimate answer, not an error.
        let page = CataloguePage {
            revision: "r1".into(),
            ..CataloguePage::default()
        };
        assert_eq!(answer(&page), Ok(()));
    }

    #[test]
    fn an_absent_app_filter_still_requires_a_locator_for_rows() {
        let page = CataloguePage {
            revision: "r1".into(),
            rows: vec![view_row()],
            ..CataloguePage::default()
        };
        assert_eq!(
            answer(&page),
            Err("resource host has no public browser locator".into())
        );
    }

    #[test]
    fn an_empty_answer_naming_another_source_is_still_refused() {
        for claimed in [
            ResourceViewLocator {
                room: uuid::Uuid::nil().to_string(),
                ..locator()
            },
            ResourceViewLocator {
                host_device: "b2".repeat(32),
                ..locator()
            },
            ResourceViewLocator {
                provider: "record:so.tilo.notes:other".into(),
                ..locator()
            },
        ] {
            let page = CataloguePage {
                revision: "r1".into(),
                locator: Some(claimed),
                ..CataloguePage::default()
            };
            assert_eq!(
                answer(&page),
                Err("resource host answered outside the requested source".into())
            );
        }
    }

    #[test]
    fn a_row_outside_the_requested_view_is_refused() {
        let mut elsewhere = view_row();
        elsewhere.provider = "record:so.tilo.notes:other".into();
        let mut other_kind = view_row();
        other_kind.resource.kind = "notes.block".into();
        let mut other_action = view_row();
        other_action.action.as_mut().expect("a view row").kind =
            tilo_actions::OperationKind::Effect;
        for row in [elsewhere, other_kind, other_action] {
            let page = CataloguePage {
                revision: "r1".into(),
                rows: vec![row],
                locator: Some(locator()),
                ..CataloguePage::default()
            };
            assert_eq!(
                answer(&page),
                Err("resource host answered outside the requested source".into())
            );
        }
    }

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
