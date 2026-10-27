//! Capture and revalidate the exact local request a person confirms.
use super::{
    AutoCommit, ControlContinuationArguments, DeferredIntent, OrdinaryConfirmation,
    OrdinaryConfirmationSnapshot, PreparedActionIntent, Widget, control_input_surface,
    encode_resource_action, legacy_document, resources_for_control_target, unique_control_spec,
    validate_deferred_arguments,
};

impl OrdinaryConfirmation {
    pub(crate) fn prepare(
        root: &dyn Widget,
        doc: &AutoCommit,
        target: legacy_document::LocalResourceTarget,
        domain: &crate::ControlDomain,
        address: tilo_actions::ActionAddress,
        arguments: ControlContinuationArguments,
    ) -> Result<Self, tilo_actions::DoorError> {
        if domain.closed() {
            return Err(Self::stale());
        }
        let resources = resources_for_control_target(root, doc, &target, &address)?;
        let (resource, spec) = unique_control_spec(&resources, &address)?;
        let context = spec.history_context(resource.id.as_str(), &resource.kind, &resource.label);
        let snapshot = Self::snapshot(
            &resources,
            &address,
            &arguments,
            tilo_resource::origin_now_ms(),
        )?;
        Ok(Self {
            target,
            address,
            arguments,
            domain: domain.downgrade(),
            snapshot,
            context,
            asked: None,
            confirming: false,
        })
    }

    /// This request as the door's answer made it: the invocation needs the
    /// person's go-ahead (the room's grants ask first) even where the action
    /// declares no confirmation, so the person is asked, told who decides,
    /// instead of the request waiting unseen for an answer nobody asks for.
    #[must_use]
    pub(crate) fn asking_first(mut self) -> Self {
        if matches!(
            self.snapshot.contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        ) {
            self.asked = Some(format!(
                "This workspace asks before \u{201c}{}\u{201d} reaches outside it, and you decide: confirm to go ahead.",
                self.snapshot.label
            ));
        }
        self
    }

    /// This request as the app asked for it: the person's pick is asked
    /// before it goes ahead though the action declares no confirmation
    /// (Delete in a message's menu), in the action's own words.
    #[must_use]
    pub(crate) fn confirming(mut self) -> Self {
        self.confirming = true;
        self
    }

    /// Whether Undo takes the action back, by its contract: what lets Shift
    /// skip its question ([`crate::widget::shift_skips_confirmation`]).
    pub(crate) fn can_be_undone(&self) -> bool {
        self.snapshot.contract.can_be_undone()
    }

    /// Why the person is asked, when the room's grants ask rather than the
    /// action itself.
    pub(crate) fn asked(&self) -> Option<&str> {
        self.asked.as_deref()
    }
    fn snapshot(
        resources: &[tilo_actions::Resource],
        address: &tilo_actions::ActionAddress,
        arguments: &ControlContinuationArguments,
        built_at: i64,
    ) -> Result<OrdinaryConfirmationSnapshot, tilo_actions::DoorError> {
        let (resource, spec) = unique_control_spec(resources, address)?;
        let (argument, json) = arguments.as_door_arguments();
        // A deferred spec retains a creation draft when its provider offers
        // one; without one (a verb with no parameters) it is built like any
        // other action, so a destructive one can still be confirmed.
        let retains_draft = matches!(spec.input(), tilo_actions::ActionInput::Deferred { .. })
            && spec
                .continuation_identity(tilo_actions::ContinuationQuery::Draft)
                .is_some();
        let intent = if retains_draft {
            validate_deferred_arguments(spec, json, argument)?;
            PreparedActionIntent::Deferred(DeferredIntent::capture(spec)?)
        } else {
            // An action carries the time it was built; the confirmation is
            // of the same action whenever the person confirms it.
            PreparedActionIntent::Encoded(tilo_resource::with_origin_clock(built_at, || {
                encode_resource_action(resources, address, json, argument)
            })?)
        };
        Ok(OrdinaryConfirmationSnapshot {
            built_at,
            resource_id: resource.id.as_str().to_owned(),
            resource_kind: resource.kind.clone(),
            action_id: spec.id.clone(),
            label: spec.label.clone(),
            description: spec.description.clone(),
            locus: spec.locus,
            kind: spec.kind,
            operation_type: spec.operation_type.clone(),
            // What these arguments do decides the question (a field edit
            // that removes an item asks; one that renames it does not).
            contract: tilo_actions::ActionContract {
                confirmation: match json {
                    Some(json) => spec.confirmation_for_json(json),
                    None => spec.confirmation_for_argument(argument),
                },
                ..spec.contract.clone()
            },
            params: spec.params().to_vec(),
            input: control_input_surface(spec),
            continuation: match &intent {
                PreparedActionIntent::Deferred(intent) => Some(intent.draft().clone()),
                PreparedActionIntent::Encoded(_) => spec.continuation_identity(
                    tilo_actions::ContinuationQuery::Exact(arguments.query()),
                ),
            },
            intent,
        })
    }
    pub(crate) fn validate_owner(
        &self,
        domain: &crate::ControlDomain,
    ) -> Result<(), tilo_actions::DoorError> {
        let original = self.domain.upgrade().ok_or_else(Self::stale)?;
        if original.closed() || domain.closed() || !original.same_owner_incarnation(domain) {
            return Err(Self::stale());
        }
        Ok(())
    }
    pub(crate) fn validate(
        &self,
        root: &dyn Widget,
        doc: &AutoCommit,
        domain: &crate::ControlDomain,
    ) -> Result<(), tilo_actions::DoorError> {
        self.validate_owner(domain)?;
        let resources = resources_for_control_target(root, doc, &self.target, &self.address)?;
        let mut current = Self::snapshot(
            &resources,
            &self.address,
            &self.arguments,
            self.snapshot.built_at,
        )?;
        // Absence at emission is not retroactively replaced with provider proof.
        if self.snapshot.continuation.is_none() {
            current.continuation = None;
        }
        if current != self.snapshot {
            return Err(Self::stale());
        }
        Ok(())
    }
    fn stale() -> tilo_actions::DoorError {
        tilo_actions::DoorError::Unavailable {
            code: "stale_ordinary_confirmation".into(),
            reason: "The original confirmation request is no longer valid".into(),
        }
    }
    pub(crate) fn intent(&self) -> &PreparedActionIntent {
        &self.snapshot.intent
    }
    pub(crate) fn kind(&self) -> tilo_actions::OperationKind {
        self.snapshot.kind
    }
    pub(crate) fn confirmation(&self) -> &tilo_actions::ConfirmationPolicy {
        static ASKED: tilo_actions::ConfirmationPolicy = tilo_actions::ConfirmationPolicy::Confirm;
        if self.asked.is_some() || self.confirming {
            &ASKED
        } else {
            &self.snapshot.contract.confirmation
        }
    }
    pub(crate) fn risk(&self) -> tilo_actions::RiskLevel {
        self.snapshot.contract.risk
    }

    /// The confirmed action's label and description, as the person asks it.
    pub(crate) fn action_text(&self) -> (&str, Option<&str>) {
        (
            self.snapshot.label.as_str(),
            self.snapshot.description.as_deref(),
        )
    }
}
