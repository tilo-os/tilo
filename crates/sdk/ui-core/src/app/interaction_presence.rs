// A person's activation of a bound control, published as `Interaction`
// presence.
//
// An agent's broker invocation already reaches peers as a `SemanticInteraction`
// lifecycle. A person pressing the same control through pointer, keyboard or
// touch enters the same action door, so it publishes the same lifecycle, with
// the phases projected by the same `InteractionPhase::from_*` functions. The
// broker replaces the actor with the authenticated principal (a `Person`) and
// assigns the sequence, exactly as for an agent. What peers draw of it is
// decided once, in `presence::interaction`.

/// The attempt being observed, captured before the control runs so the
/// resource, labels and control are the ones the person actually pressed.
struct HumanAttempt {
    interaction: tilo_actions::SemanticInteraction,
    kind: Option<tilo_actions::OperationKind>,
    revision: Option<Vec<automerge::ChangeHash>>,
    local_before: std::collections::HashSet<tilo_actions::InvocationId>,
    broker_before: std::collections::HashSet<BrokerReply>,
}

fn find_action<'a>(
    resources: &'a [tilo_actions::Resource],
    address: &tilo_actions::ActionAddress,
) -> Option<(&'a tilo_actions::Resource, &'a tilo_actions::ActionSpec)> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == address.resource
            && let Some(spec) = resource
                .actions
                .iter()
                .find(|spec| spec.id == address.action)
        {
            return Some((resource, spec));
        }
        find_action(&resource.children, address)
    })
}

impl App {
    /// Whether a presence frame from this window reaches anyone.
    fn presence_has_audience(&self) -> bool {
        self.collaboration.manager.is_connected() || self.presence.broadcast_sink.is_some()
    }

    /// The document revision an attempt is measured against, or `None` when
    /// reading it would close an open edit transaction (an attempt started
    /// mid-edit is never reported as a no-op).
    fn human_attempt_revision(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        (automerge::transaction::Transactable::pending_ops(&self.state.doc) == 0)
            .then(|| self.state.doc.get_heads())
    }

    fn begin_human_attempt(
        &mut self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<HumanAttempt> {
        if !self.presence_has_audience() {
            return None;
        }
        // The unfiltered catalogue: a refused attempt still names what was tried.
        let resources = self.collect_resources();
        let found = find_action(&resources, address);
        let kind = found.map(|(_, spec)| spec.kind);
        let resource = found.map_or_else(
            || tilo_actions::ActionResource {
                id: address.resource.clone(),
                kind: "tilo.unknown".into(),
                label: address.resource.clone(),
                sensitivity: tilo_actions::Sensitivity::Public,
            },
            |(resource, _)| tilo_actions::ActionResource {
                id: resource.id.as_str().to_string(),
                kind: resource.kind.clone(),
                label: resource.label.clone(),
                sensitivity: resource.sensitivity,
            },
        );
        let label = found
            .map(|(_, spec)| spec.label.clone())
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| address.action.clone());
        // Name the control only when its identity survives a rebuild; a
        // frame-local id would point a follower at a different control.
        let anchor = self
            .preferred_action_ui_route(address)
            .and_then(|route| route.path.last().cloned())
            .filter(|id| {
                self.accessibility
                    .semantic_tree
                    .node(id)
                    .is_some_and(|node| node.durable_identity)
            })
            .map_or(tilo_actions::InteractionAnchor::Resource, |id| {
                tilo_actions::InteractionAnchor::SemanticControl { id: id.0 }
            });
        let app_id = crate::collaboration::local_app_id();
        let interaction = tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: tilo_actions::InvocationId::new(),
            sequence: 0,
            // The broker replaces the claimed actor with the authenticated
            // principal before fan-out.
            actor: tilo_actions::InteractionActor {
                id: self.collaboration.local_author.0.clone(),
                kind: tilo_actions::InteractionActorKind::Person,
            },
            phase: tilo_actions::InteractionPhase::Locating,
            action: tilo_actions::InteractionAction {
                id: address.action.clone(),
                label,
                app: (!app_id.trim().is_empty()).then(|| tilo_actions::ActionApp {
                    label: app_id.clone(),
                    id: app_id,
                }),
            },
            resource,
            anchor,
            group: None,
        };
        Some(HumanAttempt {
            interaction,
            kind,
            revision: self.human_attempt_revision(),
            local_before: self.actions.local_invocations.keys().copied().collect(),
            broker_before: self.actions.broker_requests.keys().copied().collect(),
        })
    }

    fn publish_human_phase(
        &mut self,
        interaction: &mut tilo_actions::SemanticInteraction,
        phase: tilo_actions::InteractionPhase,
    ) {
        interaction.sequence = interaction.sequence.saturating_add(1);
        interaction.phase = phase;
        if let Err(error) = interaction.validate() {
            tracing::debug!(%error, "human interaction is not publishable");
            return;
        }
        let owner = interaction
            .action
            .app
            .as_ref()
            .map_or_else(crate::collaboration::local_app_id, |app| app.id.clone());
        let key = crate::presence::PresenceKey::resource(
            owner,
            &interaction.resource.id,
            &interaction.resource.kind,
        );
        self.broadcast_presence(
            key,
            crate::collaboration::PresencePayload::Interaction(Box::new(interaction.clone())),
        );
    }

    /// Run one person-initiated activation of `address` and publish its
    /// lifecycle. The result is returned untouched; presence only observes.
    fn observe_human_activation(
        &mut self,
        address: &tilo_actions::ActionAddress,
        run: impl FnOnce(
            &mut Self,
        )
            -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let Some(attempt) = self.begin_human_attempt(address) else {
            return run(self);
        };
        let result = run(self);
        self.settle_human_attempt(attempt, &result);
        result
    }

    fn settle_human_attempt(
        &mut self,
        attempt: HumanAttempt,
        result: &Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        use tilo_actions::{InteractionPhase, InteractionRejection};
        let HumanAttempt {
            mut interaction,
            kind,
            revision,
            local_before,
            broker_before,
        } = attempt;
        let terminal = match result {
            Ok(crate::widget::ResourceOperationStart::Completed(_)) => {
                let unchanged = kind == Some(tilo_actions::OperationKind::Document)
                    && revision.is_some()
                    && self.human_attempt_revision() == revision;
                Some(if unchanged {
                    InteractionPhase::Rejected {
                        reason: InteractionRejection::NoChange,
                        message: None,
                    }
                } else {
                    InteractionPhase::Committed
                })
            }
            Ok(crate::widget::ResourceOperationStart::Accepted) => {
                // Report the terminal phase under the invocation that will
                // deliver it: a local operation the poll sweep settles, or a
                // request the session action host answers.
                let local = self
                    .actions
                    .local_invocations
                    .keys()
                    .find(|id| !local_before.contains(id))
                    .copied();
                let brokered = self
                    .actions
                    .broker_requests
                    .keys()
                    .find_map(|reply| match reply {
                        BrokerReply::Receipt(id) if !broker_before.contains(reply) => Some(*id),
                        _ => None,
                    });
                if let Some(id) = local.or(brokered) {
                    interaction.invocation = id;
                }
                None
            }
            Err(error) => Some(InteractionPhase::from_door_error(error)),
        };
        self.publish_human_phase(&mut interaction, InteractionPhase::Applying);
        if let Some(phase) = terminal {
            self.publish_human_phase(&mut interaction, phase);
            return;
        }
        // Bounded: an accepted request whose answer never arrives must not
        // grow this forever. Peers expire an unanswered attempt on their own,
        // so dropping one here loses nothing they would still show.
        if self.actions.human_interactions.len() >= 64
            && let Some(evicted) = self.actions.human_interactions.keys().next().copied()
        {
            self.actions.human_interactions.remove(&evicted);
        }
        self.actions
            .human_interactions
            .insert(interaction.invocation, interaction);
    }

    /// A press on a disabled control runs nothing and emits no intent, so the
    /// door never hears of it. It is still an attempt: if the control is
    /// bound to an action, publish its refusal.
    fn observe_press_on_disabled_control(&mut self, event: &WindowEvent, scale: f64) {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let point = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.input.last_pointer = Some(vello::kurbo::Point::new(
                    position.x / scale,
                    position.y / scale,
                ));
                return;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.input.last_pointer,
            WindowEvent::Touch(touch) if touch.phase == winit::event::TouchPhase::Started => Some(
                vello::kurbo::Point::new(touch.location.x / scale, touch.location.y / scale),
            ),
            _ => None,
        };
        let Some(point) = point else {
            return;
        };
        if !self.presence_has_audience() {
            return;
        }
        self.refresh_semantic_tree();
        let Some(hit) = self
            .accessibility
            .semantic_tree
            .iter()
            .filter(|node| {
                node.widget_taffy_id.is_some()
                    && node.bounds.is_some_and(|bounds| bounds.contains(point))
            })
            .min_by(|left, right| {
                let area = |node: &crate::a11y_tree::SemanticNode| {
                    node.bounds.map_or(f64::INFINITY, |bounds| bounds.area())
                };
                area(left).total_cmp(&area(right))
            })
            .filter(|node| node.state.disabled)
            .map(|node| (node.id.clone(), node.durable_identity))
        else {
            return;
        };
        let Some(widget) = self.semantic_node_widget(&hit.0) else {
            return;
        };
        let Some(address) = self
            .inspect_node(widget, |widget| {
                crate::widget::controls::control_action_bindings(widget)
                    .into_iter()
                    .find_map(|binding| match binding.kind {
                        crate::widget::ControlBindingKind::Action(address) => Some(address),
                        _ => None,
                    })
            })
            .ok()
            .flatten()
        else {
            return;
        };
        let Some(mut attempt) = self.begin_human_attempt(&address) else {
            return;
        };
        if hit.1 {
            attempt.interaction.anchor =
                tilo_actions::InteractionAnchor::SemanticControl { id: hit.0.0 };
        }
        // Outside the actor's permission-filtered surface is a denial; still
        // offered but switched off is an unmet precondition.
        let offered =
            tilo_actions::door::resolve(&self.collect_local_resources(), &address).is_some();
        let (reason, message) = if offered {
            (
                tilo_actions::InteractionRejection::Unavailable,
                "the control is disabled",
            )
        } else {
            (
                tilo_actions::InteractionRejection::PermissionDenied,
                "the action is not available to this participant",
            )
        };
        self.publish_human_phase(
            &mut attempt.interaction,
            tilo_actions::InteractionPhase::Rejected {
                reason,
                message: Some(message.into()),
            },
        );
    }

    /// Publish the phase a later lifecycle update gives a person's pending
    /// attempt; terminal phases end it.
    fn advance_human_interaction(
        &mut self,
        invocation: tilo_actions::InvocationId,
        phase: tilo_actions::InteractionPhase,
    ) {
        let Some(mut interaction) = self.actions.human_interactions.remove(&invocation) else {
            return;
        };
        let terminal = phase.is_terminal();
        if interaction.phase != phase {
            self.publish_human_phase(&mut interaction, phase);
        }
        if !terminal {
            self.actions
                .human_interactions
                .insert(invocation, interaction);
        }
    }
}
