//! Route-owned observations of synchronous local input. Snapshot identities and
//! child aliases are frozen before execution; draining never queries a directory.
use super::*;
use crate::local_document_action::{InputRoute, Queue};
use std::{collections::HashMap, rc::Rc};

#[derive(Default)]
pub(super) struct LocalInputCapture {
    queues: HashMap<crate::collaboration::RouteSpec, Queue>,
    pending: HashMap<u64, PendingCapture>,
    retained: std::collections::VecDeque<(crate::local_document_action::CapturedAction, String)>,
    retained_bytes: usize,
    evicted: u64,
    transport_connected: bool,
    // One bounded, inert copy of the last authenticated pre-input surface.
    offline_routes: Vec<InputRoute>,
    recovered_routes: std::collections::HashSet<crate::collaboration::RouteSpec>,
    pub(super) failure: Option<String>,
}

struct PendingCapture {
    capture: crate::local_document_action::CapturedAction,
    route: crate::collaboration::RouteSpec,
    _lease: crate::local_document_action::Lease,
    retry_at: crate::time::Instant,
    retry_delay: crate::time::Duration,
}

impl LocalInputCapture {
    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    #[cfg(test)]
    pub(super) fn pending_count(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub(super) fn retry_now(&mut self) {
        for pending in self.pending.values_mut() {
            pending.retry_at = crate::time::now();
        }
    }

    #[cfg(test)]
    pub(super) fn custody_size(&self) -> usize {
        self.pending
            .values()
            .map(|pending| retained_size(&pending.capture, ""))
            .sum()
    }

    #[cfg(test)]
    pub(super) fn route_count(&self) -> usize {
        self.queues.len()
    }
}

fn retained_size(capture: &crate::local_document_action::CapturedAction, admission: &str) -> usize {
    // Include allocations duplicated in the typed and published forms.
    capture.encoded.payload.len()
        + capture.encoded.type_id.len()
        + capture.revision.len()
        + serde_json::to_vec(&capture.context).map_or(0, |bytes| bytes.len())
        + capture.published.as_ref().map_or(0, |published| {
            published.resource.len()
                + published.action.len()
                + serde_json::to_vec(&published.arguments).map_or(0, |bytes| bytes.len())
        })
        + (capture.original_heads.len() + capture.mutation_hashes.len()) * 32
        + admission.len()
}

impl App {
    /// Private admission evidence, not execution proof or publication authority.
    pub fn local_document_action_admissions(
        &self,
    ) -> impl Iterator<Item = (&crate::local_document_action::CapturedAction, &str)> {
        self.local_input_capture
            .retained
            .iter()
            .map(|(capture, admission)| (capture, admission.as_str()))
    }

    /// Oldest private admissions evicted by the bounded history.
    pub fn local_document_action_admission_evictions(&self) -> u64 {
        self.local_input_capture.evicted
    }

    pub(super) fn begin_local_input_event(
        &mut self,
        event: &WindowEvent,
    ) -> crate::local_document_action::Scope {
        let input = match event {
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_)
            | WindowEvent::Ime(winit::event::Ime::Commit(_)) => true,
            WindowEvent::CursorMoved { .. } => self.input.pointer_buttons != 0,
            _ => false,
        };
        if input {
            self.begin_local_input_capture()
        } else {
            crate::local_document_action::suspend()
        }
    }

    pub(super) fn begin_local_input_capture(&mut self) -> crate::local_document_action::Scope {
        if !self.collaboration.manager.is_connected() {
            let routes = self.local_input_capture.offline_routes.clone();
            if routes.is_empty() && self.actions.action_directory_snapshot.is_none() {
                // No authenticated publication: preserve editing, but do not
                // invent a caller, route, revision or admission.
                self.local_input_capture.failure = Some(crate::i18n::I18nManager::lookup_plain(
                    "history-local-action-unsigned",
                ));
                self.needs_sync = true;
                return crate::local_document_action::suspend();
            }
            for route in &routes {
                if let Some(pin) = route.pin.as_ref().and_then(|pin| {
                    pin.clone()
                        .downcast::<action_directory_snapshot::LocalDocumentDirectoryPin>()
                        .ok()
                }) {
                    if !self
                        .actions
                        .local_document_directory_pins
                        .iter()
                        .any(|old| old.upgrade().is_some_and(|old| Rc::ptr_eq(&old, &pin)))
                    {
                        self.actions
                            .local_document_directory_pins
                            .push(Rc::downgrade(&pin));
                    }
                }
            }
            if !routes.is_empty() {
                return crate::local_document_action::enter_input(routes);
            }
        }
        // Publication runs outside the input scope: refreshing controls or
        // draining a model wake must not acquire the person's input identity.
        let nested = crate::local_document_action::input_is_active();
        let background = crate::local_document_action::suspend();
        if !nested && self.collaboration.manager.is_connected() {
            // Controls left by an earlier model pass are not this input's
            // actions. Service them before freezing the input's snapshot.
            self.dispatch_control_invocations();
        }
        self.publish_action_surface();
        if self.actions.action_directory_snapshot.is_none() {
            drop(background);
            self.local_input_capture.failure = Some(crate::i18n::I18nManager::lookup_plain(
                "history-local-action-unsigned",
            ));
            self.needs_sync = true;
            return crate::local_document_action::suspend();
        }
        let mut routes: Vec<InputRoute> = Vec::new();
        let revision = self
            .actions
            .action_directory_stamp
            .as_ref()
            .map(|(_, revision)| revision.clone());
        let pin = revision
            .as_deref()
            .and_then(|revision| self.pin_local_document_action_directory(revision));
        if pin.is_none() {
            self.local_input_capture_failed("local input directory could not be pinned");
        }
        if let (Some((_, revision)), Some(snapshot)) = (
            self.actions.action_directory_stamp.as_ref(),
            self.actions.action_directory_snapshot.as_ref(),
        ) {
            let mut aliases: HashMap<
                Option<uuid::Uuid>,
                (crate::collaboration::RouteSpec, HashMap<String, String>),
            > = HashMap::new();
            for address in snapshot.actions.keys() {
                match self.actions.bound_publication.target(address) {
                    Ok(Some(target)) => {
                        if let Some(route) = target.binding.broker_route() {
                            let entry = aliases
                                .entry(Some(target.binding.lifetime_id))
                                .or_insert_with(|| (route, HashMap::new()));
                            entry.1.insert(
                                target.address.address.resource.clone(),
                                address.resource.clone(),
                            );
                            entry
                                .1
                                .insert(address.resource.clone(), address.resource.clone());
                        }
                    }
                    Ok(None) => {
                        if let Some(witness) = self
                            .collaboration
                            .manager
                            .primary_route()
                            .or(self.actions.last_published_bound_route.as_ref())
                        {
                            aliases
                                .entry(None)
                                .or_insert_with(|| (witness.route.clone(), HashMap::new()))
                                .1
                                .insert(address.resource.clone(), address.resource.clone());
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "local input publication alias unavailable")
                    }
                }
            }
            for (owner, (route, aliases)) in aliases {
                let queue = self
                    .local_input_capture
                    .queues
                    .entry(route)
                    .or_default()
                    .clone();
                routes.push(InputRoute {
                    owner,
                    queue,
                    revision: if pin.is_some() {
                        revision.clone()
                    } else {
                        String::new()
                    },
                    aliases: Rc::new(aliases),
                    pin: pin.clone().map(|pin| pin as Rc<dyn std::any::Any>),
                });
            }
        }
        if routes.is_empty() && pin.is_some() {
            self.local_input_capture_failed("local input has no published action route");
        }
        // Retain inert snapshot copies, not registered leases. Ordinary online
        // reconciliation can still release all in-flight directory pins.
        self.local_input_capture.offline_routes = routes
            .iter()
            .cloned()
            .map(|mut route| {
                route.pin = pin
                    .as_ref()
                    .map(|pin| Rc::new((**pin).clone()) as Rc<dyn std::any::Any>);
                route
            })
            .collect();
        // Drop the suspension before installing a new input scope so nesting
        // restores the outer host rather than erasing the new scope.
        drop(background);
        crate::local_document_action::enter_input(routes)
    }

    pub(super) fn drain_local_input_capture(&mut self) {
        let mut failures = Vec::new();
        let mut recovery_routes: Vec<_> = self
            .document_routes
            .owners
            .iter()
            .map(|entry| entry.route.clone())
            .collect();
        if let Some(primary) = self.collaboration.manager.primary_route() {
            if self.persistence.records.is_some() || self.persistence.autosave.is_some() {
                recovery_routes.push(primary.route.clone());
            }
        }
        for route in recovery_routes {
            if !self.local_input_capture.recovered_routes.contains(&route) {
                match self.restore_local_input_checkpoint(&route) {
                    Ok(()) => {
                        self.local_input_capture.recovered_routes.insert(route);
                    }
                    // A live owner can be ahead of its durable checkpoint,
                    // including immediately after the edit being drained here.
                    // That is ordinary save progress, not failed capture. Keep
                    // recovery eligible for the next pump; explicit recovery
                    // still reports these conditions to its caller.
                    Err(error)
                        if matches!(
                            error.as_str(),
                            "private action checkpoint is not durable"
                                | "document checkpoint is not durable"
                        ) => {}
                    Err(error) => failures.push(error),
                }
            }
        }
        let now = crate::time::now();
        let queues: Vec<_> = self
            .local_input_capture
            .queues
            .iter()
            .map(|(route, queue)| (route.clone(), queue.clone()))
            .collect();
        for (route, queue) in queues {
            failures.extend(queue.take_refusals());
            for (capture, lease) in queue.drain_leased() {
                let mut request_id = capture.invocation.as_u64_pair().0;
                while self.local_input_capture.pending.contains_key(&request_id) {
                    request_id = request_id.wrapping_add(1);
                }
                if let Err(error) = self.stage_local_action_evidence(&route, &capture, None) {
                    failures.push(error);
                }
                // Transfer custody before enqueueing: send failure must not
                // discard an action already applied to the local document.
                self.local_input_capture.pending.insert(
                    request_id,
                    PendingCapture {
                        capture,
                        route: route.clone(),
                        _lease: lease,
                        retry_at: now,
                        retry_delay: crate::time::Duration::from_secs(1),
                    },
                );
            }
        }
        let connected = self.collaboration.manager.is_connected();
        if connected && !self.local_input_capture.transport_connected {
            for pending in self.local_input_capture.pending.values_mut() {
                pending.retry_at = now;
            }
        }
        self.local_input_capture.transport_connected = connected;
        let mut rejected = Vec::new();
        if connected {
            // A bounded amount of nonblocking enqueue work per pump. Retrying
            // reports is not dispatch: the original UUID, snapshot and mutations
            // are reused, and the document action is never executed again.
            for (request_id, pending) in self
                .local_input_capture
                .pending
                .iter_mut()
                .filter(|(_, pending)| pending.retry_at <= now)
                .take(4)
            {
                let result = self
                    .collaboration
                    .manager
                    .report_local_document_action_to_route(
                        pending.route.clone(),
                        *request_id,
                        pending.capture.clone(),
                    );
                // The transport owns a four-second RPC deadline. Do not
                // enqueue another report behind an ordinary in-flight call.
                let wait = if result.is_ok() {
                    pending.retry_delay.max(crate::time::Duration::from_secs(5))
                } else {
                    pending.retry_delay
                };
                pending.retry_at = now + wait;
                pending.retry_delay =
                    (pending.retry_delay * 2).min(crate::time::Duration::from_secs(30));
                if let Err(error) = result {
                    if matches!(error, runtime_collab::LocalDocumentActionError::Rejected(_)) {
                        rejected.push(*request_id);
                    }
                    failures.push(error.to_string());
                }
            }
        }
        for request_id in rejected {
            self.retire_rejected_local_capture(request_id);
        }
        // Never retire a route's capacity accounting while replies are in
        // flight, including unknown outcomes that still own snapshot leases.
        self.local_input_capture.queues.retain(|route, queue| {
            queue.has_outstanding()
                || self
                    .collaboration
                    .manager
                    .primary_route()
                    .is_some_and(|w| &w.route == route)
                || self
                    .document_routes
                    .owners
                    .iter()
                    .any(|owner| &owner.route == route)
        });
        for error in failures {
            self.local_input_capture_failed(&error);
        }
    }

    /// Restore private reports only for the exact currently owned document.
    /// No action execution, new invocation identity or publication authority is
    /// reconstructed from this metadata.
    pub fn restore_local_input_checkpoint(
        &mut self,
        route: &crate::collaboration::RouteSpec,
    ) -> Result<(), String> {
        let entries = self.local_action_checkpoint_entries(route)?;
        for (checkpoint, receipt) in entries {
            if receipt.as_deref() == Some("{\"local_capture_rejected\":true}") {
                continue;
            }
            let report = checkpoint.report;
            if self
                .local_input_capture
                .pending
                .values()
                .any(|pending| pending.capture.invocation == report.invocation)
                || self
                    .local_input_capture
                    .retained
                    .iter()
                    .any(|(capture, _)| capture.invocation == report.invocation)
            {
                continue;
            }
            let arguments = match report.argument.as_deref() {
                None => serde_json::Map::new(),
                Some(argument) => serde_json::from_str(
                    argument
                        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                        .ok_or("private action checkpoint arguments are not canonical")?,
                )
                .map_err(|error| error.to_string())?,
            };
            let capture = crate::local_document_action::CapturedAction {
                invocation: report.invocation,
                capture_id: report.capture_id,
                revision: report.revision.clone(),
                published: Some(crate::local_document_action::PublishedInvocation {
                    resource: report.resource.clone(),
                    action: report.action.clone(),
                    arguments,
                }),
                encoded: report.encoded.clone(),
                context: report.context.clone(),
                original_heads: report.original_heads.clone(),
                mutation_hashes: report.mutation_hashes.clone(),
            };
            if serde_json::to_vec(&capture.clone().into_report()?).map_err(|e| e.to_string())?
                != serde_json::to_vec(&report).map_err(|e| e.to_string())?
            {
                return Err("private action checkpoint report changed on recovery".into());
            }
            let recovery_pin = if receipt.is_none() {
                checkpoint
                    .directory_row
                    .map(|row| {
                        self.restore_local_directory_row(
                            report.revision.clone(),
                            report.resource.clone(),
                            report.action.clone(),
                            row,
                        )
                        .ok_or("private action recovery exceeds directory pin budget")
                    })
                    .transpose()?
            } else {
                None
            };
            let history = &mut self.local_input_capture;
            if let Some(receipt) = receipt {
                history.retained_bytes += retained_size(&capture, &receipt);
                history.retained.push_back((capture, receipt));
                while history.retained.len() > 128 || history.retained_bytes > 4 * 1024 * 1024 {
                    let (capture, receipt) = history.retained.pop_front().unwrap();
                    history.retained_bytes -= retained_size(&capture, &receipt);
                    history.evicted += 1;
                }
            } else {
                let bytes: usize = history
                    .pending
                    .values()
                    .map(|pending| retained_size(&pending.capture, ""))
                    .sum();
                if history.pending.len() >= 128
                    || bytes + retained_size(&capture, "") > 4 * 1024 * 1024
                {
                    return Err("private action recovery exceeds pending custody budget".into());
                }
                let mut request_id = capture.invocation.as_u64_pair().0;
                while history.pending.contains_key(&request_id) {
                    request_id = request_id.wrapping_add(1);
                }
                let queue = history.queues.entry(route.clone()).or_default();
                let lease = queue.restore_lease(
                    &capture,
                    recovery_pin.map(|pin| pin as Rc<dyn std::any::Any>),
                )?;
                history.pending.insert(
                    request_id,
                    PendingCapture {
                        capture,
                        route: route.clone(),
                        _lease: lease,
                        retry_at: crate::time::now(),
                        retry_delay: crate::time::Duration::from_secs(1),
                    },
                );
            }
        }
        Ok(())
    }

    pub(super) fn local_input_capture_deadline(&self) -> Option<crate::time::Instant> {
        self.collaboration
            .manager
            .is_connected()
            .then(|| {
                self.local_input_capture
                    .pending
                    .values()
                    .map(|pending| pending.retry_at)
                    .min()
            })
            .flatten()
    }

    fn retire_rejected_local_capture(&mut self, request_id: u64) {
        if let Some(pending) = self.local_input_capture.pending.remove(&request_id) {
            if let Err(error) =
                self.stage_local_action_checkpoint(&pending.route, &pending.capture, None, true)
            {
                self.local_input_capture_failed(&error);
            }
        }
    }

    fn local_input_capture_failed(&mut self, error: &str) {
        tracing::warn!(%error, "local document action observation was not confirmed");
        self.local_input_capture.failure = Some(crate::i18n::I18nManager::lookup_plain(
            "history-local-action-unconfirmed",
        ));
        self.needs_sync = true;
    }

    pub(super) fn observe_local_input_capture(
        &mut self,
        request_id: u64,
        result: Result<
            runtime_collab::PublishedInvocationObservation,
            runtime_collab::LocalDocumentActionError,
        >,
    ) {
        let Some(pending) = self.local_input_capture.pending.get(&request_id) else {
            return;
        };
        let invocation = pending.capture.invocation;
        use runtime_collab::LocalDocumentActionError::{Rejected, Uncertain};
        let result = result.and_then(|observation| {
            if observation.invocation != *invocation.as_bytes() {
                return Err(Uncertain(
                    "local action observation invocation mismatch".into(),
                ));
            }
            let result = observation
                .result
                .ok_or_else(|| Uncertain("local action observation has no outcome".into()))?;
            let update: tilo_actions::InvocationUpdate = serde_json::from_str(&result.update_json)
                .map_err(|error| Uncertain(error.to_string()))?;
            if update.invocation().0 != invocation {
                return Err(Uncertain("local action update invocation mismatch".into()));
            }
            if result.uncertain {
                return Err(Uncertain("local action admission was not confirmed".into()));
            }
            // A known terminal refusal need not carry an admission. Release
            // custody before the next input needs the frozen directory's pin.
            if matches!(
                update,
                tilo_actions::InvocationUpdate::Failed { .. }
                    | tilo_actions::InvocationUpdate::Cancelled { .. }
            ) {
                return Err(Rejected("local action observation was refused".into()));
            }
            // Admission is private status only, never completion or a signed
            // document-publication permit.
            result
                .admission_json
                .ok_or_else(|| Uncertain("local action admission was not confirmed".into()))
        });
        match result {
            Ok(admission) => {
                let pending = self
                    .local_input_capture
                    .pending
                    .remove(&request_id)
                    .unwrap();
                if let Err(error) = self.stage_local_action_evidence(
                    &pending.route,
                    &pending.capture,
                    Some(&admission),
                ) {
                    self.local_input_capture_failed(&error);
                }
                let history = &mut self.local_input_capture;
                history.retained_bytes += retained_size(&pending.capture, &admission);
                history.retained.push_back((pending.capture, admission));
                while history.retained.len() > 128 || history.retained_bytes > 4 * 1024 * 1024 {
                    let (capture, admission) = history.retained.pop_front().unwrap();
                    history.retained_bytes -= retained_size(&capture, &admission);
                    history.evicted += 1;
                    tracing::warn!(
                        evicted = history.evicted,
                        "oldest private local admission evicted"
                    );
                }
            }
            Err(Rejected(error)) => {
                self.retire_rejected_local_capture(request_id);
                self.local_input_capture_failed(&error);
            }
            // Unknown or malformed replies keep custody until a bounded,
            // backoff-paced exact-UUID report reconciles their outcome.
            Err(Uncertain(error)) => self.local_input_capture_failed(&error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration::{CollabEvent, CollabRequest, PeerTransport, RouteSpec};
    use std::cell::RefCell;

    struct Peer(Rc<RefCell<Vec<CollabRequest>>>);
    impl PeerTransport for Peer {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, _: CollabRequest) -> Result<(), String> {
            Ok(())
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            Vec::new()
        }
        fn send_to_route(&mut self, _: RouteSpec, request: CollabRequest) -> Result<(), String> {
            self.0.borrow_mut().push(request);
            Ok(())
        }
    }
    #[derive(Debug, serde::Serialize)]
    struct Noop;
    impl tilo_actions::Action for Noop {
        fn apply(&self, _: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
            Ok(())
        }
        fn locus(&self) -> tilo_actions::ExecutionLocus {
            tilo_actions::ExecutionLocus::ClientLocal
        }
    }

    #[test]
    fn retired_route_is_released_after_uncertain_capture_is_reconciled() {
        let mut app = App::new(crate::State::new(), Box::new(crate::widget::Text::new("")));
        let requests = Rc::new(RefCell::new(Vec::new()));
        app.collaboration.manager =
            crate::collaboration::CollaborationManager::with_peer_transport(Peer(requests.clone()));
        let queue = Queue::new();
        app.local_input_capture.queues.insert(
            RouteSpec::app("retired", uuid::Uuid::new_v4()),
            queue.clone(),
        );
        let mut doc = automerge::AutoCommit::new();
        let mut host = crate::action_host::ActionHost::new();
        {
            let _scope = queue.enter("snapshot".into());
            let _invocation = crate::local_document_action::with_invocation(
                crate::local_document_action::PublishedInvocation {
                    resource: "body".into(),
                    action: "read".into(),
                    arguments: serde_json::Map::new(),
                },
            );
            host.try_dispatch(&mut doc, "noop", &Noop).unwrap();
        }
        app.drain_local_input_capture();
        let request_id = requests
            .borrow()
            .iter()
            .find_map(|request| match request {
                CollabRequest::ReportLocalDocumentAction { request_id, .. } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        app.observe_local_input_capture(
            request_id,
            Err(runtime_collab::LocalDocumentActionError::Uncertain(
                "reply lost".into(),
            )),
        );
        app.drain_local_input_capture();
        assert!(queue.has_outstanding());
        assert_eq!(app.local_input_capture.route_count(), 1);
        app.observe_local_input_capture(
            request_id,
            Err(runtime_collab::LocalDocumentActionError::Rejected(
                "worker retired".into(),
            )),
        );
        app.drain_local_input_capture();
        assert!(!queue.has_outstanding());
        assert_eq!(app.local_input_capture.route_count(), 0);
        assert_eq!(app.local_input_capture.pending_count(), 0);
    }

    #[test]
    fn noops_are_submitted_and_authenticated_admissions_have_bounded_private_history() {
        let mut app = App::new(crate::State::new(), Box::new(crate::widget::Text::new("")));
        let requests = Rc::new(RefCell::new(Vec::new()));
        app.collaboration.manager =
            crate::collaboration::CollaborationManager::with_peer_transport(Peer(requests.clone()));
        let route = RouteSpec::app("test", uuid::Uuid::new_v4());
        let queue = Queue::new();
        app.local_input_capture.queues.insert(route, queue.clone());
        let mut doc = automerge::AutoCommit::new();
        let mut host = crate::action_host::ActionHost::new();
        let mut first = None;
        for _ in 0..130 {
            {
                let _scope = queue.enter("snapshot".into());
                let _invocation = crate::local_document_action::with_invocation(
                    crate::local_document_action::PublishedInvocation {
                        resource: "body".into(),
                        action: "read".into(),
                        arguments: serde_json::Map::new(),
                    },
                );
                host.try_dispatch(&mut doc, "noop", &Noop).unwrap();
            }
            app.drain_local_input_capture();
            let (request_id, invocation) = requests
                .borrow()
                .iter()
                .rev()
                .find_map(|request| match request {
                    CollabRequest::ReportLocalDocumentAction { request_id, report } => {
                        assert!(report.mutation_hashes.is_empty());
                        Some((*request_id, report.invocation))
                    }
                    _ => None,
                })
                .expect("read/no-op must be submitted");
            first.get_or_insert(invocation);
            app.observe_local_input_capture(
                request_id,
                Ok(runtime_collab::PublishedInvocationObservation {
                    host: [1; 16],
                    invocation: *invocation.as_bytes(),
                    result: Some(runtime_collab::PublishedInvocationResult {
                        id: request_id,
                        update_json: serde_json::to_string(
                            &tilo_actions::InvocationUpdate::Accepted {
                                invocation: tilo_actions::InvocationId(invocation),
                            },
                        )
                        .unwrap(),
                        admission_json: Some("{\"admitted\":true}".into()),
                        terminal_json: None,
                        uncertain: false,
                        cancellation_requested: false,
                    }),
                }),
            );
            assert!(!queue.has_outstanding());
        }
        assert_eq!(app.local_document_action_admissions().count(), 128);
        assert_eq!(app.local_document_action_admission_evictions(), 2);
        assert!(
            app.local_document_action_admissions()
                .all(|(capture, admission)| {
                    Some(capture.invocation) != first && admission == "{\"admitted\":true}"
                })
        );
    }
}
