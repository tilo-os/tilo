//! Ordinary application workers shared by durable native and hosted owners.
//! The host supplies current authority and committed bytes; this module owns
//! only the real application runner and its original recovery custody.
use crate::{
    application::{ApplicationDefinition, ComponentCloseReport, PreparedComponentInputs},
    collaboration::CollaborationManager,
    launch::LaunchError,
    permissions::PermissionContext,
};
use std::{cell::RefCell, rc::Rc};

#[cfg(unix)]
pub mod private_actions;

#[cfg(unix)]
pub mod managed_sender;

pub use crate::invocation_scope::{
    PublishedInvocationAuthorizer, PublishedInvocationLease, PublishedInvocationRequest,
};

pub struct HostWorker {
    app: crate::App,
    permissions: Rc<RefCell<PermissionContext>>,
    /// Last presence handed to the host per participant, for
    /// [`HostWorker::take_participant_presence_changes`].
    published_presence:
        std::collections::BTreeMap<tilo_actions::ParticipantId, crate::widget::ParticipantPresence>,
}

/// One change in a remote participant's presence since the last drain.
#[derive(Clone, Debug, PartialEq)]
pub enum ParticipantPresenceUpdate {
    /// New or changed caret/selection (or label) of this participant.
    Changed(crate::widget::ParticipantPresence),
    /// The participant has no presence any more (left, or no caret).
    Removed(tilo_actions::ParticipantId),
}
impl HostWorker {
    pub fn mount(
        definition: &ApplicationDefinition,
        inputs: PreparedComponentInputs,
        collaboration: CollaborationManager,
        permissions: Rc<RefCell<PermissionContext>>,
    ) -> Result<Self, LaunchError> {
        Self::mount_inner(definition, inputs, collaboration, permissions)
    }
    /// Bind funded negative custody before the actual factory. This does not
    /// activate a reader, authenticate a Source, or install disclosure authority.
    pub fn mount_with_retirement(
        definition: &ApplicationDefinition,
        mut inputs: PreparedComponentInputs,
        collaboration: CollaborationManager,
        permissions: Rc<RefCell<PermissionContext>>,
        retirement: crate::semantic_document_read::retirement::MountRetirement,
    ) -> Result<Self, LaunchError> {
        inputs.context.bind_retirement(retirement)?;
        Self::mount_inner(definition, inputs, collaboration, permissions)
    }
    fn mount_inner(
        definition: &ApplicationDefinition,
        inputs: PreparedComponentInputs,
        mut collaboration: CollaborationManager,
        permissions: Rc<RefCell<PermissionContext>>,
    ) -> Result<Self, LaunchError> {
        // The actual admitted record exists before the factory. Defaults or
        // migrations performed by the factory cannot become a trusted baseline.
        inputs.context.require_invocation_provenance()?;
        collaboration.require_document_authorization();
        let mut component =
            definition.instantiate(inputs.context, inputs.target, inputs.restore)?;
        let binding = component.callback_binding();
        if let Err(error) = binding
            .records
            .require_invocation_provenance(&binding.checkout)
        {
            return Err(component.setup_error(LaunchError::Operation(error)));
        }
        let mut app = crate::App::from_component_with_collaboration(
            component,
            inputs.runner_inputs,
            collaboration,
        )?;
        if let Err(error) = binding
            .records
            .require_invocation_provenance(&binding.checkout)
        {
            app.close_components();
            return Err(LaunchError::ComponentSetup {
                source: Box::new(LaunchError::Operation(error)),
                record: Box::new(binding.checkout),
                recovery: crate::launch::LaunchRecovery::new(
                    Some(binding.records),
                    app.take_component_close_reports(),
                ),
            });
        }
        app.require_published_invocation_authorizer();
        // A host worker has no local person: its own view publishes the
        // participant-independent catalogue schema, and every remote
        // invoker resolves against its own participant view.
        app.set_participant_host(true);
        Ok(Self {
            app,
            permissions,
            published_presence: std::collections::BTreeMap::new(),
        })
    }
    /// An agent task starts working on this record: create its own
    /// participant view (Writer: caret at the document end) before its first
    /// invocation, so its presence exists from the spawn. The participant
    /// must be host-derived from the admitted task
    /// ([`tilo_actions::ParticipantId::for_agent_task`]), the same one the
    /// broker sets on the task's invocations; they then resolve against this
    /// view. Idempotent; returns whether a view was newly created.
    pub fn join_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool {
        self.app.join_participant(participant)
    }
    /// The agent task ended: drop its participant view. Idempotent.
    pub fn leave_participant(&mut self, participant: &tilo_actions::ParticipantId) {
        self.app.leave_participant(participant);
    }
    /// Name shown beside the participant's caret (the agent's display name).
    pub fn set_participant_label(
        &mut self,
        participant: &tilo_actions::ParticipantId,
        label: &str,
    ) {
        self.app.set_participant_label(participant, label);
    }
    /// Snapshot of every agent participant's current presence (caret or
    /// selection, from its own view), keyed and shaped like a local caret.
    pub fn participant_presence(&self) -> Vec<crate::widget::ParticipantPresence> {
        self.app.participant_presence()
    }
    /// What changed since the previous call: poll after each
    /// [`Self::pump`] and forward each update under the participant's agent
    /// identity. Only differences are returned, so an idle agent costs
    /// nothing; a participant that disappears yields one `Removed`.
    pub fn take_participant_presence_changes(&mut self) -> Vec<ParticipantPresenceUpdate> {
        let current = self.app.participant_presence();
        participant_presence_changes(&mut self.published_presence, current)
    }
    /// Install once for the exact host worker connection. Required dispatch
    /// derives explicit per-job scopes and carries original contributors into
    /// document publication; the concrete final owner must evaluate them again.
    pub fn install_published_invocation_authorizer(
        &mut self,
        authorizer: PublishedInvocationAuthorizer,
    ) -> Result<(), String> {
        self.app.install_published_invocation_authorizer(authorizer)
    }
    /// Recompute time-sensitive permissions in the host before every pump.
    pub fn pump(&mut self, current: PermissionContext) -> Result<(), String> {
        self.app.published_invocation_health()?;
        *self.permissions.borrow_mut() = current;
        self.app.pump_hosted_model();
        self.app.published_invocation_health()?;
        if !self.app.collaboration_is_connected() {
            return Err("application collaboration transport failed; outcome unknown".into());
        }
        Ok(())
    }
    pub fn close(&mut self) -> Vec<ComponentCloseReport> {
        self.app.close_components();
        self.app.take_component_close_reports()
    }
    pub fn app_mut(&mut self) -> &mut crate::App {
        &mut self.app
    }
    pub fn permissions(&self) -> &Rc<RefCell<PermissionContext>> {
        &self.permissions
    }
}

/// Diff `current` against what was last handed to the host, and remember it.
pub(crate) fn participant_presence_changes(
    published: &mut std::collections::BTreeMap<
        tilo_actions::ParticipantId,
        crate::widget::ParticipantPresence,
    >,
    current: Vec<crate::widget::ParticipantPresence>,
) -> Vec<ParticipantPresenceUpdate> {
    let current: std::collections::BTreeMap<_, _> = current
        .into_iter()
        .map(|presence| (presence.participant.clone(), presence))
        .collect();
    let mut updates: Vec<_> = published
        .keys()
        .filter(|participant| !current.contains_key(*participant))
        .cloned()
        .map(ParticipantPresenceUpdate::Removed)
        .collect();
    updates.extend(
        current
            .iter()
            .filter(|(participant, presence)| published.get(*participant) != Some(*presence))
            .map(|(_, presence)| ParticipantPresenceUpdate::Changed(presence.clone())),
    );
    *published = current;
    updates
}

#[cfg(test)]
mod participant_presence_tests {
    use super::*;

    fn presence(task: &str, x: u32) -> crate::widget::ParticipantPresence {
        crate::widget::ParticipantPresence {
            participant: tilo_actions::ParticipantId::for_agent_task(task).unwrap(),
            label: Some("Scribe".into()),
            key: crate::PresenceKey::Widget {
                path: vec![crate::PathSegment::Field("wp_block".into())],
            },
            payload: crate::PresencePayload::Caret { pos: x as usize },
        }
    }

    #[test]
    fn presence_drain_reports_only_changes_and_removals() {
        let mut published = std::collections::BTreeMap::new();
        let first = participant_presence_changes(&mut published, vec![presence("a", 1)]);
        assert_eq!(
            first,
            vec![ParticipantPresenceUpdate::Changed(presence("a", 1))]
        );
        assert!(
            participant_presence_changes(&mut published, vec![presence("a", 1)]).is_empty(),
            "an idle agent costs nothing"
        );
        let moved =
            participant_presence_changes(&mut published, vec![presence("a", 5), presence("b", 0)]);
        assert_eq!(
            moved,
            vec![
                ParticipantPresenceUpdate::Changed(presence("a", 5)),
                ParticipantPresenceUpdate::Changed(presence("b", 0)),
            ]
        );
        let left = participant_presence_changes(&mut published, vec![presence("b", 0)]);
        assert_eq!(
            left,
            vec![ParticipantPresenceUpdate::Removed(
                tilo_actions::ParticipantId::for_agent_task("a").unwrap()
            )]
        );
    }
}

/// Preserve the original failed component's record/view retry owners.
/// Hosts retain these reports until durable acknowledgement or recovery export.
pub fn retain_setup_error(error: LaunchError, reports: &mut Vec<ComponentCloseReport>) {
    if let LaunchError::ComponentSetup {
        source,
        record,
        recovery,
    } = error
    {
        // The worker retains setup errors on the thread that launched them.
        let Ok((records, nested)) = recovery.into_parts() else {
            retain_setup_error(*source, reports);
            return;
        };
        reports.extend(nested);
        reports.push(ComponentCloseReport {
            recovery: None,
            view_error: None,
            view_retry: None,
            record_error: None,
            record_retry: records,
            record_retry_target: Some(record.id),
            teardown_error: None,
        });
        retain_setup_error(*source, reports);
    }
}
