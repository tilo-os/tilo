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
    pub(super) failure: Option<String>,
}

struct PendingCapture {
    capture: crate::local_document_action::CapturedAction,
    _route: crate::collaboration::RouteSpec,
    _lease: crate::local_document_action::Lease,
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
        // Standalone and disconnected views retain their existing local editing
        // semantics. An absent broker is not an authenticated admission failure.
        if !self.collaboration.manager.is_connected() {
            return crate::local_document_action::suspend();
        }
        // Publication runs outside the input scope: refreshing controls or
        // draining a model wake must not acquire the person's input identity.
        let nested = crate::local_document_action::input_is_active();
        let background = crate::local_document_action::suspend();
        if !nested {
            // Controls left by an earlier model pass are not this input's
            // actions. Service them before freezing the input's snapshot.
            self.dispatch_control_invocations();
        }
        self.publish_action_surface();
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
                        if let Some(witness) = self.collaboration.manager.primary_route() {
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
        // Drop the suspension before installing a new input scope so nesting
        // restores the outer host rather than erasing the new scope.
        drop(background);
        crate::local_document_action::enter_input(routes)
    }

    pub(super) fn drain_local_input_capture(&mut self) {
        let mut failures = Vec::new();
        for (route, queue) in &self.local_input_capture.queues {
            failures.extend(queue.take_refusals());
            for (capture, lease) in queue.drain_leased() {
                let invocation = capture.invocation;
                let request_id = invocation.as_u64_pair().0;
                if self.local_input_capture.pending.contains_key(&request_id) {
                    failures.push("local action observation replies are still pending".into());
                    continue;
                }
                match self
                    .collaboration
                    .manager
                    .report_local_document_action_to_route(
                        route.clone(),
                        request_id,
                        capture.clone(),
                    ) {
                    Ok(()) => {
                        self.local_input_capture.pending.insert(
                            request_id,
                            PendingCapture {
                                capture,
                                _route: route.clone(),
                                _lease: lease,
                            },
                        );
                    }
                    Err(error) => failures.push(error),
                }
            }
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
        result: Result<runtime_collab::PublishedInvocationObservation, String>,
    ) {
        let Some(pending) = self.local_input_capture.pending.get(&request_id) else {
            return;
        };
        let invocation = pending.capture.invocation;
        let result = result.and_then(|observation| {
            if observation.invocation != *invocation.as_bytes() {
                return Err("local action observation invocation mismatch".into());
            }
            let result = observation
                .result
                .ok_or("local action observation has no outcome")?;
            if result.uncertain || result.admission_json.is_none() {
                return Err("local action admission was not confirmed".into());
            }
            let update: tilo_actions::InvocationUpdate =
                serde_json::from_str(&result.update_json).map_err(|error| error.to_string())?;
            if update.invocation().0 != invocation
                || matches!(
                    update,
                    tilo_actions::InvocationUpdate::Failed { .. }
                        | tilo_actions::InvocationUpdate::Cancelled { .. }
                )
            {
                return Err("local action observation was refused".into());
            }
            // Admission is private status only, not a document-publication permit.
            Ok(result.admission_json.unwrap())
        });
        match result {
            Ok(admission) => {
                let pending = self
                    .local_input_capture
                    .pending
                    .remove(&request_id)
                    .unwrap();
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
            // Unknown or malformed outcomes retain the exact capture, lease and
            // directory pin. Never replace their identity or retry as new work.
            Err(error) => self.local_input_capture_failed(&error),
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
