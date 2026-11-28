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
) -> Result<crate::app_actions::ModerationConsentPreview, ActionCallError> {
    let signing_key = if let Some(key) = caller.user_public_key() {
        *key
    } else {
        let workspace = caller
            .workspace()
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "no workspace to sign the Store disclosure in".into(),
            })?;
        tilo_identity::trusted_device::certificate(workspace)
            .map(|certificate| certificate.user_public_key)
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("no identity to sign the Store disclosure with: {error}"),
            })?
    };
    crate::app_actions::ModerationConsentPreview::from_argument(
        &request.target.address.action,
        &request.argument,
        &signing_key,
    )
    .map_err(|reason| ActionCallError::InvalidRequest { reason })
}

/// Whether the person answered `caller`'s call already: `confirmation` is their approve of the
/// asked record it was kept as, which only the dispatch that admits it again on their answer
/// carries (where a frame enters the device it is refused unless the store that kept the record
/// handed it back, [`tilo_actions::remote::asked::admit_frame`]), or the call is a step of their
/// Shortcut whose saved rules consent to it in advance
/// ([`Caller::is_consented_in_advance`]).
pub(super) fn approved_by_person(
    caller: &Caller,
    confirmation: Option<&tilo_actions::remote::Confirmation>,
) -> bool {
    caller.is_consented_in_advance()
        || matches!(
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
    /// ([`tilo_action_services::browser::host_answered`]).
    ///
    /// # Errors
    /// A confirmation another device forwarded here, which answers for nobody.
    pub(super) fn answered_for_person(
        &self,
        caller: &Caller,
        evidence: Option<&tilo_actions::remote::Confirmation>,
    ) -> Result<bool, ActionCallError> {
        if approved_by_person(caller, evidence) {
            return Ok(true);
        }
        tilo_action_services::browser::host_answered(self.placement.get(), caller, evidence)
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
        let mut facts =
            AskedFacts::by(tilo_workspace_service::workspaces::asking::asked_by(caller));
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
            })
            // A registered provider's action on one of the rows its reading publishes (a saved
            // network, a paired accessory): the eager listing holds the resources in front alone,
            // and its own catalogue names the row at the address the call asks.
            .or_else(|| {
                let registered = self.answering(&request.target)?;
                let descriptor = self.services.providers.as_ref()?.descriptor(
                    &registered,
                    caller,
                    address,
                    request.argument.to_slot().as_deref(),
                )?;
                Some((registered.app.to_string(), descriptor))
            });
        let Some((app, descriptor)) = offered else {
            // Nothing the catalogue offers `caller` names the action, which is no reason for a
            // question the person's yes lets through to say nothing of it: it says what the call
            // is from the provider's own declaration, and what it would run with, a secret never
            // among them (a call nothing here declares shows none of its arguments).
            facts.verb.clone_from(&address.action);
            facts.verb = facts.verb.replace('-', " ");
            // An application that publishes a directory names the action by its address.
            facts.on = match caller.workspace() {
                Some(workspace)
                    if self
                        .approval_directory(provider, workspace, request.revision.as_deref())
                        .is_some() =>
                {
                    Some(address.resource.clone())
                }
                _ => resource_in_words(&address.resource),
            };
            facts.facts = self.declared_arguments(caller, request);
            return Ok(facts);
        };
        let declared = tilo_workspace_records::effects::declared_effects(&request.target);
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

    /// The arguments `request` would run with, as the provider answering the action declares
    /// them: each by its name as the declaration writes it, with the value the call carries, and
    /// a secret never (the declaration marks it, and no record of the call keeps its value).
    /// Empty where nothing here declares the action, which then shows none of its arguments.
    fn declared_arguments(&self, caller: &Caller, request: &InvokeRequest) -> Vec<Fact> {
        let Some(providers) = self.services.providers.as_ref() else {
            return Vec::new();
        };
        let address = &request.target.address;
        let declared = providers
            .in_workspace(&request.target)
            .and_then(|answers| answers.params(&request.target))
            .or_else(|| {
                let registered = self.registered(&request.target.provider)?;
                providers
                    .descriptor(
                        &registered,
                        caller,
                        address,
                        request.argument.to_slot().as_deref(),
                    )
                    .map(|descriptor| descriptor.params)
            });
        let (Some(params), tilo_actions::remote::InvokeArgument::Json(arguments)) =
            (declared, &request.argument)
        else {
            return Vec::new();
        };
        params
            .into_iter()
            .filter(|param| param.ty != tilo_actions::ParamType::Secret)
            .filter_map(|param| {
                if param.name.starts_with('$') {
                    return None;
                }
                let value = arguments.get(param.name.as_ref())?;
                let shown = match value {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                Some(Fact::new(param.name.to_string(), shown))
            })
            .collect()
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
                    let worded = tilo_workspace_service::i18n::message(
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

/// What the person is shown of `descriptor`, the action of `request` their answer runs: the
/// action's label on the resource it acts on, what it would run with, and whether Undo takes
/// the change back, asked of `caller`, whoever carries the call.
pub(super) fn asked_of(
    caller: &Caller,
    descriptor: tilo_actions::ActionDescriptor,
    request: &InvokeRequest,
) -> AskedFacts {
    described(
        AskedFacts::by(tilo_workspace_service::workspaces::asking::asked_by(caller)),
        descriptor,
        &request.argument,
    )
}

/// The record `resource` names, in the words a person reads: its last part, each `-` a space
/// (`.../mail-accounts` → "mail accounts"). The whole resource where it has no parts, and none
/// where it is empty.
fn resource_in_words(resource: &str) -> Option<String> {
    let last = resource.rsplit('/').next().unwrap_or(resource);
    (!last.is_empty()).then(|| last.replace('-', " "))
}

/// `error`, the policy's answer to `caller`'s `request` of `descriptor`, naming what it asks of
/// the person where it asks them first: the action as its provider describes it, what it would
/// run with and who asks. The policy alone knows only that it asks.
pub(super) fn stated(
    caller: &Caller,
    descriptor: &tilo_actions::ActionDescriptor,
    request: &InvokeRequest,
    error: ActionCallError,
) -> ActionCallError {
    match error {
        ActionCallError::ConfirmationRequired(_) => {
            ActionCallError::asks(asked_of(caller, descriptor.clone(), request))
        }
        other => other,
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

#[cfg(test)]
mod tests {
    use super::resource_in_words;

    /// A record nothing in the catalogue names is still said in words the person reads, and one
    /// that names no part says nothing rather than a slash.
    #[test]
    fn a_record_is_named_by_its_last_part_in_words() {
        assert_eq!(
            resource_in_words("workspace.permissions:5541b1c6/mail-accounts").as_deref(),
            Some("mail accounts")
        );
        assert_eq!(
            resource_in_words("mail-accounts").as_deref(),
            Some("mail accounts")
        );
        assert_eq!(resource_in_words(""), None);
    }
}
