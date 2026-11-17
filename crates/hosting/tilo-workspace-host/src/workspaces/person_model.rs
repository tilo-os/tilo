//! A person's own model answers on the host.
//!
//! Whatever this host runs for a person that needs a model asks here and
//! nowhere else: an agent's turn, a part of a task handed on, a judging
//! filter, a summary. It asks for a person and a kind of work
//! ([`Interaction`]). The host reads that person's own choice (the model they
//! chose for the kind, else their default), takes the provider's key from
//! their vault, asks the provider, and hands the answer back as it streams,
//! with what it cost ([`ModelResponse::cost_micros`]).
//!
//! The key is read in here and goes to the provider's request alone
//! ([`ProviderCall::key`]). No answer, refusal, log line or `Debug` output
//! carries it, and nothing on the action plane reads it: the store's own
//! actions refuse every agent and answer only whether a key is kept.
//!
//! There is no model of the host's own behind this. A person who linked none
//! is refused with a reason they can act on ([`ModelRefusal`]), never
//! answered by a model somebody else pays for.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_provider_config::EndpointLocation;
use agent_provider_config::protocol::{
    ErrorKind, ModelError, ModelRequest, ModelResponse, StreamEvent,
};
pub use agent_provider_config::{
    Interaction, ModelDescriptor, ProviderProfile, ProviderProtocol, protocol,
};
use tilo_clock::time::Instant;
use tilo_history::FluentArgs;
use tilo_lock::RwLockRecover as _;

use super::Workspaces;
use super::person_settings::{credentials, key_id, provider_config};

#[cfg(any(test, feature = "test-support"))]
pub mod scripted;
#[cfg(test)]
mod tests;

/// Where a call's stream goes as it arrives: text and reasoning to show, and
/// a tool call's arguments as they grow. Presentation only: the answer the
/// call returns is what counts, and only its tool calls may run.
pub type Events<'a> = &'a mut (dyn FnMut(StreamEvent) + Send);

/// A provider's answer on its way.
pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelResponse, ModelError>> + Send + 'a>>;

/// Why a person's model did not answer.
///
/// Each reason is typed, so whoever asked can show it where the person will
/// see it ([`Self::words`]) and decide what to do next ([`Self::code`]).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelRefusal {
    /// The person chose no model for this kind of work and has no default.
    NoModelLinked {
        /// The kind of work asked for.
        kind: Interaction,
    },
    /// The model the person chose for this kind of work cannot be used: its
    /// provider is switched off, or no longer offers it. Their default is
    /// not asked in its place, since they chose this one.
    ChoiceUnusable {
        /// The kind of work asked for.
        kind: Interaction,
        /// The provider, as the person named it.
        provider: String,
        /// The model they chose.
        model: String,
    },
    /// The provider has no key in the person's vault.
    NoKey {
        /// The provider, as the person named it.
        provider: String,
    },
    /// The provider's address is not one this host calls: only a provider on
    /// the public internet, over TLS, unless the host's operator lets its
    /// people name others ([`Workspaces::call_private_model_addresses`]).
    AddressRefused {
        /// The provider, as the person named it.
        provider: String,
    },
    /// The person's settings or their vault could not be read.
    SettingsUnreadable,
    /// The provider was asked and gave no answer: its own failure, the
    /// deadline, or the call cancelled.
    Provider(ModelError),
}

impl ModelRefusal {
    /// The reason's stable name, for whoever decides by it.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoModelLinked { .. } => "no-model-linked",
            Self::ChoiceUnusable { .. } => "choice-unusable",
            Self::NoKey { .. } => "no-key",
            Self::AddressRefused { .. } => "address-refused",
            Self::SettingsUnreadable => "settings-unreadable",
            Self::Provider(_) => "provider",
        }
    }

    /// The reason in words the person can act on, in `language` (a BCP 47
    /// tag; the host's source language for one it has no catalogue of).
    #[must_use]
    pub fn words(&self, language: Option<&str>) -> String {
        let mut args = FluentArgs::new();
        let key = match self {
            Self::NoModelLinked { .. } => "model-none-linked",
            Self::ChoiceUnusable {
                provider, model, ..
            } => {
                args.set("model", model.as_str());
                // A provider the person removed since has no name to say.
                if provider.is_empty() {
                    "model-choice-gone"
                } else {
                    args.set("provider", provider.as_str());
                    "model-choice-unusable"
                }
            }
            Self::NoKey { provider } => {
                args.set("provider", provider.as_str());
                "model-no-key"
            }
            Self::AddressRefused { provider } => {
                args.set("provider", provider.as_str());
                "model-address-refused"
            }
            Self::SettingsUnreadable => "model-settings-unreadable",
            // A provider's failure is already a sentence safe to show: it
            // never carries a credential or the provider's own text.
            Self::Provider(error) => return error.message.clone(),
        };
        crate::i18n::message(language, key, &args)
    }

    /// The reason as a model failure, for a caller that reports those: the
    /// same words, under the kind that says what the person does next.
    #[must_use]
    pub fn model_error(&self) -> ModelError {
        let kind = match self {
            Self::NoModelLinked { .. } | Self::ChoiceUnusable { .. } => ErrorKind::NoProvider,
            Self::NoKey { .. } => ErrorKind::Authentication,
            Self::AddressRefused { .. } => ErrorKind::PermissionDenied,
            Self::SettingsUnreadable => ErrorKind::Unavailable,
            Self::Provider(error) => return error.clone(),
        };
        ModelError {
            kind,
            message: self.words(None),
        }
    }
}

impl std::fmt::Display for ModelRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.words(None))
    }
}

impl std::error::Error for ModelRefusal {}

/// What answers one kind of a person's work now: the provider as they named
/// it, and the model. Never its key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    /// The provider, as the person named it.
    pub provider: String,
    /// The provider's id in the person's settings.
    pub provider_id: String,
    /// How the provider is spoken to.
    pub protocol: ProviderProtocol,
    /// The model, with what its provider says of it (its context size,
    /// whether it reads pictures and calls tools).
    pub model: ModelDescriptor,
}

/// A person's answer, and what answered it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Answer {
    /// The model's whole answer. Its [`ModelResponse::cost_micros`] is what
    /// the call cost, in millionths of the currency.
    pub response: ModelResponse,
    /// What answered.
    pub linked: Linked,
}

/// One call on its way to a provider: the person's own provider and model,
/// and the request.
pub struct ProviderCall<'a> {
    /// The provider, as the person's settings keep it.
    pub profile: &'a ProviderProfile,
    /// The model asked.
    pub model: &'a ModelDescriptor,
    /// What is asked.
    pub request: &'a ModelRequest,
    /// When the answer is due.
    pub deadline: Instant,
    key: Option<&'a str>,
}

impl ProviderCall<'_> {
    /// The provider's key from the person's vault, for the request's own
    /// header and nothing else.
    #[must_use]
    pub const fn key(&self) -> Option<&str> {
        self.key
    }
}

impl std::fmt::Debug for ProviderCall<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderCall")
            .field("provider", &self.profile.id)
            .field("model", &self.model.id)
            .field("keyed", &self.key.is_some())
            .finish_non_exhaustive()
    }
}

/// What speaks to providers for this host's people.
pub trait ModelProvider: Send + Sync {
    /// Ask `call`'s provider, handing what streams to `events` as it
    /// arrives. Dropping the answer on its way stops the call.
    fn answer<'a>(&'a self, call: ProviderCall<'a>, events: Events<'a>) -> ProviderFuture<'a>;
}

/// A host that speaks to no provider.
struct NoProvider;

impl ModelProvider for NoProvider {
    fn answer<'a>(&'a self, _: ProviderCall<'a>, _: Events<'a>) -> ProviderFuture<'a> {
        Box::pin(async { Err(ModelError::new(ErrorKind::Unavailable)) })
    }
}

/// What answers people's model calls on this host, and which addresses it
/// calls for them.
pub(super) struct Models {
    provider: std::sync::RwLock<Arc<dyn ModelProvider>>,
    private_addresses: AtomicBool,
    /// The runtime a caller that cannot wait asynchronously is answered on
    /// ([`PersonModel::answer_blocking`]).
    runtime: Option<tokio::runtime::Handle>,
}

impl Models {
    /// Whether this host calls providers on its own network
    /// ([`Workspaces::call_private_model_addresses`]).
    pub(super) fn private_addresses(&self) -> bool {
        self.private_addresses.load(Ordering::Acquire)
    }

    /// Models answered on the runtime this is called on.
    pub(super) fn new() -> Self {
        Self {
            provider: std::sync::RwLock::new(Arc::new(NoProvider)),
            private_addresses: AtomicBool::new(false),
            runtime: tokio::runtime::Handle::try_current().ok(),
        }
    }
}

impl Workspaces {
    /// The model `person` chose for `kind` of work, to ask on their behalf:
    /// the one entry everything this host runs for a person asks a model
    /// through.
    ///
    /// The handle reads the person's choice and key anew for every call, so
    /// it may be kept: a model they change in Settings answers the next
    /// call.
    #[must_use]
    pub fn person_model(&self, person: [u8; 32], kind: Interaction) -> PersonModel {
        PersonModel {
            models: Arc::clone(&self.inner.models),
            root: self.person_settings_root(&person),
            kind,
        }
    }

    /// Let this host's people name providers at addresses off the public
    /// internet (a model server on the operator's own network): the choice
    /// of whoever runs the host, since a call to such an address is made
    /// from inside the host's network. Off unless set.
    pub fn call_private_model_addresses(&self, allowed: bool) {
        self.inner
            .models
            .private_addresses
            .store(allowed, Ordering::Release);
    }

    /// Answer people's model calls through `provider` in place of the
    /// host's own, so a test scripts what a provider says.
    #[cfg(any(test, feature = "test-support"))]
    pub fn answer_models_with(&self, provider: Arc<dyn ModelProvider>) {
        *self.inner.models.provider.write_recover() = provider;
    }
}

/// One kind of one person's work, answered by the model they chose for it.
#[derive(Clone)]
pub struct PersonModel {
    models: Arc<Models>,
    /// The person's own store on this host.
    root: PathBuf,
    kind: Interaction,
}

impl std::fmt::Debug for PersonModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonModel")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl PersonModel {
    /// The kind of work this answers.
    #[must_use]
    pub const fn kind(&self) -> Interaction {
        self.kind
    }

    /// What would answer now, or why nothing can. It reads the person's
    /// settings and vault from disk, so an asynchronous caller asks it off
    /// the runtime's workers.
    pub fn linked(&self) -> Result<Linked, ModelRefusal> {
        self.resolve().map(|resolved| resolved.linked())
    }

    /// Ask the person's model `request`, handing what streams to `events` as
    /// it arrives, until `deadline`. Dropping the answer on its way stops
    /// the call.
    pub async fn answer(
        &self,
        request: &ModelRequest,
        deadline: Instant,
        events: Events<'_>,
    ) -> Result<Answer, ModelRefusal> {
        // The settings and the vault are files: read off the runtime's
        // workers, which serve every other member meanwhile.
        let reading = self.clone();
        let resolved = tokio::task::spawn_blocking(move || reading.resolve())
            .await
            .map_err(|error| {
                tracing::warn!(%error, "a person's model settings were not read");
                ModelRefusal::SettingsUnreadable
            })??;
        let provider = Arc::clone(&*self.models.provider.read_recover());
        let call = ProviderCall {
            profile: &resolved.profile,
            model: &resolved.model,
            request,
            deadline,
            key: resolved.key.as_ref().map(|secret| secret.password.as_str()),
        };
        let response = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            provider.answer(call, events),
        )
        .await
        .map_err(|_| ModelRefusal::Provider(ModelError::new(ErrorKind::TimedOut)))?
        .map_err(ModelRefusal::Provider)?;
        Ok(Answer {
            response,
            linked: resolved.linked(),
        })
    }

    /// [`Self::answer`] for a caller on a thread of its own, which waits
    /// here until the answer, `deadline` or `cancel`. Never called from an
    /// asynchronous task: that one awaits [`Self::answer`].
    pub fn answer_blocking(
        &self,
        request: &ModelRequest,
        deadline: Instant,
        cancel: &Cancel,
        events: Events<'_>,
    ) -> Result<Answer, ModelRefusal> {
        let Some(runtime) = self.models.runtime.as_ref() else {
            return Err(ModelRefusal::Provider(ModelError::new(
                ErrorKind::Unavailable,
            )));
        };
        runtime.block_on(async {
            tokio::select! {
                answer = self.answer(request, deadline, events) => answer,
                () = cancel.cancelled() => {
                    Err(ModelRefusal::Provider(ModelError::new(ErrorKind::Cancelled)))
                }
            }
        })
    }

    fn resolve(&self) -> Result<Resolved, ModelRefusal> {
        resolve(
            &self.root,
            self.kind,
            self.models.private_addresses.load(Ordering::Acquire),
        )
    }
}

/// Stops a call a thread waits for ([`PersonModel::answer_blocking`]): the
/// person cancelled, or a newer message replaced the turn.
#[derive(Clone, Default)]
pub struct Cancel {
    inner: Arc<Cancelling>,
}

#[derive(Default)]
struct Cancelling {
    cancelled: AtomicBool,
    wake: tokio::sync::Notify,
}

impl Cancel {
    /// Stop the call, now or when it starts.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.wake.notify_waiters();
    }

    /// Whether the call was stopped.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Returns once the call is stopped.
    pub async fn cancelled(&self) {
        loop {
            // Listening before the flag is read: a stop between the two
            // still wakes this.
            let woken = self.inner.wake.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            woken.await;
        }
    }
}

/// A person's choice for one kind of work, read from their store: the
/// provider, the model and the key.
struct Resolved {
    profile: ProviderProfile,
    model: ModelDescriptor,
    key: Option<accounts::Secret>,
}

impl Resolved {
    fn linked(&self) -> Linked {
        Linked {
            provider: self.profile.display_label().to_owned(),
            provider_id: self.profile.id.clone(),
            protocol: self.profile.protocol,
            model: self.model.clone(),
        }
    }
}

/// What the person whose store is `root` chose for `kind`, with its key:
/// the model chosen for the kind, else their default provider's default
/// model.
fn resolve(
    root: &Path,
    kind: Interaction,
    private_addresses: bool,
) -> Result<Resolved, ModelRefusal> {
    // A person who never set anything up keeps no store, and asking about
    // them makes none.
    if !root.is_dir() {
        return Err(ModelRefusal::NoModelLinked { kind });
    }
    let config = provider_config(root);
    let (profile, model) = match config.interaction_models.get(kind.id()) {
        Some(chosen) => {
            config
                .interaction_selection(kind)
                .ok_or_else(|| ModelRefusal::ChoiceUnusable {
                    kind,
                    provider: config
                        .profiles
                        .iter()
                        .find(|profile| profile.id == chosen.profile)
                        .map(|profile| profile.display_label().to_owned())
                        .unwrap_or_default(),
                    model: chosen.model.clone(),
                })?
        }
        None => config
            .active_profile()
            .and_then(|profile| {
                let default = profile.default_model.as_ref()?;
                let model = profile.models.iter().find(|model| &model.id == default)?;
                Some((profile, model))
            })
            .ok_or(ModelRefusal::NoModelLinked { kind })?,
    };
    let provider = || profile.display_label().to_owned();
    let public = EndpointLocation::of_endpoint(&profile.endpoint) == Some(EndpointLocation::Hosted)
        && profile.endpoint.trim_start().starts_with("https://");
    if !public && !private_addresses {
        return Err(ModelRefusal::AddressRefused {
            provider: provider(),
        });
    }
    // A sign-in in place of a key is kept on the person's own computer, so
    // this host holds nothing to ask such a provider with.
    if profile.sign_in.is_some() {
        return Err(ModelRefusal::NoKey {
            provider: provider(),
        });
    }
    let vault = credentials(root).map_err(|error| {
        tracing::warn!(%error, "a person's vault was not opened for their model");
        ModelRefusal::SettingsUnreadable
    })?;
    let key = vault
        .get(&key_id(&profile.id))
        .filter(|secret| !secret.password.is_empty());
    // Only a server on the operator's own network may take no key.
    if key.is_none() && public {
        return Err(ModelRefusal::NoKey {
            provider: provider(),
        });
    }
    Ok(Resolved {
        profile: profile.clone(),
        model: model.clone(),
        key,
    })
}
