//! The driver's collaboration peer: what the workspace's broker is to a window, played over the
//! app's own collaboration path. A call an agent or a member makes reaches the app as the broker
//! delivers it ([`CollabEvent::Invocation`]), a cancellation as the caller sends one, and every
//! request the app makes of its peer (a job's progress and answer, an edit it submits for its
//! invoker) is kept for the caller to read. Nothing here dispatches an action: the app's
//! collaboration pump takes the delivery and runs it as it runs any other.
//!
//! The peer also answers what the app asks of its broker, as the broker does: the window's route
//! is acknowledged as the app announces it, an invocation of the app's own action (a person's
//! press, which the app hands its broker before running) is admitted against what the app
//! publishes and delivered back to it, and a request for another provider (opening an app) goes to
//! the session the caller serves the window with ([`Driver::set_session_api`]), whose answer comes
//! back as the request's receipt.
#![allow(
    clippy::expect_used,
    reason = "a caller that delivers through a peer it never connected, or cancels a call it never made, has a broken test: it fails there, naming the step"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use super::Driver;
use crate::collaboration::{CollabEvent, CollabRequest};

/// What passed between the app and its peer.
struct Exchange {
    /// Delivered to the app on its next pump, oldest first.
    inbound: VecDeque<CollabEvent>,
    /// What the app asked of its peer, oldest first.
    sent: Vec<CollabRequest>,
    /// The route the app last announced, until the peer acknowledges it.
    announced: Option<crate::collaboration::RouteSpec>,
}

/// The peer the driver connects the app to ([`Driver::connect_peer`]).
struct RecordingPeer(Rc<RefCell<Exchange>>);

impl crate::collaboration::PeerTransport for RecordingPeer {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        let mut exchange = self.0.borrow_mut();
        if let CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            ..
        } = &request
        {
            exchange.announced = Some(crate::collaboration::RouteSpec {
                app_id: app_id.clone(),
                cluster_id: *cluster_id,
                document: record.clone(),
            });
        }
        exchange.sent.push(request);
        Ok(())
    }

    fn take_primary_route_result(
        &mut self,
    ) -> Option<Result<crate::collaboration::RouteSpec, String>> {
        self.0.borrow_mut().announced.take().map(Ok)
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        self.0.borrow_mut().inbound.drain(..).collect()
    }

    fn join_route(
        &mut self,
        _: crate::collaboration::RouteSpec,
        _: crate::resource::ResourceType,
    ) -> Result<(), String> {
        Ok(())
    }

    fn send_to_route(
        &mut self,
        _: crate::collaboration::RouteSpec,
        request: CollabRequest,
    ) -> Result<(), String> {
        self.send(request)
    }

    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}

/// How the session finally answered a forwarded request, as its receipt says it; `None` while it
/// has not finished, since what it says first (that it took the request) may still end refused.
fn receipt_status(
    arrival: &crate::arriving::Arrival<
        tilo_actions::InvocationUpdate,
        tilo_actions::remote::ActionCallError,
    >,
) -> Option<tilo_actions::remote::ReceiptStatus> {
    use crate::arriving::Arrival;
    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::ReceiptStatus;
    Some(match arrival {
        Arrival::Failed(error) => ReceiptStatus::Rejected {
            error: error.clone(),
        },
        Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => ReceiptStatus::Completed {
            output: output.clone(),
        },
        Arrival::Ready(InvocationUpdate::Failed { error, .. }) => ReceiptStatus::Failed {
            error: error.clone(),
        },
        Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => ReceiptStatus::Cancelled {
            message: message.clone(),
        },
        Arrival::Pending { .. } | Arrival::Ready(_) => return None,
    })
}

impl Peer {
    /// Deliver a call to the app on its next pump, under a fresh wire id, for `caller`: what the
    /// effect it runs submits for them is decided as theirs, reaching no further than the call.
    fn deliver(
        &mut self,
        invocation: tilo_actions::InvocationId,
        resource: &str,
        action: &str,
        argument: Option<String>,
        caller: Caller,
    ) {
        let id = self.next_wire_id;
        self.next_wire_id += 1;
        self.calls.push((invocation, id));
        let Caller { actor, context, .. } = caller.clone();
        self.running.insert(invocation, (id, caller));
        self.exchange
            .borrow_mut()
            .inbound
            .push_back(CollabEvent::Invocation {
                id,
                invocation,
                resource: resource.to_owned(),
                action: action.to_owned(),
                argument,
                actor,
                context: context.map(Box::new),
            });
    }

    /// Hand the app `answer` to what it asked of its broker, on its next pump.
    fn answer(&self, answer: Option<tilo_actions::remote::RemoteActionMessage>) {
        if let Some(message) = answer {
            self.exchange
                .borrow_mut()
                .inbound
                .push_back(CollabEvent::RemoteAction { message });
        }
    }

    /// Admit `request`, an invocation of the app's own action by the person at the window, as
    /// the broker does: against `resources`, what the app publishes. Admitted, it is accepted and
    /// delivered back to the app for `actor`; refused, the app hears why in its receipt.
    ///
    /// A call whose action asks first and that carries no yes is kept as the device keeps its
    /// asked record, accepted and not run ([`Driver::asking`]); the same call sent again with the
    /// person's approve ([`Driver::confirm_asked`]) answers whatever the action asks, as the
    /// broker takes it, and its handler reads the approve from the call's context.
    fn admit_own(
        &mut self,
        resources: &[tilo_actions::ResourceSpec],
        request: tilo_actions::remote::InvokeRequest,
        actor: tilo_actions::Actor,
    ) {
        use tilo_actions::ConfirmationPolicy;
        use tilo_actions::remote::{
            ActionCallError, Confirmation, InvocationReceipt, InvokeArgument, ReceiptStatus,
            RemoteActionMessage,
        };
        let address = &request.target.address;
        let spec = tilo_actions::api::resolve(resources, address);
        let slot = request.argument.to_slot();
        let need = spec.map(|spec| match &request.argument {
            InvokeArgument::Json(arguments) => spec.need_for(Some(arguments), None).kind,
            _ => spec.need_for(None, slot.as_deref()).kind,
        });
        // An edit an effect submits for its caller is decided as the caller's, reaching no
        // further than the call it continues; once that call ended it continues nothing.
        let continued = match request.on_behalf_of {
            Some(running) => match self.running_caller(running) {
                Some(caller) if need.is_none_or(|need| need <= caller.reach) => Some(caller),
                _ => {
                    self.answer(tilo_actions::remote::refuse(
                        &RemoteActionMessage::Invoke(request),
                        ActionCallError::NotPermitted,
                    ));
                    return;
                }
            },
            None => None,
        };
        let policy = spec.map(|spec| match &request.argument {
            InvokeArgument::Json(arguments) => spec.confirmation_for_json(arguments),
            argument => spec.confirmation_for_argument(argument.to_slot().as_deref()),
        });
        let confirmation = match (request.approval(), &policy) {
            (Some(_), Some(ConfirmationPolicy::Confirm)) => Some(Confirmation::Confirmed),
            (Some(_), Some(ConfirmationPolicy::Phrase { phrase })) => {
                Some(Confirmation::Phrase(phrase.clone()))
            }
            (Some(_), _) => None,
            (None, _) => request.confirmation.clone(),
        };
        // Nobody asked for an effect's edit, and the person at the device is never asked about it.
        if confirmation.is_none()
            && continued.is_none()
            && let Some(spec) = spec
            && policy.is_some_and(|policy| policy != ConfirmationPolicy::None)
        {
            let invocation = request.invocation;
            self.asked = Some(AskedCall {
                name: spec.label.clone(),
                request,
                actor,
            });
            self.answer(Some(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Accepted,
            })));
            return;
        }
        let admitted = match &request.argument {
            InvokeArgument::Json(arguments) => {
                tilo_actions::api::admit(resources, address, arguments, confirmation.as_ref())
                    .map(drop)
            }
            InvokeArgument::None => tilo_actions::api::admit(
                resources,
                address,
                &serde_json::Map::new(),
                confirmation.as_ref(),
            )
            .map(drop),
            // Typed text binds to the action's own input in the app; the broker checks only
            // that the action is offered.
            InvokeArgument::Text(_) => spec
                .map(drop)
                .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone())),
        };
        if let Err(error) = admitted {
            let refusal = ActionCallError::from_api(&error);
            self.refused.push((request.invocation, refusal.clone()));
            self.answer(tilo_actions::remote::refuse(
                &RemoteActionMessage::Invoke(request),
                refusal,
            ));
            return;
        }
        let invocation = request.invocation;
        // The app runs what it asked for itself: its receipt says what the app first answered,
        // a job taken or the call's end, with what it made.
        self.receipts_owed.push(invocation);
        let caller = if let Some(caller) = continued {
            caller
        } else {
            // The handler reads the person's yes from the call it runs, set by the host that
            // admitted it from the call itself.
            let context = match request.approval() {
                Some(approval) => {
                    let mut context = request.context.clone().unwrap_or_default();
                    context.approval = Some(approval.clone());
                    Some(context)
                }
                None => request.context.clone(),
            };
            Caller {
                actor,
                context,
                reach: spec.map_or(tilo_actions::ActionKind::Read, |spec| spec.contract.kind),
            }
        };
        self.deliver(invocation, &address.resource, &address.action, slot, caller);
    }

    /// Answer each of the app's own calls whose delivery the app answered since, with its first
    /// answer as the call's receipt. Whether one was answered.
    fn answer_owed_receipts(&mut self) -> bool {
        use tilo_actions::InvocationUpdate;
        use tilo_actions::remote::{InvocationReceipt, ReceiptStatus, RemoteActionMessage};
        let mut answered = Vec::new();
        {
            let exchange = self.exchange.borrow();
            self.receipts_owed.retain(|invocation| {
                let first = exchange.sent.iter().find_map(|request| match request {
                    CollabRequest::UpdateInvocation { update, .. }
                        if update.invocation() == *invocation =>
                    {
                        Some(update.clone())
                    }
                    _ => None,
                });
                let Some(first) = first else {
                    return true;
                };
                let status = match first {
                    InvocationUpdate::Succeeded { output, .. } => {
                        ReceiptStatus::Completed { output }
                    }
                    InvocationUpdate::Failed { error, .. } => ReceiptStatus::Failed { error },
                    InvocationUpdate::Cancelled { message, .. } => {
                        ReceiptStatus::Cancelled { message }
                    }
                    _ => ReceiptStatus::Accepted,
                };
                answered.push(InvocationReceipt {
                    invocation: *invocation,
                    status,
                });
                false
            });
        }
        let any = !answered.is_empty();
        for receipt in answered {
            self.answer(Some(RemoteActionMessage::Receipt(receipt)));
        }
        any
    }

    /// Who the call `invocation` this peer delivered was admitted for, while it runs.
    fn running_caller(&self, invocation: tilo_actions::InvocationId) -> Option<Caller> {
        let (wire_id, caller) = self.running.get(&invocation)?;
        let ended = self.exchange.borrow().sent.iter().any(|request| {
            matches!(request, CollabRequest::UpdateInvocation { id, update }
                if id == wire_id && update.is_terminal())
        });
        (!ended).then(|| caller.clone())
    }
}

/// Who a call the peer delivered runs for: its caller, the context it was admitted in, and how far
/// it was admitted to reach.
#[derive(Clone)]
pub(super) struct Caller {
    pub(super) actor: tilo_actions::Actor,
    pub(super) context: Option<tilo_actions::ActionContext>,
    /// The kind of action the call runs, as the broker caps a call's children: an effect on the
    /// person's own device needs no more than reading of the workspace, and still reaches the
    /// edits it submits.
    pub(super) reach: tilo_actions::ActionKind,
}

/// A call of the app's own that waits for the person's yes, kept as the device keeps its asked
/// record ([`Driver::asking`]).
struct AskedCall {
    request: tilo_actions::remote::InvokeRequest,
    actor: tilo_actions::Actor,
    /// The action's name, as its card shows it.
    name: String,
}

/// The driver's side of its peer: the exchange, and the wire id of each call it delivered.
pub(super) struct Peer {
    exchange: Rc<RefCell<Exchange>>,
    calls: Vec<(tilo_actions::InvocationId, u64)>,
    next_wire_id: u64,
    /// How many of the app's requests the peer has answered as its broker.
    served: usize,
    /// The session other providers' requests go to ([`Driver::set_session_api`]).
    session: Option<crate::RemoteActions>,
    /// The requests handed to the session whose answer has not come yet.
    forwarded: Vec<(
        tilo_actions::InvocationId,
        crate::remote_actions::InvocationCall,
    )>,
    /// The providers the caller serves, and what decides the window's shown actions.
    served_by_caller: super::workspace_side::Served,
    /// The app's own call that waits for the person's yes.
    asked: Option<AskedCall>,
    /// Who each call it delivered runs for, by invocation, with the wire id it went under.
    running: std::collections::HashMap<tilo_actions::InvocationId, (u64, Caller)>,
    /// The app's own calls delivered back to it whose receipt waits for its first answer.
    receipts_owed: Vec<tilo_actions::InvocationId>,
    /// The app's own calls the peer refused, by invocation, with why ([`Driver::own_refusal`]).
    refused: Vec<(
        tilo_actions::InvocationId,
        tilo_actions::remote::ActionCallError,
    )>,
}

impl Driver {
    /// Connect the app to a peer that stands for the workspace's broker: calls delivered with
    /// [`Self::deliver_call`] reach the app as the broker delivers them, and what the app answers
    /// is kept ([`Self::call_updates`], [`Self::peer_requests`]).
    pub fn connect_peer(&mut self) {
        if self.peer.is_some() {
            // The window's workspace is already in place: a second connect would drop what the
            // app asked of it since.
            return;
        }
        let exchange = Rc::new(RefCell::new(Exchange {
            inbound: VecDeque::new(),
            sent: Vec::new(),
            announced: None,
        }));
        self.app.collaboration.manager =
            crate::collaboration::CollaborationManager::with_peer_transport(RecordingPeer(
                Rc::clone(&exchange),
            ));
        // A hosted component's window says which record it carries, as its connection to the
        // workspace does when it opens, and the workspace admits it there: the calls of the
        // component's own actions the workspace delivers are then the window's to run.
        if let Some(route) = crate::bound_dispatch::callback_binding(&*self.app.root_widget)
            .and_then(|binding| binding.broker_route())
        {
            let said = self.app.collaboration.manager.send(CollabRequest::Hello {
                app_id: route.app_id,
                cluster_id: route.cluster_id,
                record: route.document,
                resource_type: None,
                handle: None,
            });
            assert!(said, "the driver's peer takes the window's Hello");
        }
        self.peer = Some(Peer {
            exchange,
            calls: Vec::new(),
            next_wire_id: 1,
            served: 0,
            session: None,
            forwarded: Vec::new(),
            served_by_caller: super::workspace_side::Served::default(),
            asked: None,
            running: std::collections::HashMap::new(),
            receipts_owed: Vec::new(),
            refused: Vec::new(),
        });
        self.run_frame();
    }

    /// The app's own call that waits for the person's yes, kept as the device keeps it in its asked
    /// record ([`crate::app::App::ask_through_device`]): its action as the window
    /// publishes it, and the action's name its card shows.
    #[must_use]
    pub fn asking(&self) -> Option<(tilo_actions::ActionAddress, String)> {
        let asked = self.peer.as_ref()?.asked.as_ref()?;
        Some((asked.request.target.address.clone(), asked.name.clone()))
    }

    /// Allow the call that waits for the person's yes, as they do on its card: the device sends it
    /// again with their approve ([`Self::approval`]), the workspace admits it, and the app runs it.
    /// Answers the action it ran; `None` when nothing waits.
    pub fn confirm_asked(&mut self) -> Option<tilo_actions::ActionAddress> {
        let approval = self.approval();
        let asked = self.peer.as_mut()?.asked.take()?;
        let address = asked.request.target.address.clone();
        let mut request = asked.request;
        request.confirmation = Some(tilo_actions::remote::Confirmation::Asked(approval));
        let resources = self.app.publication_resources();
        self.peer_mut().admit_own(&resources, request, asked.actor);
        self.run_frame();
        Some(address)
    }

    /// Decline the call that waits for the person's yes, as they do on its card: nothing of it
    /// runs. Answers the action declined; `None` when nothing waits.
    pub fn decline_asked(&mut self) -> Option<tilo_actions::ActionAddress> {
        let asked = self.peer.as_mut()?.asked.take()?;
        self.run_frame();
        Some(asked.request.target.address)
    }

    /// The approve a call kept as its asked record comes back with once the person allowed it:
    /// what the driver, standing in for the device, sends such a call again with.
    #[must_use]
    pub fn approval(&self) -> tilo_actions::remote::asked::Approval {
        self.approval_typing(None)
    }

    /// [`Self::approval`] carrying what the person typed into the ask's one field (the phrase a
    /// destructive action has them type).
    #[must_use]
    pub fn approval_typing(&self, typed: Option<&str>) -> tilo_actions::remote::asked::Approval {
        let mut approval = tilo_actions::remote::asked::Approval::new(
            tilo_actions::remote::asked::RecordRef {
                workspace: crate::permissions::LOCAL_WORKSPACE.to_owned(),
                record: format!("invocations/{}", tilo_actions::InvocationId::new().0),
                device: None,
            },
            None,
        );
        approval
            .typed
            .extend(typed.map(|typed| zeroize::Zeroizing::new(typed.to_owned())));
        approval
    }

    /// What the caller serves in the workspace's place, the peer connected first when it is not.
    pub(super) fn served_mut(&mut self) -> &mut super::workspace_side::Served {
        &mut self.connected_peer().served_by_caller
    }

    /// The driver's peer, connected first when it is not.
    fn connected_peer(&mut self) -> &mut Peer {
        if self.peer.is_none() {
            self.connect_peer();
        }
        self.peer_mut()
    }

    /// Whether the peer holds something for the app that it has not taken yet.
    pub(super) fn peer_delivering(&self) -> bool {
        self.peer
            .as_ref()
            .is_some_and(|peer| !peer.exchange.borrow().inbound.is_empty())
    }

    /// Why the peer refused the app's own call `invocation`, as its caller hears it: none when
    /// it admitted the call.
    pub(super) fn own_refusal(
        &self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::ApiError> {
        let peer = self.peer.as_ref()?;
        peer.refused
            .iter()
            .find(|(refused, _)| *refused == invocation)
            .map(|(_, error)| crate::session_api::api_error(error))
    }

    fn peer_mut(&mut self) -> &mut Peer {
        self.peer
            .as_mut()
            .expect("connect the driver's peer first (Driver::connect_peer)")
    }

    /// Deliver a call of `action` on the resource `resource` with JSON `arguments`, made by
    /// `actor` within `context` (an agent's run, the person it acts for), as the broker delivers
    /// an admitted call: the app runs it on its next frames, for that invoker, and answers on its
    /// peer. The call's invocation, whose answers [`Self::call_updates`] reads.
    ///
    /// # Panics
    /// When the peer is not connected.
    pub fn deliver_call(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Value,
        actor: tilo_actions::Actor,
        context: Option<tilo_actions::ActionContext>,
    ) -> tilo_actions::InvocationId {
        let invocation = context
            .as_ref()
            .and_then(|context| context.source_invocation)
            .unwrap_or_default();
        // The call reaches as far as the kind of action it runs, as the broker admits it.
        let address = tilo_actions::ActionAddress::new(resource, action);
        let reach = [self.resources(), self.app.publication_resources()]
            .iter()
            .find_map(|resources| tilo_actions::api::resolve(resources, &address).cloned())
            .map_or(tilo_actions::ActionKind::Read, |spec| spec.contract.kind);
        self.peer_mut().deliver(
            invocation,
            resource,
            action,
            Some(format!(
                "{}{arguments}",
                tilo_actions::ARGUMENTS_JSON_PREFIX
            )),
            Caller {
                actor,
                context,
                reach,
            },
        );
        self.take_delivery();
        invocation
    }

    /// Serve the window the session `client` is: what the app asks of another provider (opening
    /// an app in the workspace) goes to it, and its answer comes back to the app as the request's
    /// receipt, as the broker relays it. Connects the peer when it is not connected. With
    /// `workspace`, the window joins that workspace first, as a window does once it is placed in
    /// one: what it opens opens there.
    ///
    /// # Panics
    /// If `workspace` is not a workspace id.
    pub fn set_session_api(&mut self, client: crate::RemoteActions, workspace: Option<String>) {
        if self.peer.is_none() {
            self.connect_peer();
        }
        self.peer_mut().session = Some(client);
        if let Some(workspace) = workspace {
            let cluster_id =
                uuid::Uuid::parse_str(&workspace).expect("the workspace is a workspace id");
            // A hosted component's window placed in the workspace its record lives in still says
            // which record it carries: the calls of the component's own actions stay its to run.
            let carried = crate::bound_dispatch::callback_binding(&*self.app.root_widget)
                .and_then(|binding| binding.broker_route())
                .filter(|route| route.cluster_id == cluster_id);
            let (app_id, record) = carried.map_or_else(
                || (crate::collaboration::local_app_id(), None),
                |route| (route.app_id, route.document),
            );
            assert!(
                self.app.collaboration.manager.send(CollabRequest::Hello {
                    app_id,
                    cluster_id,
                    record,
                    resource_type: None,
                    handle: None,
                }),
                "the window announces its workspace to its peer"
            );
        }
        self.run_frame();
    }

    /// Whether the window's session finished a request the peer handed it, or the window's shown
    /// actions are owed a decision: an answer a frame is owed to bring to the app.
    pub(super) fn peer_owes_frame(&self) -> bool {
        self.peer.as_ref().is_some_and(|peer| {
            peer.served_by_caller.owes_decision()
                || peer.receipts_owed.iter().any(|invocation| {
                    peer.exchange.borrow().sent.iter().any(|request| {
                        matches!(request, CollabRequest::UpdateInvocation { update, .. }
                            if update.invocation() == *invocation)
                    })
                })
                || peer
                    .forwarded
                    .iter()
                    .any(|(_, call)| call.with(receipt_status).is_some())
        })
    }

    /// Answer what the app asked of its broker since the peer last looked, as the broker does:
    /// an invocation of the app's own action is admitted against what the app publishes now and
    /// delivered back to it with its receipt; a request for another provider is answered by what
    /// the caller serves it with ([`Self::serve_provider`]) or goes to the window's session, and
    /// each answer the session gave comes back as its receipt; the actions the window shows are
    /// decided as it asks, and again once permissions change ([`Self::decide_shown_actions`]).
    /// Whether anything is on its way to the app.
    pub(super) fn serve_peer(&mut self) -> bool {
        use tilo_actions::remote::{ActionCallError, RemoteActionMessage};
        let Some(peer) = self.peer.as_mut() else {
            return false;
        };
        let asked = {
            let exchange = peer.exchange.borrow();
            let asked = exchange
                .sent
                .get(peer.served..)
                .unwrap_or_default()
                .iter()
                .filter(|request| {
                    matches!(
                        request,
                        CollabRequest::RemoteAction {
                            message: RemoteActionMessage::Invoke(_),
                        } | CollabRequest::DecideShownActions { .. }
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            peer.served = exchange.sent.len();
            asked
        };
        let receipted = peer.answer_owed_receipts();
        if asked.is_empty() && peer.forwarded.is_empty() && !peer.served_by_caller.owes_decision() {
            return receipted;
        }
        // The broker admits against the surface the app published to it, where a hosted
        // component's resources stand at the addresses it binds. A resource the app publishes
        // only when it is asked for (an item past the page a listing shows, a live row the tree
        // leaves out) is the app's to offer too: the broker asks the window for the one address a
        // call names, as the app's own API resolves it, never withholding it for not being listed.
        let mut resources = self.app.publication_resources();
        for request in &asked {
            let CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            } = request
            else {
                continue;
            };
            // The app hands the call under the alias its publication binds; the tree knows the
            // address the component publishes it by.
            let Ok(Some(target)) = self
                .app
                .actions
                .bound_publication
                .target(&request.target.address)
            else {
                continue;
            };
            let address = &target.address.address;
            if tilo_actions::api::resolve(&resources, &request.target.address).is_some() {
                continue;
            }
            let Some(resource) = crate::bound_dispatch::local_target::resolve_on_demand(
                self.app.root_widget.as_ref(),
                &self.app.state.doc,
                address,
            ) else {
                continue;
            };
            let Some(resource) = self
                .app
                .permission_ctx
                .permitted_resource(&crate::collaboration::local_app_id(), resource)
            else {
                continue;
            };
            let Ok(resource) = self
                .app
                .actions
                .bound_publication
                .project(&target.binding, resource)
            else {
                continue;
            };
            if !resources.iter().any(|known| known.id == resource.id) {
                resources.push(resource);
            }
        }
        let actor = tilo_actions::Actor::Member(self.app.collaboration.local_author.clone());
        let peer = self.peer_mut();
        for request in asked {
            let request = match request {
                CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Invoke(request),
                } => request,
                asked => {
                    if let Some(verdicts) = peer.served_by_caller.asked(&asked) {
                        peer.exchange.borrow_mut().inbound.push_back(verdicts);
                    }
                    continue;
                }
            };
            if request.target.provider == tilo_actions::remote::OWN_PROVIDER {
                peer.admit_own(&resources, request, actor.clone());
                continue;
            }
            if let Some(receipt) = peer.served_by_caller.invoke(&request, &actor) {
                peer.answer(Some(receipt));
                continue;
            }
            match &peer.session {
                Some(session) => {
                    let invocation = request.invocation;
                    let call = session.invoke_continuing(
                        request.on_behalf_of,
                        invocation,
                        request.target,
                        request.argument,
                        request.confirmation,
                    );
                    peer.forwarded.push((invocation, call));
                }
                None => peer.answer(tilo_actions::remote::refuse(
                    &RemoteActionMessage::Invoke(request),
                    ActionCallError::Unavailable {
                        reason: "no session serves this window".into(),
                    },
                )),
            }
        }
        let mut answered = Vec::new();
        peer.forwarded
            .retain(|(invocation, call)| match call.with(receipt_status) {
                Some(status) => {
                    answered.push(tilo_actions::remote::InvocationReceipt {
                        invocation: *invocation,
                        status,
                    });
                    false
                }
                None => true,
            });
        for receipt in answered {
            peer.answer(Some(RemoteActionMessage::Receipt(receipt)));
        }
        if let Some(verdicts) = peer.served_by_caller.unasked() {
            peer.exchange.borrow_mut().inbound.push_back(verdicts);
        }
        !peer.exchange.borrow().inbound.is_empty()
    }

    /// Ask the app to stop the call `invocation`, as its caller does: the job decides where it
    /// can stop and answers with what it ended as ([`Self::call_updates`]).
    ///
    /// # Panics
    /// When the peer is not connected, or `invocation` was not delivered through it.
    pub fn cancel_call(&mut self, invocation: tilo_actions::InvocationId) {
        let peer = self.peer_mut();
        let id = peer
            .calls
            .iter()
            .find_map(|(call, id)| (*call == invocation).then_some(*id))
            .expect("the call was delivered through this driver's peer");
        peer.exchange
            .borrow_mut()
            .inbound
            .push_back(CollabEvent::InvocationCancellation { id });
        self.take_delivery();
    }

    /// Every update the app sent its peer about the call `invocation`, oldest first: its
    /// progress, then what it ended as.
    #[must_use]
    pub fn call_updates(
        &self,
        invocation: tilo_actions::InvocationId,
    ) -> Vec<tilo_actions::InvocationUpdate> {
        let Some(peer) = &self.peer else {
            return Vec::new();
        };
        peer.exchange
            .borrow()
            .sent
            .iter()
            .filter_map(|request| match request {
                CollabRequest::UpdateInvocation { update, .. }
                    if update.invocation() == invocation =>
                {
                    Some(update.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// Why the calls delivered through the peer that ended refused, failed or cancelled did,
    /// oldest first.
    #[must_use]
    pub fn call_failures(&self) -> Vec<String> {
        let Some(peer) = &self.peer else {
            return Vec::new();
        };
        peer.exchange
            .borrow()
            .sent
            .iter()
            .filter_map(|request| match request {
                CollabRequest::UpdateInvocation {
                    update: tilo_actions::InvocationUpdate::Failed { error, .. },
                    ..
                } => Some(format!("{}: {}", error.code, error.message)),
                CollabRequest::UpdateInvocation {
                    update: tilo_actions::InvocationUpdate::Cancelled { .. },
                    ..
                } => Some("cancelled".to_owned()),
                _ => None,
            })
            .collect()
    }

    /// Every request the app made of its peer, oldest first.
    #[must_use]
    pub fn peer_requests(&self) -> Vec<CollabRequest> {
        self.peer
            .as_ref()
            .map(|peer| peer.exchange.borrow().sent.clone())
            .unwrap_or_default()
    }

    /// Deliver `event` to the app as its peer would, on its next frames: a collaboration event
    /// no call above covers (an edit from another member, a presence update, a view to follow).
    /// Connects the peer when it is not.
    pub fn deliver_peer_event(&mut self, event: CollabEvent) {
        self.connected_peer()
            .exchange
            .borrow_mut()
            .inbound
            .push_back(event);
        self.take_delivery();
    }

    /// Take up what the peer delivered, laying nothing out: the call or event runs as the model
    /// pass runs it, and the frame that shows what it changed is the one whoever reads the window
    /// runs. An action answers once its own change is made, never after a frame of the view.
    fn take_delivery(&mut self) {
        let _ = self.app.take_up_delivery();
    }
}
