//! The models a person's connected providers offer, as a conversation with an
//! agent offers them: one list, read from the server that keeps the providers,
//! and the name the model an agent answers on goes by.
//!
//! A page holds no provider and no key: it asks the server for the providers it
//! keeps, their models and which one is used where, through the same
//! `tilo.models` action Settings reads. A window that is no live window (a
//! capture) asks nothing, and says so by answering nothing.

use std::cell::RefCell;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::InvokeArgument;
use tilo_actions::remote::person_models::{self, ModelProviders, ProviderView};
use ui::{Arrival, InvocationCall, RemoteActions};

/// The same Settings section opens from the model picker and an empty agent conversation.
pub(crate) fn settings_reference() -> ui::tilo_reference::ResourceRef {
    ui::tilo_reference::ResourceRef::whole(
        "settings.section",
        "settings.section:agent_models",
        crate::i18n::t("chat-agent-model-settings"),
    )
}

/// One model a person may choose for an agent: the value an agent's model
/// names (provider/model), the words a person reads for the model, and the
/// provider that offers it as its own fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) provider: String,
}

/// The person's model providers, as the server that keeps them answered.
#[derive(Default)]
pub(crate) struct ModelChoices {
    /// Whether the window may ask a server: only a live window does.
    live: bool,
    /// The one listing asked of the server, followed until it answers.
    listing: RefCell<Option<InvocationCall>>,
    /// The providers as the server answered them; `None` until it answered.
    kept: RefCell<Option<ModelProviders>>,
}

impl ModelChoices {
    /// The choices of a window that `live` says is a live window: the one a
    /// person uses asks the server, one drawn for a capture asks nothing.
    pub(crate) fn new(live: bool) -> Self {
        Self {
            live,
            listing: RefCell::default(),
            kept: RefCell::default(),
        }
    }

    /// The choices of a window holding `answered` already: a test's stand-in
    /// for the server's listing.
    #[cfg(test)]
    pub(crate) fn answered(answered: ModelProviders) -> Self {
        let choices = Self::new(false);
        *choices.kept.borrow_mut() = Some(answered);
        choices
    }

    /// Take the server's answer, if it answered since the last pass: the
    /// providers it keeps and their models.
    pub(crate) fn poll(&self) {
        if !self.live {
            return;
        }
        let mut listing = self.listing.borrow_mut();
        let call = listing.get_or_insert_with(|| {
            RemoteActions::current().invoke(
                person_models::target(person_models::LIST),
                InvokeArgument::None,
                None,
            )
        });
        let answered = match call.get() {
            Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => output
                .value
                .and_then(|value| serde_json::from_value::<ModelProviders>(value).ok()),
            Arrival::Ready(_) | Arrival::Pending { .. } => return,
            Arrival::Failed(_) => None,
        };
        if let Some(answered) = answered {
            let mut kept = self.kept.borrow_mut();
            if kept.as_ref() != Some(&answered) {
                *kept = Some(answered);
            }
        }
    }

    /// Every model of every provider whose key or sign-in the server keeps, as
    /// the choices a conversation offers; `None` until the server answered.
    pub(crate) fn choices(&self) -> Option<Vec<Choice>> {
        let kept = self.kept.borrow();
        let kept = kept.as_ref()?;
        Some(
            kept.providers
                .iter()
                .filter(|provider| provider.configured)
                .flat_map(provider_choices)
                .collect(),
        )
    }

    /// Whether the host supplied a configured model that can be chosen.
    pub(crate) fn has_choices(&self) -> bool {
        self.kept.borrow().as_ref().is_some_and(|kept| {
            kept.providers
                .iter()
                .any(|provider| provider.configured && !provider.models.is_empty())
        })
    }

    /// The choice `named` names, as provider/model or by the model's own name;
    /// the one an agent's model names.
    pub(crate) fn find(&self, named: &str) -> Option<Choice> {
        let choices = self.choices()?;
        if let Some(found) = choices.iter().find(|choice| choice.value == named) {
            return Some(found.clone());
        }
        let named = named.trim();
        choices.into_iter().find(|choice| {
            let model = choice
                .value
                .split_once('/')
                .map_or(choice.value.as_str(), |(_, model)| model);
            model.eq_ignore_ascii_case(named) || choice.label.eq_ignore_ascii_case(named)
        })
    }

    /// The model an agent with no model of its own answers on: the server's
    /// default, where one is chosen and kept.
    pub(crate) fn default_model(&self) -> Option<Choice> {
        let kept = self.kept.borrow();
        let kept = kept.as_ref()?;
        let provider = kept
            .providers
            .iter()
            .find(|provider| Some(&provider.id) == kept.default_provider.as_ref())?;
        provider_choice(provider, provider.default_model.as_deref()?)
    }

    /// What a person reads for the model `named`, or of the default with
    /// none; the name as given where the server's list does not hold it (a
    /// model chosen on a device this server does not share).
    pub(crate) fn label(&self, named: Option<&str>) -> Option<String> {
        match named {
            Some(named) => Some(
                self.find(named)
                    .map_or_else(|| named.to_owned(), |choice| choice.label),
            ),
            None => self.default_model().map(|choice| choice.label),
        }
    }
}

/// The choices of one provider: each model it offers.
fn provider_choices(provider: &ProviderView) -> Vec<Choice> {
    provider
        .models
        .iter()
        .map(|model| Choice {
            value: format!("{}/{}", provider.id, model.id),
            label: model_label(&model.label, &model.id),
            provider: provider.name.clone(),
        })
        .collect()
}

/// One named model of `provider`, where it offers it.
fn provider_choice(provider: &ProviderView, model: &str) -> Option<Choice> {
    provider
        .models
        .iter()
        .find(|offered| offered.id == model)
        .map(|offered| Choice {
            value: format!("{}/{}", provider.id, offered.id),
            label: model_label(&offered.label, &offered.id),
            provider: provider.name.clone(),
        })
}

/// A model as a person reads it: its provider's label, or its id where it
/// carries none.
fn model_label(label: &str, id: &str) -> String {
    if label.trim().is_empty() {
        id.to_owned()
    } else {
        label.to_owned()
    }
}

#[cfg(test)]
#[path = "model_choice_tests.rs"]
mod tests;
