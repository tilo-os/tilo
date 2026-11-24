//! What a person reads before they answer a call someone acting for them
//! makes.
//!
//! An agent, or an application, never confirms an action by its own word:
//! where an action asks for confirmation, or the workspace's grants let the
//! caller take it only after asking, the call is refused with what the
//! person is shown ([`ActionCallError::ConfirmationRequired`]), and the
//! dispatch keeps it as its asked invocation record. Their approve admits the
//! same call again carrying it ([`InvokeRequest::approval`]); nothing else
//! does.

use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};

use super::AppBrokerState;
use super::action_provider::RowScope;

/// These Store actions disclose signed content outside the workspace.
/// Their confirmation must come from the host, not the publishing app.
pub(super) fn requires_store_approval(app: &str, action: &str) -> bool {
    app == "so.tilo.store" && matches!(action, "submit-report" | "appeal")
}

/// The Store disclosure `request` puts to the person, signed as `caller`'s
/// person or this device's.
///
/// # Errors
/// No identity to sign with, or an `operation` that does not decode: the
/// request is refused with why.
fn store_disclosure(
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<crate::apps::app_actions::ModerationConsentPreview, ActionCallError> {
    let signing_key = match caller.user_public_key() {
        Some(key) => *key,
        None => {
            let workspace = caller
                .workspace()
                .ok_or_else(|| ActionCallError::Unavailable {
                    reason: "no workspace to sign the Store disclosure in".into(),
                })?;
            tilo_identity::trusted_device::current(workspace)
                .map(|(_, certificate)| certificate.user_public_key)
                .map_err(|error| ActionCallError::Unavailable {
                    reason: format!("no identity to sign the Store disclosure with: {error}"),
                })?
        }
    };
    crate::apps::app_actions::ModerationConsentPreview::from_argument(
        &request.target.address.action,
        &request.argument,
        &signing_key,
    )
    .map_err(|reason| ActionCallError::InvalidRequest { reason })
}

impl AppBrokerState {
    /// Resolve only this provider's live publication. Host-owned providers
    /// have no application publication; unrelated discovery cannot change them.
    pub(super) fn approval_publication(&self, provider: &str) -> (u64, u64) {
        let peer = if provider.starts_with(super::canonical_route::PROVIDER_PREFIX) {
            self.canonical_token_directory(provider)
                .ok()
                .map(|root| root.peer_id)
        } else if super::launchable::is_launchable_provider(provider) {
            self.launchable.surface_for(provider).and_then(|retained| {
                self.publishers
                    .surfaces
                    .lock()
                    .ok()?
                    .values()
                    .find_map(|surface| {
                        (surface.live
                            && surface.app_id == retained.app_id
                            && surface.route == retained.route)
                            .then_some(surface.peer_id)
                    })
            })
        } else {
            tilo_failure::ignored!(
                "a provider that is not a peer number names a canonical, launchable or provider one",
                provider.parse::<u64>().ok()
            )
        };
        peer.and_then(|peer| {
            self.approval_publications
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&peer)
                .copied()
        })
        .unwrap_or_default()
    }

    /// Capture before asking, and reconstruct from the original request at
    /// admission. Republishing this provider invalidates the pending answer.
    pub(crate) fn approval_binding(
        &self,
        actor: &tilo_action_policy::MemberId,
        workspace: &str,
        request: &InvokeRequest,
    ) -> super::ApprovalBinding {
        super::ApprovalBinding {
            actor: actor.clone(),
            provider: request.target.provider.clone(),
            workspace: workspace.into(),
            revision: request.revision.clone(),
            publication: self.approval_publication(&request.target.provider),
            invocation: request.invocation,
            resource: request.target.address.resource.clone(),
            action: request.target.address.action.clone(),
            argument_digest: super::argument_digest(request.argument.to_slot().as_deref()),
        }
    }

    /// The binding a confirmation of `offered`, invoked by `actor` with `argument` as
    /// `invocation`, is held to.
    #[cfg(test)]
    pub(super) fn offered_approval_binding(
        &self,
        actor: &tilo_action_policy::MemberId,
        offered: &(super::PublishedSurface, super::PublishedAction),
        argument: Option<&str>,
        invocation: tilo_actions::InvocationId,
    ) -> super::ApprovalBinding {
        super::ApprovalBinding {
            actor: actor.clone(),
            provider: offered.0.peer_id.to_string(),
            workspace: offered.0.route.cluster_id.clone(),
            revision: None,
            publication: self.approval_publication(&offered.0.peer_id.to_string()),
            invocation,
            resource: offered.1.resource.clone(),
            action: offered.1.action.clone(),
            argument_digest: super::argument_digest(argument),
        }
    }

    pub(super) fn consume_answer(
        &self,
        confirmation: Option<&tilo_actions::remote::Confirmation>,
        binding: &super::ApprovalBinding,
    ) -> bool {
        match confirmation {
            Some(tilo_actions::remote::Confirmation::Approved(token)) => {
                self.take_approval(token, binding)
            }
            _ => false,
        }
    }

    /// Resolve a directory's original identity and selected content revision.
    fn approval_directory(
        &self,
        provider: &str,
        workspace: &str,
        revision: Option<&str>,
    ) -> Option<super::ActionDirectoryRoot> {
        // A directory's contents can move without replacing its root.
        revision.filter(|revision| !revision.is_empty())?;
        let directory = if provider.starts_with(super::canonical_route::PROVIDER_PREFIX) {
            self.canonical_token_directory(provider).ok()?
        } else {
            let peer = provider.parse::<u64>().ok()?;
            self.action_directory_roots()
                .into_iter()
                .find(|candidate| candidate.peer_id == peer && candidate.cluster_id == workspace)?
        };
        (directory.cluster_id == workspace).then_some(directory)
    }

    /// What the person is shown of `request`, which `caller` makes: the action
    /// as the catalogue offers it to `caller`, the arguments it would run with,
    /// and what a grant given with their answer would allow.
    ///
    /// # Errors
    /// A Store disclosure that cannot be shown as it would leave (an `operation` that does not
    /// decode, or no identity to sign it with) is refused with why rather than asked about.
    pub(super) fn asked_facts(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<AskedFacts, ActionCallError> {
        let mut facts = AskedFacts::by(crate::workspaces::asking::asked_by(caller));
        let provider = request.target.provider.as_str();
        let address = &request.target.address;
        let offered = self
            .eager_catalogue(caller, RowScope::Provider(provider))
            .0
            .into_iter()
            .find_map(|row| {
                let app = row.app_id;
                (row.provider == provider)
                    .then_some(row.action)
                    .flatten()
                    .filter(|descriptor| &descriptor.address == address)
                    .map(|descriptor| (app, descriptor))
            });
        let Some((app, descriptor)) = offered else {
            // An application that publishes a directory names the action by its address.
            if let Some(workspace) = caller.workspace()
                && self
                    .approval_directory(provider, workspace, request.revision.as_deref())
                    .is_some()
            {
                facts.verb.clone_from(&address.action);
                facts.on = Some(address.resource.clone());
            }
            return Ok(facts);
        };
        let mut facts = described(facts, descriptor, &request.argument);
        // A Store disclosure shows exactly what leaves the workspace, and where to.
        if requires_store_approval(&app, &address.action) {
            facts
                .facts
                .extend(store_disclosure(caller, request)?.facts());
        }
        Ok(facts)
    }

    /// `error`, an answer to `caller`'s `request`, stating what the person is shown where it
    /// waits for their yes and says nothing of it yet ([`Self::asked_facts`]).
    pub(super) fn with_asked_facts(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        error: ActionCallError,
    ) -> ActionCallError {
        match error {
            ActionCallError::ConfirmationRequired(facts) if *facts == AskedFacts::default() => {
                match self.asked_facts(caller, request) {
                    Ok(facts) => ActionCallError::asks(facts),
                    Err(refused) => refused,
                }
            }
            other => other,
        }
    }
}

/// `facts`, stating what `descriptor`, the action as the person is offered it, does with
/// `argument`: the arguments it would run with, and what a grant given with their answer allows.
pub(super) fn described(
    mut facts: AskedFacts,
    descriptor: tilo_actions::ActionDescriptor,
    argument: &tilo_actions::remote::InvokeArgument,
) -> AskedFacts {
    if let Some(draft) = super::rendered_draft(&descriptor, argument) {
        facts.facts = draft
            .summary()
            .into_iter()
            .map(|(label, value)| Fact::new(label, value))
            .collect();
        // What the person may change before they approve: the fields the contract lets them.
        facts.form = draft
            .fields
            .into_iter()
            .filter(|field| field.editable)
            .collect();
    }
    let kind = descriptor.contract.kind;
    facts.destructive = kind == tilo_actions::ActionKind::Outside;
    facts.verb = descriptor.action_label;
    facts.on = Some(descriptor.resource_label.clone()).filter(|label| !label.is_empty());
    if !descriptor.resource_kind.is_empty() {
        facts.plural = Some(tilo_actions::plural(&descriptor.resource_label));
        facts.kind = descriptor.resource_kind;
    }
    facts.effect = Some(kind);
    facts
}
