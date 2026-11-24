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
pub(super) fn store_disclosure(
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

/// Whether `confirmation` is the person's approve of the asked record this call was kept as.
/// Only the dispatch that admits the call again on their answer carries one, and where a frame
/// enters the device it is refused unless the store that kept the record handed it back
/// ([`tilo_actions::remote::asked::admit_frame`]).
pub(super) fn approved_by_person(
    confirmation: Option<&tilo_actions::remote::Confirmation>,
) -> bool {
    matches!(
        confirmation,
        Some(tilo_actions::remote::Confirmation::Asked(_))
    )
}

/// The person's approve of the call `invocation`, kept as its asked record in `workspace`, as the
/// store that kept it hands it back to the dispatch that admits the call again.
#[cfg(test)]
pub(crate) fn person_approved(
    workspace: &str,
    invocation: tilo_actions::InvocationId,
) -> tilo_actions::remote::Confirmation {
    tilo_actions::remote::Confirmation::Asked(tilo_actions::remote::asked::Approval::new(
        tilo_actions::remote::asked::RecordRef {
            workspace: workspace.to_owned(),
            record: invocation.0.to_string(),
            device: None,
        },
        None,
    ))
}

impl AppBrokerState {
    /// Whether `evidence`, which `caller` carries, is the person's own answer for the call: their
    /// approve of the asked record it was kept as, which only the dispatch sets, or the word of the
    /// host that placed this device's window, forwarded over that host's link
    /// ([`tilo_actions::remote::browser::host_answered`]).
    ///
    /// # Errors
    /// A confirmation another device forwarded here, which answers for nobody.
    pub(super) fn answered_for_person(
        &self,
        caller: &Caller,
        evidence: Option<&tilo_actions::remote::Confirmation>,
    ) -> Result<bool, ActionCallError> {
        if approved_by_person(evidence) {
            return Ok(true);
        }
        tilo_actions::remote::browser::host_answered(self.placement.get(), caller, evidence)
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
        let declared = tilo_actions::remote::declared_effects(&request.target);
        let mut facts = described(facts, descriptor, &request.argument);
        facts.effects = self.worded_effects(caller, &declared);
        // A Store disclosure shows exactly what leaves the workspace, and where to.
        if requires_store_approval(&app, &address.action) {
            facts
                .facts
                .extend(store_disclosure(caller, request)?.facts());
        }
        Ok(facts)
    }

    /// The records `effects`, a called action's declaration says it writes beside its own, as
    /// the person answering the call reads them: each app by the name it shows, and what the
    /// step does there in the words the app that keeps the record publishes for the step, else
    /// in this host's words for it.
    pub(super) fn worded_effects(
        &self,
        caller: &Caller,
        effects: &[tilo_actions::ActionEffect],
    ) -> Vec<tilo_actions::remote::asked::DeclaredEffect> {
        let language = settings_store::appearance::preferred_language();
        effects
            .iter()
            .map(|effect| {
                let app = self.app_title(&effect.app);
                let key = format!(
                    "{}-{}",
                    tilo_resource::label::message_key(&effect.kind),
                    effect.verb
                );
                let what = self.step_label(caller, effect).unwrap_or_else(|| {
                    let worded = crate::i18n::message(
                        language.as_deref(),
                        &key,
                        &tilo_history::FluentArgs::new(),
                    );
                    // A key no catalogue holds reads as the key itself: the verb in words says
                    // what the step does though nothing has named its phrase.
                    if worded == key {
                        effect.verb.replace('-', " ")
                    } else {
                        worded
                    }
                });
                tilo_actions::remote::asked::DeclaredEffect::new(app, what, effect)
            })
            .collect()
    }

    /// The label the app keeping `effect`'s record offers its step under, where the app offers
    /// it to `caller`.
    fn step_label(&self, caller: &Caller, effect: &tilo_actions::ActionEffect) -> Option<String> {
        let rows = self
            .eager_catalogue(
                caller,
                RowScope::App {
                    app_id: &effect.app,
                    provider: None,
                },
            )
            .0;
        rows.iter().find_map(|row| {
            let action = row.action.as_ref()?;
            (action.resource_kind == effect.kind && action.address.action == effect.verb)
                .then(|| action.action_label.trim().to_owned())
                .filter(|label| !label.is_empty())
        })
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
