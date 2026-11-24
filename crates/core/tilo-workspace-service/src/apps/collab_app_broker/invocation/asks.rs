#[allow(
    clippy::wildcard_imports,
    reason = "one part of admission: its shared state and imports reach this through the parent glob"
)]
use super::*;

use tilo_actions::remote::asked::{AskedFacts, Take};

/// Why an admission did not admit an invocation: a reason of its own, or the person's yes it
/// waits for, with what they are shown when the dispatch keeps the call as its asked record.
#[derive(Debug)]
pub(crate) enum Refusal {
    Reason(String),
    Asks(Box<AskedFacts>),
}

impl From<String> for Refusal {
    fn from(reason: String) -> Self {
        Self::Reason(reason)
    }
}

impl From<&str> for Refusal {
    fn from(reason: &str) -> Self {
        Self::Reason(reason.to_owned())
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reason(reason) => f.write_str(reason),
            Self::Asks(_) => f.write_str(tilo_actions::remote::CONFIRMATION_REQUIRED_CODE),
        }
    }
}

impl Refusal {
    /// The refusal as the caller receives it: an ask keeps every fact it shows the person.
    pub(crate) fn into_call_error(self) -> tilo_actions::remote::ActionCallError {
        match self {
            Self::Reason(reason) => super::action_provider::refusal(reason),
            Self::Asks(facts) => tilo_actions::remote::ActionCallError::ConfirmationRequired(facts),
        }
    }
}

/// What the person types to allow an action whose contract asks for `phrase`.
fn phrase_take(phrase: &str) -> Take {
    Take {
        label: format!("Type “{phrase}” to confirm"),
        secret: false,
        shown: false,
        initial: None,
        chord: false,
    }
}

/// What the person reads of a call of `action` with `argument` before they answer its ask: each
/// argument its declaration shows (`ActionContract::shows`), labelled as its parameter is
/// published, in their language. Read from the call alone, with no call into the app, so the
/// ask reads the same whether or not the app is running. A secret is shown withheld.
fn shown_facts(
    action: &PublishedAction,
    argument: Option<&str>,
) -> Vec<tilo_actions::remote::asked::Fact> {
    use tilo_actions::remote::asked::{Fact, ShownAs};
    let arguments: serde_json::Map<String, serde_json::Value> = argument
        .and_then(|argument| serde_json::from_str(argument).ok())
        .unwrap_or_default();
    action
        .contract
        .shows
        .iter()
        .filter_map(|shown| {
            let value = arguments.get(&shown.param)?;
            let value = match &shown.field {
                Some(field) => value.get(field)?,
                None => value,
            };
            let label = action
                .params
                .iter()
                .find(|param| *param.name == *shown.param)
                .and_then(|param| param.schema.as_deref())
                .and_then(|schema| schema.get("title"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&shown.param)
                .to_owned();
            let value = match (shown.render, value) {
                (ShownAs::Secret, _) => "••••••".to_owned(),
                (_, serde_json::Value::String(text)) => text.clone(),
                (_, serde_json::Value::Null) => return None,
                (_, other) => other.to_string(),
            };
            Some(Fact::new(label, value))
        })
        .collect()
}

/// What the ask of a call of `action` with `argument` shows and takes: the arguments its
/// declaration shows, and what the person types where they answer it (a site's user name and
/// password), which reaches the action with their approve and never as an argument.
fn shown_on_ask(action: &PublishedAction, argument: Option<&str>, facts: &mut AskedFacts) {
    facts.facts = shown_facts(action, argument);
    facts.takes.extend(action.contract.takes.iter().cloned());
}

/// The parts of an invocation its ask is decided on.
pub(super) struct Asking<'a> {
    pub(super) request: Option<&'a tilo_actions::remote::InvokeRequest>,
    pub(super) caller: Option<&'a tilo_actions::remote::Caller>,
    pub(super) argument: Option<&'a str>,
    pub(super) actor: &'a BrokerActor,
    pub(super) token: Option<&'a str>,
    /// The invocation as the caller or its owner names it.
    pub(super) invocation: Option<tilo_actions::InvocationId>,
    /// The directory revision it was admitted at.
    pub(super) revision: Option<&'a str>,
}

impl AppBrokerState {
    /// Whether `offered` waits for the person's yes before it runs, and whether this call carries
    /// it: the one decision for an action that asks, for an ask the workspace's grants leave to
    /// the person (`asks`), and for what the store has a person approve on a trusted sheet.
    ///
    /// A call nobody approved is refused with what the person is shown, and the dispatch that
    /// carried it keeps it as its asked record; the same call comes back with their approve
    /// ([`tilo_actions::remote::InvokeRequest::approval`]), carrying the phrase they typed where
    /// the action asks for one. A one-use confirmation token minted for this call answers it too.
    /// Answers `true` when the call needed a yes and has one, `false` when it needed none.
    pub(super) fn asks_first(
        &self,
        peer_id: u64,
        offered: &(PublishedSurface, PublishedAction),
        inputs: &Asking<'_>,
        asks: bool,
    ) -> Result<bool, Refusal> {
        let (surface, action) = offered;
        let store = requires_store_approval(&surface.app_id, &action.action);
        let policy = action.contract.confirmation_for_argument(inputs.argument);
        if !asks && !store && matches!(policy, tilo_actions::ConfirmationPolicy::None) {
            return Ok(false);
        }
        let facts = || {
            let mut facts = AskedFacts::by(
                inputs
                    .caller
                    .map(crate::workspaces::asking::asked_by)
                    .unwrap_or_default(),
            );
            facts.on = Some(action.resource_label.clone()).filter(|label| !label.is_empty());
            facts.verb.clone_from(&action.action_label);
            facts.destructive = !action.contract.can_be_undone();
            if let tilo_actions::ConfirmationPolicy::Phrase { phrase } = &policy {
                facts.takes.push(phrase_take(phrase));
            }
            shown_on_ask(action, inputs.argument, &mut facts);
            facts
        };
        // A token minted for this very call answers for it where nobody's approve came back.
        if let Some(token) = inputs.token
            && inputs
                .request
                .and_then(|request| request.approval())
                .is_none()
        {
            self.confirmation_grant_answers(peer_id, offered, inputs, token)?;
            return Ok(true);
        }
        let Some(request) = inputs.request else {
            return Err(Refusal::Asks(Box::new(facts())));
        };
        match tilo_actions::remote::asked::approved_or_asks(request, facts) {
            Ok(()) => {}
            Err(tilo_actions::remote::ActionCallError::ConfirmationRequired(facts)) => {
                return Err(Refusal::Asks(facts));
            }
            Err(refused) => return Err(Refusal::Reason(refused.to_string())),
        }
        // Approved: where the action asks for a phrase, what they typed is that phrase.
        if let tilo_actions::ConfirmationPolicy::Phrase { phrase } = &policy
            && request
                .approval()
                .and_then(|approval| approval.typed.first())
                .map(|typed| typed.as_str())
                != Some(phrase.as_str())
        {
            return Err(Refusal::Reason(format!("what was typed is not “{phrase}”")));
        }
        Ok(true)
    }

    /// Whether the one-use confirmation `token` was minted for exactly this call: spent here
    /// either way.
    fn confirmation_grant_answers(
        &self,
        peer_id: u64,
        (surface, action): &(PublishedSurface, PublishedAction),
        inputs: &Asking<'_>,
        token: &str,
    ) -> Result<(), Refusal> {
        let grant = self
            .confirmation_grants
            .lock()
            .map_err(|_| "the confirmation registry is unavailable")?
            .remove(token)
            .ok_or("the confirmation token is unknown or already used")?;
        let mismatched = (requires_store_approval(&surface.app_id, &action.action)
            && grant.host_approved.is_none())
            || grant.expires < tilo_clock::now()
            || grant.peer_id != peer_id
            || grant.resource != action.resource
            || grant.action != action.action
            || grant.argument_digest != argument_digest(inputs.argument)
            || grant.actor != *inputs.actor.member()
            || grant.host_approved.as_ref().is_some_and(|binding| {
                binding.workspace != surface.route.cluster_id
                    || Some(binding.invocation) != inputs.invocation
                    || binding.publication != self.approval_publication(&binding.provider)
                    || match inputs.request {
                        Some(request) => {
                            binding.provider != request.target.provider
                                || binding.revision != request.revision
                        }
                        None => {
                            binding.provider != peer_id.to_string()
                                || binding
                                    .revision
                                    .as_deref()
                                    .is_some_and(|bound| Some(bound) != inputs.revision)
                        }
                    }
            });
        if mismatched {
            return Err(
                "the confirmation token is expired or does not match this invocation".into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::asked::{Shown, ShownAs};

    /// The Browser's `sign_in` on a page, as its declaration publishes it.
    fn sign_in() -> PublishedAction {
        PublishedAction {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: "tl:so.tilo.browser:page-1".into(),
            action: "sign_in".into(),
            resource_kind: "Page".into(),
            resource_label: "Page".into(),
            action_label: "Sign in to the site".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                shows: ["site", "realm"]
                    .map(|param| Shown {
                        param: param.into(),
                        field: None,
                        render: ShownAs::Text,
                    })
                    .into(),
                takes: vec![
                    Take {
                        label: "User name".into(),
                        ..Take::default()
                    },
                    Take {
                        label: "Password".into(),
                        secret: true,
                        ..Take::default()
                    },
                ],
                ..Default::default()
            },
            input: PublishedInput::None,
            view: None,
        }
    }

    /// A site's sign-in asks the person with the site and its realm read from the call, and has
    /// them type a user name and a password; the call itself carries neither.
    #[test]
    fn a_sign_in_shows_the_site_and_takes_the_password_as_a_secret() {
        let argument = r#"{"id":4,"asker":"a1","site":"https://intranet.example","realm":"Staff","proxy":"false"}"#;
        let mut facts = AskedFacts::default();
        shown_on_ask(&sign_in(), Some(argument), &mut facts);
        let shown: Vec<(&str, &str)> = facts
            .facts
            .iter()
            .map(|fact| (fact.label.as_str(), fact.value.as_str()))
            .collect();
        assert_eq!(
            shown,
            [("site", "https://intranet.example"), ("realm", "Staff")],
            "what the declaration shows, in its order, and nothing else of the call"
        );
        let takes: Vec<(&str, bool)> = facts
            .takes
            .iter()
            .map(|take| (take.label.as_str(), take.secret))
            .collect();
        assert_eq!(takes, [("User name", false), ("Password", true)]);
        assert!(
            facts.answered_where_asked(),
            "a password is typed only on the device that asked"
        );
    }
}
