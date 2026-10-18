//! The session's action broker as an [`ActionHost`].
//!
//! [`BrokerActionHost`] is the one door through which anything other than
//! the publishing application reaches a published action: a peer on the
//! shell network, an app or agent on this machine over the collaboration
//! socket, the MCP bridge, the palette. Each supplies a [`Caller`] its own
//! transport authenticated; the broker admits, permission-checks and
//! dispatches the invocation exactly as it always has, and the shared
//! invocation ledger makes a retried [`InvocationId`] idempotent whichever
//! way it arrives. Native Person projections retain a certified session from
//! the protected seat service for custody-preserving actions; existing unsigned
//! workspace branches retain their original caller and lifecycle. App connections
//! and transport-authenticated network and agent callers never borrow that seat's
//! certificate custody.
//!
//! [`InvocationId`]: tilo_actions::InvocationId

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::shell::certified_action_session::CertifiedActionSession;
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::collab_broker_capnp::LaunchScope;

/// [`ActionHost`] over the live application broker.
#[derive(Clone)]
pub struct BrokerActionHost {
    broker: AppActionBroker,
    /// The application connection the caller itself is, when it is one; it
    /// is what [`tilo_actions::remote::OWN_PROVIDER`] names.
    own_peer: Option<u64>,
    /// The application the launcher authenticated for that connection; the
    /// only scope through which a launch may read content by hash.
    launch: Option<LaunchScope>,
    /// Original native execution -> retained certified execution. Clones must
    /// share this mapping: reopening per call loses invocation ownership.
    native_sessions: Option<Arc<Mutex<HashMap<Caller, CertifiedActionSession>>>>,
}

impl BrokerActionHost {
    /// The session facade. Native Person projections use a retained certified
    /// door when this broker has a native-seat service; a missing service is
    /// an uncertified/system context, never authority to sign an actor label.
    #[must_use]
    pub fn new(broker: AppActionBroker) -> Self {
        Self {
            broker,
            own_peer: None,
            launch: None,
            native_sessions: Some(Arc::default()),
        }
    }

    /// Select only native Person projections. Applications and authenticated
    /// network/agent callers retain their transport's authority unchanged.
    fn native_session(
        &self,
        caller: &Caller,
    ) -> Result<Option<CertifiedActionSession>, ActionHostError> {
        let Some(sessions) = &self.native_sessions else {
            return Ok(None);
        };
        if !matches!(
            caller.principal(),
            tilo_actions::remote::CallerPrincipal::Person(_)
        ) || caller.user_public_key().is_some()
            || caller.device_public_key().is_some()
            || caller.is_foreign_device()
            || caller.acting_for_person().is_some()
            || caller.shortcut().is_some()
        {
            return Ok(None);
        }
        let certificate = self
            .broker
            .state()
            .native_local_person_certificate(caller)
            .map_err(|_| ActionHostError::NotPermitted)?;
        let Some(certificate) = certificate else {
            // Pre-identity/system brokers have no certificate custody. Keep
            // their ordinary door; never sign their displayed actor text.
            return Ok(None);
        };
        let mut sessions = sessions.lock().map_err(|_| ActionHostError::Unavailable {
            reason: "native action sessions unavailable".into(),
        })?;
        if let Some(session) = sessions.get(caller) {
            return if session.has_certificate(&certificate) {
                Ok(Some(session.clone()))
            } else {
                // An existing execution may not silently become a new device.
                Err(ActionHostError::NotPermitted)
            };
        }
        sessions.retain(|caller, _| caller.execution_id().is_none() || caller.execution_is_live());
        if sessions.len() >= 128 {
            return Err(ActionHostError::Unavailable {
                reason: "native action session limit reached".into(),
            });
        }
        let session =
            CertifiedActionSession::open_native(self.broker.clone(), caller, &certificate)
                .map_err(|_| ActionHostError::NotPermitted)?;
        sessions.insert(caller.clone(), session.clone());
        Ok(Some(session))
    }

    #[cfg(test)]
    pub(crate) fn for_fixture(broker: AppActionBroker) -> Self {
        Self {
            native_sessions: None,
            ..Self::new(broker)
        }
    }

    /// Workspace branches without an original-custody admission barrier stay
    /// on their existing unsigned door. Never retry a refused signed action
    /// there: only this target classification may select compatibility.
    fn unsigned_workspace_target(target: &tilo_actions::remote::RemoteActionTarget) -> bool {
        use tilo_actions::remote::{
            APPLICATION_CLOSE_ACTION, APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX,
            workspace_provider_room,
        };
        workspace_provider_room(&target.provider).is_some()
            && !(target
                .address
                .resource
                .starts_with(APPLICATION_RESOURCE_PREFIX)
                && matches!(
                    target.address.action.as_str(),
                    APPLICATION_OPEN_ACTION | APPLICATION_CLOSE_ACTION
                ))
            && tilo_action_broker::automation::run_target(&target.address).is_none()
    }

    /// The host as seen from application connection `peer_id`, which the
    /// launcher authenticated as `launch` when it arrived through it.
    #[must_use]
    pub fn for_connection(
        broker: AppActionBroker,
        peer_id: u64,
        launch: Option<LaunchScope>,
    ) -> Self {
        Self {
            broker,
            own_peer: Some(peer_id),
            launch,
            native_sessions: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn broker_for_fixture(&self) -> AppActionBroker {
        self.broker.clone()
    }

    /// Legacy source fixtures delegate into the production broker's native
    /// authority checks. Live palette/control use maintained action sessions.
    #[cfg(test)]
    pub(crate) fn invoke_local_person_job(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<(u64, InvocationReceipt), ActionHostError>> {
        if self.own_peer.is_some() || self.launch.is_some() {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        let state = std::sync::Arc::clone(self.broker.state());
        let origin = caller.clone();
        let invocation = request.invocation;
        state
            .clone()
            .action_invoke_local_person(caller, request)
            .map(move |result| {
                let receipt = result?;
                let job = state
                    .remote_job(&origin, &invocation)
                    .ok_or(ActionHostError::UnknownInvocation)?;
                Ok((job, receipt))
            })
    }

    /// [`ActionHost::invoke`], also naming the broker job once admitted —
    /// for an in-process caller, such as the palette, that lists jobs.
    #[must_use]
    pub(crate) fn invoke_job(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<(u64, InvocationReceipt), ActionHostError>> {
        match self.native_session(caller) {
            Ok(Some(session)) if !Self::unsigned_workspace_target(&request.target) => {
                return session.invoke_job(request);
            }
            Ok(Some(_)) => {}
            Err(error) => return Pending::ready(Err(error)),
            Ok(None) => {}
        }
        let state = std::sync::Arc::clone(self.broker.state());
        let caller = caller.clone();
        let invocation = request.invocation;
        self.invoke(&caller, request).map(move |result| {
            let receipt = result?;
            let job = state
                .remote_job(&caller, &invocation)
                .ok_or(ActionHostError::UnknownInvocation)?;
            Ok((job, receipt))
        })
    }
}

impl ActionHost for BrokerActionHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        match self.native_session(caller) {
            // The original native catalogue includes unsigned governance,
            // shortcut-library and keyboard-shortcut actions. Filtering it
            // through the certified door would silently remove those features.
            // native_session has already checked current protected seat custody.
            Ok(Some(_)) | Ok(None) => self.broker.state().action_query(caller, &query),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        match self.native_session(caller) {
            Ok(Some(session)) if !Self::unsigned_workspace_target(&request.target) => {
                let (host, caller) = session.host_and_caller();
                host.invoke(caller, request)
            }
            Ok(Some(_)) | Ok(None) => self.broker.state().action_invoke(
                caller,
                self.own_peer,
                self.launch.as_ref(),
                &request,
            ),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        match self.native_session(caller) {
            Ok(Some(session)) => {
                let (host, certified) = session.host_and_caller();
                host.observe(certified, id)
                    .or_else(|| self.broker.state().action_observe(caller, id))
            }
            Ok(None) => self.broker.state().action_observe(caller, id),
            Err(_) => None,
        }
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        // Authorized as `observe` is: an invocation this caller did not
        // admit has no change to wait for.
        match self.native_session(caller) {
            Ok(Some(session)) => {
                let (host, certified) = session.host_and_caller();
                if host.observe(certified, id).is_some() {
                    host.next_change(certified, id)
                } else {
                    self.broker.state().action_next_change_after_now(caller, id)
                }
            }
            Ok(None) => self.broker.state().action_next_change_after_now(caller, id),
            Err(_) => None,
        }
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        match self.native_session(caller)? {
            Some(session) => {
                let (host, certified) = session.host_and_caller();
                if host.observe(certified, id).is_some() {
                    host.cancel(certified, id)
                } else {
                    self.broker.state().action_cancel(caller, id)
                }
            }
            None => self.broker.state().action_cancel(caller, id),
        }
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<tilo_actions::remote::Confirmation, ActionHostError>> {
        match self.native_session(caller) {
            Ok(Some(session)) if !Self::unsigned_workspace_target(&request.target) => {
                let (host, caller) = session.host_and_caller();
                host.confirm(caller, request)
            }
            Ok(Some(_)) | Ok(None) => self.broker.state().request_witness(caller, None, request),
            Err(error) => Pending::ready(Err(error)),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use super::*;
    use crate::shell::collab_broker::CrdtRoute;
    use crate::shell::collab_broker_capnp::{PublishedAction, PublishedInput, PublishedSurface};
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
    use tilo_actions::ActionAddress;
    use tilo_actions::remote::ReceiptStatus;
    use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

    const OWNER_ROOT: [u8; 32] = [0xA1; 32];

    fn owner() -> Caller {
        Caller::authenticated(OWNER_ROOT, [0xA2; 32])
    }

    fn stranger() -> Caller {
        Caller::authenticated([0xB1; 32], [0xB2; 32])
    }

    fn action(name: &str, contract: tilo_actions::ActionContract) -> PublishedAction {
        PublishedAction {
            semantic_identity: None,
            resource: "files.entry:/tmp/report".into(),
            action: name.into(),
            resource_kind: "files.entry".into(),
            resource_label: "report".into(),
            action_label: name.into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract,
            input: PublishedInput::None,
            view: None,
        }
    }

    /// A broker hosting one workspace the owner's root owns, with one live
    /// files app publishing a cancellable job and a write-gated action.
    fn host() -> (
        BrokerActionHost,
        tokio::sync::mpsc::UnboundedReceiver<crate::shell::collab_broker_capnp::InvocationEnvelope>,
    ) {
        host_with(Vec::new())
    }

    /// [`host`], with `extra` actions published beside the fixture's own.
    fn host_with(
        extra: Vec<PublishedAction>,
    ) -> (
        BrokerActionHost,
        tokio::sync::mpsc::UnboundedReceiver<crate::shell::collab_broker_capnp::InvocationEnvelope>,
    ) {
        let mut workspace = Workspace::new(WorkspaceId(41), "Remote room");
        workspace.set_owner("a1".repeat(32));
        let route = CrdtRoute::app("so.tilo.files", workspace.room.to_string());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let broker = crate::shell::broker_services::test_broker_with_membership(tree.shared());
        broker.replace_published_surfaces_for_test(vec![PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.files".into(),
            route,
            actions: vec![
                action(
                    "refresh",
                    tilo_actions::ActionContract {
                        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                        result: tilo_actions::ResultSpec::Job,
                        ..tilo_actions::ActionContract::default()
                    },
                ),
                action(
                    "erase",
                    tilo_actions::ActionContract {
                        risk: tilo_actions::RiskLevel::Destructive,
                        confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                        ..tilo_actions::ActionContract::default()
                    },
                ),
                action(
                    "rename",
                    tilo_actions::ActionContract {
                        kind: tilo_actions::ActionKind::Undoable,
                        ..tilo_actions::ActionContract::default()
                    },
                ),
            ]
            .into_iter()
            .chain(extra)
            .collect(),
            live: true,
        }]);
        let invocations = broker.capture_invocations_for_test(7);
        (BrokerActionHost::new(broker), invocations)
    }

    fn refresh(invocation: tilo_actions::InvocationId) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: "7".into(),
                address: ActionAddress::new("files.entry:/tmp/report", "refresh"),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        }
    }

    /// The answer of a host that answers at once.
    fn now<T>(mut answer: tilo_actions::remote::Pending<T>) -> T {
        match answer.poll_ready() {
            tilo_actions::remote::PendingPoll::Ready(value) => value,
            _ => panic!("an eager surface answers at once"),
        }
    }

    #[test]
    fn installed_native_service_refusal_never_falls_back_and_other_doors_do_not_borrow_it() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Refuse(AtomicUsize);
        impl tilo_action_broker::services::LocalPersonServices for Refuse {
            fn certificate(
                &self,
                _: &str,
                _: &str,
            ) -> Result<tilo_user_identity::DeviceCert, String> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Err("native identity unavailable".into())
            }
        }
        let service = Arc::new(Refuse(AtomicUsize::new(0)));
        let tree = WorkspaceTree::default().shared();
        let broker =
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_authority(
                Arc::new(crate::shell::broker_services::NativeWorkspaceAuthority(
                    tree,
                )),
                tilo_action_broker::services::HostServices {
                    local_person: Some(service.clone()),
                    ..Default::default()
                },
            );
        let host = BrokerActionHost::new(broker.clone());
        let native = Caller::local_person("displayed-person").in_room("room");
        assert!(matches!(
            now(host.query(&native, CatalogueQuery::default())),
            Err(ActionHostError::NotPermitted)
        ));
        assert_eq!(
            now(host.invoke(&native, refresh(tilo_actions::InvocationId::new()))),
            Err(ActionHostError::NotPermitted)
        );
        assert_eq!(service.0.load(Ordering::Relaxed), 2);
        // These paths own their own authority. In particular, an MCP agent
        // cannot turn its person's display label into native certificate custody.
        let agent = Caller::local_agent("agent").in_room("room");
        let delegated = native.clone().delegated_to("agent");
        for caller in [agent, delegated, owner().in_room("room")] {
            drop(host.query(&caller, CatalogueQuery::default()));
            drop(host.invoke(&caller, refresh(tilo_actions::InvocationId::new())));
        }
        let app = BrokerActionHost::for_connection(broker.clone(), 7, None);
        drop(app.query(&native, CatalogueQuery::default()));
        drop(app.invoke(&native, refresh(tilo_actions::InvocationId::new())));
        let source = crate::shell::certified_action_session::ActionSessionSource::fixture(broker);
        let fixture = source.open("room", Some("displayed-person")).unwrap();
        drop(fixture.query(CatalogueQuery::default()));
        drop(fixture.invoke(refresh(tilo_actions::InvocationId::new())));
        assert_eq!(
            service.0.load(Ordering::Relaxed),
            2,
            "transport and fixture doors must not recurse into native admission"
        );
    }

    #[test]
    fn unsigned_workspace_compatibility_does_not_downgrade_signed_host_targets() {
        use tilo_actions::remote::{application_open_target, workspace_provider};
        let room = uuid::Uuid::new_v4().to_string();
        let mut target = application_open_target(&room, "so.tilo.notes");
        assert!(!BrokerActionHost::unsigned_workspace_target(&target));
        target.address.action = tilo_actions::remote::APPLICATION_CLOSE_ACTION.into();
        assert!(!BrokerActionHost::unsigned_workspace_target(&target));
        target.address = ActionAddress::new("workspace", "grant");
        assert!(BrokerActionHost::unsigned_workspace_target(&target));
        target.provider = "7".into();
        assert!(!BrokerActionHost::unsigned_workspace_target(&target));
        target.provider = "own-device".into();
        assert!(!BrokerActionHost::unsigned_workspace_target(&target));
        target.provider = workspace_provider(&room);
        target.address = ActionAddress::new(
            tilo_automation::shortcut_resource_id("sample"),
            tilo_automation::SHORTCUT_RUN_ACTION,
        );
        assert!(!BrokerActionHost::unsigned_workspace_target(&target));
    }

    fn catalogue(host: &BrokerActionHost, caller: &Caller) -> CataloguePage {
        now(host.query(
            caller,
            CatalogueQuery {
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        ))
        .expect("catalogue")
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn the_remote_catalogue_offers_only_what_the_caller_may_reach() {
        // Phase `the_catalogue_offers_only_what_the_caller_may_use`.
        {
            let (host, _) = host();
            let page = catalogue(&host, &owner());
            let offered: Vec<_> = page
                .rows
                .iter()
                .filter_map(|row| {
                    Some((
                        row.provider.split(':').next().unwrap_or_default(),
                        row.action.as_ref()?.address.action.as_str(),
                    ))
                })
                .collect();
            assert_eq!(
                offered,
                [
                    ("7", "refresh"),
                    ("7", "erase"),
                    ("7", "rename"),
                    ("workspace", "grant"),
                    ("workspace", "revoke"),
                    ("workspace", "preset.save"),
                    ("workspace", "preset.assign"),
                    ("workspace", "app-action.grant"),
                    ("workspace", "app-action.revoke"),
                    ("workspace", "create"),
                    ("workspace", "update"),
                    ("workspace", "delete"),
                    ("workspace", "list"),
                ],
                "a remote device is not offered launching apps on this desktop"
            );
            assert!(
                catalogue(&host, &stranger()).rows.is_empty(),
                "a caller outside the workspace is offered nothing"
            );
        }

        // Phase `the_catalogue_carries_a_published_resource_view`.
        {
            let mut open = action("open", tilo_actions::ActionContract::view());
            open.operation_kind = tilo_actions::OperationKind::View;
            let view = tilo_actions::ResourceViewDescriptor {
                id: "preview".into(),
                label: "Preview".into(),
                description: None,
                open_action: "open".into(),
                primary: true,
                range: None,
                mount: None,
            };
            open.view = Some(view.clone());
            let (host, _) = host_with(vec![open]);

            let page = catalogue(&host, &owner());
            let offered = page
                .rows
                .iter()
                .filter_map(|row| row.action.as_ref())
                .find(|action| action.address.action == "open")
                .expect("the view's open action is offered");
            assert_eq!(offered.view.as_ref(), Some(&view));
            assert!(
                page.rows
                    .iter()
                    .filter_map(|row| row.action.as_ref())
                    .filter(|action| action.address.action != "open")
                    .all(|action| action.view.is_none()),
                "only the declared open action names a view"
            );
        }

        // Phase `a_room_confined_caller_sees_only_its_room`.
        {
            let (host, _) = host();
            let elsewhere = Caller::local_person("a1".repeat(32)).in_room("not-this-room");
            assert!(
                catalogue(&host, &elsewhere).rows.is_empty(),
                "a connection admitted to another room reaches nothing here"
            );
            assert_eq!(
                now(host.invoke(&elsewhere, refresh(tilo_actions::InvocationId::new()))),
                Err(ActionHostError::UnknownAction)
            );
        }

        // Phase `remote_callers_cannot_reach_what_they_are_not_offered`.
        {
            let (host, mut invocations) = host();
            assert_eq!(
                now(host.invoke(&stranger(), refresh(tilo_actions::InvocationId::new()))),
                Err(ActionHostError::UnknownAction)
            );
            let mut elsewhere = refresh(tilo_actions::InvocationId::new());
            elsewhere.target.provider = "8".into();
            assert_eq!(
                now(host.invoke(&owner(), elsewhere)),
                Err(ActionHostError::UnknownAction)
            );
            assert!(invocations.try_recv().is_err());
        }
    }

    #[test]
    fn remote_invocations_are_confirmed_scoped_and_dispatched_once() {
        // Phase `confirmation_is_evidence_the_contract_checks`.
        {
            let (host, mut invocations) = host();
            let mut erase = refresh(tilo_actions::InvocationId::new());
            erase.target.address = ActionAddress::new("files.entry:/tmp/report", "erase");
            assert_eq!(
                now(host.invoke(&owner(), erase.clone())),
                Err(ActionHostError::ConfirmationRequired)
            );
            erase.confirmation = Some(tilo_actions::remote::Confirmation::Phrase("erase".into()));
            assert_eq!(
                now(host.invoke(&owner(), erase.clone())),
                Err(ActionHostError::ConfirmationRequired),
                "the wrong kind of evidence is not accepted"
            );
            assert!(invocations.try_recv().is_err());
            erase.confirmation = Some(tilo_actions::remote::Confirmation::Confirmed);
            now(host.invoke(&owner(), erase)).expect("confirmed");
            assert!(invocations.try_recv().is_ok(), "dispatched once confirmed");
        }

        // Phase `a_launch_without_a_launch_scope_cannot_open_content_by_hash`.
        // A launch opens content only through the scope of the application
        // the launcher authenticated for the calling connection; a caller
        // without one holding a hash opens nothing.
        {
            let (host, _) = host();
            let room = catalogue(&host, &owner())
                .rows
                .iter()
                .find_map(|row| tilo_actions::remote::workspace_provider_room(&row.provider))
                .map(str::to_owned)
                .expect("the workspace is offered");
            let caller = Caller::local_person("a1".repeat(32)).in_room(&room);
            let open = InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: tilo_actions::remote::workspace_provider(&room),
                    address: ActionAddress::new(
                        format!(
                            "{}so.tilo.photos",
                            tilo_actions::remote::APPLICATION_RESOURCE_PREFIX
                        ),
                        tilo_actions::remote::APPLICATION_OPEN_ACTION,
                    ),
                },
                argument: InvokeArgument::Text(blake3::hash(b"a photograph").to_hex().to_string()),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            };
            match now(host.invoke(&caller, open)) {
                Err(ActionHostError::Refused { reason }) => assert!(
                    reason.contains("launcher authenticated"),
                    "refused for its missing launch scope: {reason}"
                ),
                other => panic!("a hash alone must not open content: {other:?}"),
            }
        }

        // Phase `a_retried_remote_invocation_dispatches_once`.
        {
            let (host, mut invocations) = host();
            let id = tilo_actions::InvocationId::new();
            let first = now(host.invoke(&owner(), refresh(id))).expect("admitted");
            assert_eq!(first.invocation, id);
            assert_eq!(first.status, ReceiptStatus::Accepted);
            assert!(invocations.try_recv().is_ok(), "dispatched to the app");

            let retry = now(host.invoke(&owner(), refresh(id))).expect("retry");
            assert_eq!(retry, first);
            assert!(
                invocations.try_recv().is_err(),
                "a retry never reaches the app twice"
            );
            let state = host.observe(&owner(), &id).expect("owner observes");
            assert_eq!(state.invocation(), id);
            assert_eq!(host.observe(&stranger(), &id), None);

            assert_eq!(
                host.cancel(&stranger(), &id),
                Err(ActionHostError::UnknownInvocation)
            );
            host.cancel(&owner(), &id).expect("owner cancels");
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
#[path = "shortcuts_door_tests.rs"]
mod shortcuts_door_tests;

#[cfg(test)]
#[cfg(target_os = "linux")]
#[path = "keyboard_shortcuts_door_tests.rs"]
mod keyboard_shortcuts_door_tests;
