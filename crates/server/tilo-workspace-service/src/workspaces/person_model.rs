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
//! carries it, and nothing on the action service reads it: the store's own
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
use std::time::Duration;

use agent_provider_config::EndpointLocation;
use agent_provider_config::protocol::{
    Content, ErrorKind, Message, ModelError, ModelRequest, ModelResponse, Role, StreamEvent,
    ToolChoice,
};
pub use agent_provider_config::{
    Interaction, ModelDescriptor, ProviderProfile, ProviderProtocol, protocol,
};
use tilo_clock::time::Instant;
use tilo_history::FluentArgs;
use tilo_lock::RwLockRecover as _;
use tilo_server_protocol::WorkspaceId;

use super::Workspaces;
use super::person_settings::provider_config;

mod destinations;
pub use destinations::{AllowedDestination, DestinationPolicy};
mod kept;
#[cfg(feature = "agents")]
mod live;
#[cfg(any(test, feature = "test-support"))]
pub mod scripted;
mod spend;
pub use spend::{SpendError, Spender};
#[cfg(test)]
mod tests;
#[cfg(all(test, feature = "agents"))]
mod wire_tests;

/// How often a call a thread waits for looks whether it was stopped: at once
/// to a person, and seldom enough to cost nothing.
const STOP_LOOK: Duration = Duration::from_millis(25);

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
    /// The person chose no model for this kind of work and has no default,
    /// nor, for a kind of their main thread, a model for its other kind.
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
    /// The request carries something the chosen model does not take.
    CannotTake {
        /// The model, as the person reads its name.
        model: String,
        /// What it does not take.
        what: Input,
    },
    /// The workspace the work is in does not let its content go to this
    /// provider.
    WorkspaceRefuses {
        /// The provider, as the person named it.
        provider: String,
        /// The blocked destination origin (`scheme://domain[:port]`).
        destination: String,
        /// Allowed alternatives that could continue (the person's linked
        /// providers allowed by the workspace, or the destinations the
        /// workspace policy permits).
        allowed_alternatives: Vec<String>,
    },
    /// The agent the call is for, or the piece of its work the call belongs
    /// to, has used what its person lets it spend in a day.
    DayLimitReached {
        /// The agent, or the piece of its work, as its person named it.
        agent: String,
        /// The most it may spend in a day, in millionths of the currency.
        limit_micros: u64,
        /// What it has spent today (settled plus active in-flight commitments).
        spent_micros: u64,
    },
    /// The person's spending file could not be read, so capped calls fail
    /// closed rather than resetting today's spend to zero.
    SpendUnreadable,
    /// The person's settings or their vault could not be read.
    SettingsUnreadable,
    /// The provider was asked and gave no answer: its own failure, the
    /// deadline, or the call cancelled.
    Provider(ModelError),
    /// The provider was asked and turned the person's key away, or the
    /// key's account: the key is wrong, may not do what was asked, or its
    /// account has no credit left. Only the person mends that, so the words
    /// name the provider and say what helps.
    KeyRefused {
        /// The provider, as the person named it.
        provider: String,
        /// The provider's failure, under its own kind.
        error: ModelError,
    },
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
            Self::CannotTake { .. } => "cannot-take",
            Self::WorkspaceRefuses { .. } => "workspace-refuses",
            Self::DayLimitReached { .. } => "day-limit",
            Self::SpendUnreadable => "spend-unreadable",
            Self::SettingsUnreadable => "settings-unreadable",
            Self::Provider(_) | Self::KeyRefused { .. } => "provider",
        }
    }

    /// A provider's failure as the refusal: under the provider's name where
    /// it is of the person's key or of the key's account.
    fn of_provider(provider: &str, error: ModelError) -> Self {
        match error.kind {
            ErrorKind::Authentication
            | ErrorKind::PermissionDenied
            | ErrorKind::CreditsExhausted => Self::KeyRefused {
                provider: provider.to_owned(),
                error,
            },
            _ => Self::Provider(error),
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
            Self::CannotTake {
                model,
                what: Input::Picture,
            } => {
                args.set("model", model.as_str());
                "model-cannot-take-picture"
            }
            Self::WorkspaceRefuses {
                provider,
                destination,
                allowed_alternatives,
            } => {
                args.set("provider", provider.as_str());
                let base = crate::i18n::message(language, "model-workspace-refuses", &args);
                let mut parts = vec![base];
                if !destination.is_empty() {
                    parts.push(format!("Blocked destination: {destination}."));
                }
                if !allowed_alternatives.is_empty() {
                    parts.push(format!(
                        "Allowed alternatives: {}.",
                        allowed_alternatives.join(", ")
                    ));
                }
                return parts.join(" ");
            }
            Self::DayLimitReached {
                agent,
                limit_micros,
                ..
            } => {
                args.set("agent", agent.as_str());
                args.set("limit", money(*limit_micros));
                "model-day-limit"
            }
            Self::SpendUnreadable => {
                return "Your spending history could not be read, so capped model calls are paused \
                        rather than resetting today's limit."
                    .to_owned();
            }
            Self::SettingsUnreadable => "model-settings-unreadable",
            Self::KeyRefused { provider, error } => {
                args.set("provider", provider.as_str());
                match error.kind {
                    ErrorKind::PermissionDenied => "model-key-not-allowed",
                    ErrorKind::CreditsExhausted => "model-key-no-credit",
                    _ => "model-key-refused",
                }
            }
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
            Self::AddressRefused { .. }
            | Self::WorkspaceRefuses { .. }
            | Self::DayLimitReached { .. } => ErrorKind::PermissionDenied,
            Self::CannotTake { .. } => ErrorKind::InvalidRequest,
            Self::SpendUnreadable | Self::SettingsUnreadable => ErrorKind::Unavailable,
            // The provider's own kind, with the words that name it.
            Self::KeyRefused { error, .. } => error.kind,
            Self::Provider(error) => return error.clone(),
        };
        ModelError {
            kind,
            message: self.words(None),
        }
    }
}

/// `micros` millionths of the currency, as a person reads an amount: `$5`,
/// `$0.42`.
fn money(micros: u64) -> String {
    let cents = micros / 10_000;
    let (units, hundredths) = (cents / 100, cents % 100);
    if hundredths == 0 {
        format!("${units}")
    } else {
        format!("${units}.{hundredths:02}")
    }
}

impl std::fmt::Display for ModelRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.words(None))
    }
}

impl std::error::Error for ModelRefusal {}

/// What a request can carry that not every model takes. A document's pages
/// travel as pictures of them; a recording has no place in a request yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Input {
    /// A picture: a photo, a page of a document, what a window shows.
    Picture,
}

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

impl From<spend::ReserveRefusal> for ModelRefusal {
    fn from(refusal: spend::ReserveRefusal) -> Self {
        match refusal {
            spend::ReserveRefusal::DayLimitReached {
                agent,
                limit_micros,
                spent_micros,
            } => Self::DayLimitReached {
                agent,
                limit_micros,
                spent_micros,
            },
            spend::ReserveRefusal::Unreadable => Self::SpendUnreadable,
        }
    }
}

/// Whether an answer's monetary cost is known from a price table/provider or
/// unknown because no price exists for the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerCost {
    /// Priced in millionths of the currency.
    Known(u64),
    /// The model has no known price and reported no cost; its actual cost is
    /// unknown rather than zero.
    Unknown,
}

impl AnswerCost {
    /// The known cost in millionths of the currency, or `None` when unpriced.
    #[must_use]
    pub const fn micros(self) -> Option<u64> {
        match self {
            Self::Known(micros) => Some(micros),
            Self::Unknown => None,
        }
    }

    /// Whether this answer's cost is unknown.
    #[must_use]
    pub const fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }
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
    /// What the agent the call was for has spent today, this answer
    /// included, in millionths of the currency; none for a call that
    /// spends no agent's day ([`PersonModel::spending_as`]). Of a call that
    /// spends more than one day, the first.
    pub spent_today_micros: Option<u64>,
    /// Whether the answer's cost was known or unpriced, distinguishing
    /// unknown model pricing from zero cost.
    pub cost: AnswerCost,
}

impl Answer {
    /// The answer's words, without its reasoning and its tool calls.
    #[must_use]
    pub fn text(&self) -> String {
        self.response
            .message
            .content
            .iter()
            .filter_map(|part| match part {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// How long a provider may show nothing before a call to it is let go, so
/// that whoever asked may ask anew: no response headers after the request,
/// and after them no byte of any kind (a piece of the answer, a keep-alive,
/// a ping). A provider that keeps showing something is held to the call's
/// deadline alone, however long it works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quiet {
    /// On a model's calls while none of them has timed out.
    pub first: Duration,
    /// On its calls after one that timed out: longer, so that a provider
    /// that shows nothing while it works is let go of once and not again.
    pub after_silence: Duration,
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
    /// Whether the host's operator lets the provider be at an address off
    /// the public internet.
    pub private_addresses: bool,
    /// How long the provider may show nothing before the call is let go and
    /// ends as timed out ([`Quiet`]); none where the deadline alone bounds
    /// it.
    pub quiet: Option<Duration>,
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

    /// Whether `endpoint` is a provider on the public internet, reached over
    /// TLS: what a person's provider is, unless the host's operator lets
    /// its people name others.
    fn reaches(&self, endpoint: &str) -> bool {
        EndpointLocation::of_endpoint(endpoint) == Some(EndpointLocation::Hosted)
            && endpoint.trim_start().starts_with("https://")
    }
}

/// A host that speaks to no provider.
#[cfg(any(test, not(feature = "agents")))]
struct NoProvider;

#[cfg(any(test, not(feature = "agents")))]
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
    /// What each person's agents spent today, and active in-flight budget
    /// reservations.
    spend: Arc<spend::Log>,
    /// Per-workspace policies controlling where workspace content may be sent.
    destinations: destinations::Store,
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

    /// Where this server connects for its people's mail: the public
    /// internet, and its own network too while its operator lets it call the
    /// addresses people name. The setting is read anew at every connection,
    /// and the one judge of addresses answers for each.
    pub(super) fn mail_reach(self: &Arc<Self>) -> tilo_account_probe::imap::Reach {
        let models = Arc::clone(self);
        tilo_account_probe::imap::Reach::public_unless(move || models.private_addresses())
    }

    /// The sessions this server holds with its people's mail servers: every
    /// connection of theirs is made within [`Self::mail_reach`].
    pub(super) fn mail_session(self: &Arc<Self>) -> tilo_mail_session::LiveMailSession {
        tilo_mail_session::LiveMailSession::default().reaching(self.mail_reach())
    }

    /// Models answered on the runtime this is called on.
    pub(super) fn new() -> Self {
        Self {
            provider: std::sync::RwLock::new(host_provider()),
            private_addresses: AtomicBool::new(false),
            spend: Arc::default(),
            destinations: destinations::Store::default(),
            runtime: tokio::runtime::Handle::try_current().ok(),
        }
    }
}

/// What a host asks providers through unless a test scripts them: the model
/// gateway's transport where the host runs agents, nothing elsewhere.
#[cfg(feature = "agents")]
fn host_provider() -> Arc<dyn ModelProvider> {
    Arc::new(live::GatewayProvider::default())
}

#[cfg(not(feature = "agents"))]
fn host_provider() -> Arc<dyn ModelProvider> {
    Arc::new(NoProvider)
}

impl Workspaces {
    /// The model `person` chose for `kind` of work, to ask on their behalf:
    /// the one entry everything this host runs for a person asks a model
    /// through.
    ///
    /// The handle reads the person's choice anew for every call, and their
    /// key again whenever their vault was written since, so it may be kept:
    /// a model they change in Settings answers the next call.
    #[must_use]
    pub fn person_model(&self, person: [u8; 32], kind: Interaction) -> PersonModel {
        PersonModel {
            models: Arc::clone(&self.inner.models),
            root: self.person_settings_root(&person),
            workspace_dir: self.inner.dir.clone(),
            kept: Arc::default(),
            kind,
            named: None,
            workspace: None,
            spenders: Vec::new(),
            reserve_micros: None,
            quiet: None,
        }
    }

    /// Set where `workspace` allows its content to be sent when a model is
    /// asked on behalf of a member working in it.
    ///
    /// Persisted atomically under the service directory and enforced on every
    /// model call working in `workspace`. Setting [`DestinationPolicy::Any`]
    /// removes the restriction.
    pub fn set_workspace_destinations(
        &self,
        workspace: WorkspaceId,
        policy: DestinationPolicy,
    ) -> std::io::Result<()> {
        self.inner
            .models
            .destinations
            .set(&self.inner.dir, workspace, policy)
    }

    /// Read the active data-destination policy for `workspace`.
    ///
    /// Returns [`DestinationPolicy::Any`] when no policy has been set; returns
    /// an error if an existing policy file cannot be read or parsed.
    pub fn workspace_destinations(
        &self,
        workspace: &WorkspaceId,
    ) -> std::io::Result<DestinationPolicy> {
        self.inner
            .models
            .destinations
            .get(&self.inner.dir, workspace)
    }

    /// Whether `person`'s settings on this server hold what `named` names as
    /// provider/model ([`PersonModel::naming`]): what a skill may name as the
    /// model its work is done with. It reads the person's settings from disk,
    /// so an asynchronous caller asks it off the runtime's workers.
    ///
    /// # Errors
    /// Nothing the person linked here is named so. The words say what they
    /// linked, each provider by its name with the models it offers, and never
    /// a key.
    pub fn person_links_model(&self, person: &[u8; 32], named: &str) -> Result<(), String> {
        let root = self.person_settings_root(person);
        let linked: Vec<String> = if root.is_dir() {
            let config = provider_config(&root);
            if named_selection(&config, named).is_some() {
                return Ok(());
            }
            config
                .profiles
                .iter()
                .filter(|profile| profile.enabled)
                .map(|profile| {
                    let offered: Vec<&str> = profile
                        .models
                        .iter()
                        .map(|model| model.id.as_str())
                        .collect();
                    format!("{} ({})", profile.display_label(), offered.join(", "))
                })
                .collect()
        } else {
            Vec::new()
        };
        Err(if linked.is_empty() {
            format!(
                "{named:?} names no model: the person linked no provider on this server. \
                 Leave the model out."
            )
        } else {
            format!(
                "{named:?} names no model the person linked on this server. Name one as \
                 provider/model from: {}.",
                linked.join("; ")
            )
        })
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
    /// The service data directory holding per-workspace destination policies.
    workspace_dir: PathBuf,
    /// The key this work last took from the person's vault (`person_model/kept.rs`).
    kept: Arc<kept::Key>,
    kind: Interaction,
    /// The model its caller names in place of the person's choice for the
    /// kind, as provider/model ([`Self::naming`]).
    named: Option<String>,
    /// The workspace the work is in, when its caller said.
    workspace: Option<WorkspaceId>,
    /// Whose days the calls spend: an agent's, and that of any piece of its
    /// work that keeps a day of its own.
    spenders: Vec<Spender>,
    /// Optional explicit budget reservation per call, in millionths of the
    /// currency, overriding automatic estimation.
    reserve_micros: Option<u64>,
    /// How long a provider may show nothing on this model's calls, and
    /// whether one of them timed out already ([`Self::letting_go`]).
    quiet: Option<(Quiet, Arc<AtomicBool>)>,
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

    /// This model, asked with the model `named` names in place of the
    /// person's choice for the kind: `provider/model`, the provider by its id
    /// in the person's settings on this server or by the name they gave it,
    /// whatever the case. It answers only where those settings hold that
    /// provider, switched on, and it offers that model, so the person's own
    /// key pays either way. A name that names nothing here changes nothing
    /// and the choice for the kind answers as before: an agent's model chosen
    /// on a device of its keeper names a provider of that device's settings,
    /// which this server may not hold.
    #[must_use]
    pub fn naming(mut self, named: Option<&str>) -> Self {
        self.named = named
            .map(str::trim)
            .filter(|named| !named.is_empty())
            .map(str::to_owned);
        self
    }

    /// This model, asked for work in `workspace`: where what a request
    /// carries comes from. A workspace's rule on where its content may be
    /// sent is held against the provider here, so every caller that works
    /// in a workspace says which.
    #[must_use]
    pub fn working_in(mut self, workspace: WorkspaceId) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// This model, asked for `spender`: an agent whose keeper bounds what
    /// it may spend in a day. Every answer's cost is counted on that day,
    /// also when many calls run at once, and a call that starts once the
    /// limit is reached is refused before the provider is asked.
    ///
    /// Said more than once, a call spends every day it was given: the
    /// agent's, and that of a piece of its work its person bounded on its
    /// own (an event, a task). Each counts the answer, the first limit
    /// reached refuses the call, and [`Answer::spent_today_micros`] stays
    /// the first day's.
    #[must_use]
    pub fn spending_as(mut self, spender: Spender) -> Self {
        self.spenders.push(spender);
        self
    }

    /// Reserve `micros` millionths of the currency against each capped spender
    /// while a call is in flight, overriding automatic cost estimation.
    #[must_use]
    pub const fn reserving(mut self, micros: u64) -> Self {
        self.reserve_micros = Some(micros);
        self
    }

    /// The same model, its calls letting go of a provider that shows
    /// nothing for as long as `quiet` says. Such a call ends as timed out,
    /// for its caller to ask anew, and from then on this model's calls wait
    /// the longer of the two times.
    #[must_use]
    pub fn letting_go(mut self, quiet: Quiet) -> Self {
        self.quiet = Some((quiet, Arc::default()));
        self
    }

    /// What `spender` has spent on its day so far, in millionths of the
    /// currency, as this person's store counts it: nothing on a day not
    /// counted yet. If the spending file is unreadable, fails closed by
    /// returning at least the spender's limit (or `u64::MAX`).
    #[must_use]
    pub fn spent_by(&self, spender: &Spender) -> u64 {
        self.models.spend.spent(&self.root, spender)
    }

    /// What `spender` has spent on its day so far, or [`SpendError::Unreadable`]
    /// if the persisted spending file cannot be read or parsed.
    pub fn try_spent_by(&self, spender: &Spender) -> Result<u64, SpendError> {
        self.models.spend.try_spent(&self.root, spender)
    }

    /// Budget currently reserved by in-flight calls for `spender` on its day.
    #[must_use]
    pub fn reserved_by(&self, spender: &Spender) -> u64 {
        self.models.spend.reserved(&self.root, spender)
    }

    /// How many calls answered by unpriced models `spender` made on its day.
    pub fn unknown_calls_by(&self, spender: &Spender) -> Result<u64, SpendError> {
        self.models.spend.unknown_calls(&self.root, spender)
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
        self.within_day()?;
        // The settings and the vault are files: read off the runtime's
        // workers, which serve every other member meanwhile.
        let reading = self.clone();
        let resolved = tokio::task::spawn_blocking(move || reading.resolve())
            .await
            .map_err(|error| {
                tracing::warn!(%error, "a person's model settings were not read");
                ModelRefusal::SettingsUnreadable
            })??;
        resolved.takes(request)?;
        // Acquire an atomic in-flight budget reservation across every spender
        // before calling the provider, so concurrent calls cannot all pass the
        // pre-call check simultaneously and blow past a hard daily cap. If the
        // provider call fails, times out, or this future is dropped/cancelled,
        // the guard releases the reservation automatically.
        let reservation = self
            .models
            .spend
            .reserve(
                self.root.clone(),
                self.spenders.clone(),
                self.reserve_estimate(&resolved.model.id, request),
            )
            .map_err(ModelRefusal::from)?;
        let provider = self.provider();
        let call = ProviderCall {
            profile: &resolved.profile,
            model: &resolved.model,
            request,
            deadline,
            private_addresses: self.models.private_addresses.load(Ordering::Acquire),
            quiet: self.quiet_now(),
            key: resolved.key.as_deref().map(String::as_str),
        };
        let response = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.noting_time_outs(provider.answer(call, events)),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                reservation.release();
                return Err(ModelRefusal::of_provider(
                    resolved.profile.display_label(),
                    error,
                ));
            }
            Err(_) => {
                reservation.release();
                tracing::warn!(
                    model = %resolved.model.id,
                    "a model call timed out with uncertain provider cost; reservation released"
                );
                return Err(ModelRefusal::Provider(ModelError::new(ErrorKind::TimedOut)));
            }
        };
        let cost = answer_cost(&resolved.model.id, &response);
        if cost.is_unknown() {
            tracing::warn!(
                model = %resolved.model.id,
                input_tokens = response.usage.input_tokens,
                output_tokens = response.usage.output_tokens,
                "model pricing is unknown; call cost is not zero"
            );
        }
        // Reconcile the reservation against actual cost across every day the
        // call spends; the answer reports the first.
        let spent_today_micros = reservation.reconcile(response.cost_micros, cost.is_unknown());
        Ok(Answer {
            response,
            linked: resolved.linked(),
            spent_today_micros,
            cost,
        })
    }

    /// Estimate the pre-call reservation for `model_id` and `request`.
    fn reserve_estimate(&self, model_id: &str, request: &ModelRequest) -> spend::ReserveEstimate {
        if let Some(explicit) = self.reserve_micros {
            return spend::ReserveEstimate::Explicit(explicit);
        }
        spend::ReserveEstimate::Estimated(estimate_call_cost_micros(model_id, request))
    }

    /// How long the provider may show nothing on a call made now.
    fn quiet_now(&self) -> Option<Duration> {
        self.quiet.as_ref().map(|(quiet, timed_out)| {
            if timed_out.load(Ordering::Acquire) {
                quiet.after_silence
            } else {
                quiet.first
            }
        })
    }

    /// `answer` as it comes, noting a call that timed out: the model's next
    /// calls wait the longer time ([`Quiet::after_silence`]).
    async fn noting_time_outs(
        &self,
        answer: ProviderFuture<'_>,
    ) -> Result<ModelResponse, ModelError> {
        let answer = answer.await;
        if let (Err(error), Some((_, timed_out))) = (&answer, &self.quiet)
            && error.kind == ErrorKind::TimedOut
        {
            timed_out.store(true, Ordering::Release);
        }
        answer
    }

    /// Refuses a call for an agent, or a piece of its work, that has used
    /// its day (including active in-flight reservations), or whose spending
    /// file is unreadable under a hard cap.
    fn within_day(&self) -> Result<(), ModelRefusal> {
        self.models
            .spend
            .check_available(&self.root, &self.spenders)
            .map_err(ModelRefusal::from)
    }

    /// [`Self::answer`] for a caller on a thread of its own, which waits
    /// here until the answer, `deadline`, or `stopped` says the call was let
    /// go of (the person cancelled, a newer message replaced the turn).
    /// Never called from an asynchronous task: that one awaits
    /// [`Self::answer`] and drops it to stop.
    pub fn answer_blocking(
        &self,
        request: &ModelRequest,
        deadline: Instant,
        stopped: &(dyn Fn() -> bool + Sync),
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
                () = until(stopped) => {
                    Err(ModelRefusal::Provider(ModelError::new(ErrorKind::Cancelled)))
                }
            }
        })
    }

    /// One answer in words to `instructions` about `input`, with no tools:
    /// a judgment, a label, a summary. For a caller on a thread of its own,
    /// as [`Self::answer_blocking`] is.
    pub fn say_blocking(
        &self,
        (instructions, input): (&str, &str),
        max_output_tokens: u32,
        deadline: Instant,
        stopped: &(dyn Fn() -> bool + Sync),
    ) -> Result<Answer, ModelRefusal> {
        let request = ModelRequest {
            messages: vec![
                Message::text(Role::System, instructions),
                Message::text(Role::User, input),
            ],
            tools: Vec::new(),
            tool_choice: ToolChoice::None,
            max_output_tokens,
        };
        self.answer_blocking(&request, deadline, stopped, &mut |_| {})
    }

    fn provider(&self) -> Arc<dyn ModelProvider> {
        Arc::clone(&*self.models.provider.read_recover())
    }

    fn resolve(&self) -> Result<Resolved, ModelRefusal> {
        let workspace = self.workspace.as_ref().map(|id| WorkspaceCheck {
            id,
            dir: &self.workspace_dir,
            store: &self.models.destinations,
        });
        resolve(
            &self.root,
            &self.kept,
            (self.kind, self.named.as_deref()),
            workspace,
            self.models.private_addresses.load(Ordering::Acquire),
            &*self.provider(),
        )
    }
}

/// Returns once `stopped` says the call was let go of.
async fn until(stopped: &(dyn Fn() -> bool + Sync)) {
    while !stopped() {
        tokio::time::sleep(STOP_LOOK).await;
    }
}

/// Workspace context needed to verify a workspace's data-destination policy.
struct WorkspaceCheck<'a> {
    id: &'a WorkspaceId,
    dir: &'a Path,
    store: &'a destinations::Store,
}

/// Enforce `workspace`'s data-destination policy against `profile`.
///
/// If the workspace forbids sending content to `profile`, returns a structured
/// [`ModelRefusal::WorkspaceRefuses`] naming the provider, sanitized blocked
/// destination, and any allowed alternatives. If the workspace policy file on
/// disk is unreadable or corrupt, fails closed and refuses dispatch.
fn may_send(
    workspace: Option<WorkspaceCheck<'_>>,
    config: &agent_provider_config::ProviderConfig,
    profile: &ProviderProfile,
    reachable: impl Fn(&ProviderProfile) -> bool,
) -> Result<(), ModelRefusal> {
    let Some(workspace) = workspace else {
        return Ok(());
    };
    let policy = match workspace.store.get(workspace.dir, workspace.id) {
        Ok(policy) => policy,
        Err(error) => {
            tracing::warn!(
                workspace = %workspace.id,
                %error,
                "workspace data-destination policy could not be read; refusing model call"
            );
            return Err(ModelRefusal::WorkspaceRefuses {
                provider: profile.display_label().to_owned(),
                destination: destinations::sanitize_destination(&profile.endpoint),
                allowed_alternatives: Vec::new(),
            });
        }
    };
    if policy.permits(profile) {
        return Ok(());
    }
    let allowed_alternatives = policy.allowed_alternatives(config, reachable);
    Err(ModelRefusal::WorkspaceRefuses {
        provider: profile.display_label().to_owned(),
        destination: destinations::sanitize_destination(&profile.endpoint),
        allowed_alternatives,
    })
}

/// Determine whether an answer's cost is known or unpriced.
fn answer_cost(model_id: &str, response: &ModelResponse) -> AnswerCost {
    if response.cost_micros > 0 {
        return AnswerCost::Known(response.cost_micros);
    }
    #[cfg(feature = "agents")]
    {
        if tilo_model_gateway::pricing::price_of(model_id).is_some() {
            AnswerCost::Known(0)
        } else {
            AnswerCost::Unknown
        }
    }
    #[cfg(not(feature = "agents"))]
    {
        let _ = model_id;
        let used_tokens = response.usage.total_tokens > 0
            || response.usage.input_tokens > 0
            || response.usage.output_tokens > 0;
        if used_tokens {
            AnswerCost::Unknown
        } else {
            AnswerCost::Known(0)
        }
    }
}

/// Conservative upper-bound pre-call cost estimate for budget reservation.
#[cfg(feature = "agents")]
fn estimate_call_cost_micros(model_id: &str, request: &ModelRequest) -> Option<u64> {
    let input_tokens = estimate_request_input_tokens(request);
    tilo_model_gateway::pricing::estimated_max_cost_micros(
        model_id,
        input_tokens,
        u64::from(request.max_output_tokens),
    )
}

#[cfg(not(feature = "agents"))]
fn estimate_call_cost_micros(_model_id: &str, _request: &ModelRequest) -> Option<u64> {
    None
}

#[cfg(feature = "agents")]
fn estimate_request_input_tokens(request: &ModelRequest) -> u64 {
    let mut bytes: u64 = 0;
    let mut image_tokens: u64 = 0;
    for message in &request.messages {
        for part in &message.content {
            match part {
                Content::Text { text } | Content::Reasoning { text, .. } => {
                    bytes = bytes.saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX));
                }
                Content::ToolCall(call) => {
                    bytes = bytes
                        .saturating_add(u64::try_from(call.name.len()).unwrap_or(u64::MAX))
                        .saturating_add(
                            u64::try_from(call.input.to_string().len()).unwrap_or(u64::MAX),
                        );
                }
                Content::ToolResult(result) => {
                    bytes = bytes
                        .saturating_add(u64::try_from(result.content.len()).unwrap_or(u64::MAX));
                }
                Content::Image { .. } => {
                    image_tokens = image_tokens.saturating_add(1_000);
                }
            }
        }
    }
    for tool in &request.tools {
        bytes = bytes
            .saturating_add(u64::try_from(tool.name.len()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(tool.description.len()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(tool.input_schema.to_string().len()).unwrap_or(u64::MAX));
    }
    let text_tokens = bytes.div_ceil(4);
    text_tokens.saturating_add(image_tokens)
}

/// A person's choice for one kind of work, read from their store: the
/// provider, the model and the key.
struct Resolved {
    profile: ProviderProfile,
    model: ModelDescriptor,
    key: Option<zeroize::Zeroizing<String>>,
}

impl Resolved {
    /// Whether the model takes everything `request` carries.
    fn takes(&self, request: &ModelRequest) -> Result<(), ModelRefusal> {
        let pictured = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .any(|part| matches!(part, Content::Image { .. }));
        if pictured && !self.model.vision {
            return Err(ModelRefusal::CannotTake {
                model: self.model.display_label().to_owned(),
                what: Input::Picture,
            });
        }
        Ok(())
    }

    fn linked(&self) -> Linked {
        Linked {
            provider: self.profile.display_label().to_owned(),
            provider_id: self.profile.id.clone(),
            protocol: self.profile.protocol,
            model: self.model.clone(),
        }
    }
}

/// The provider and model `named` names in `config`, as provider/model: the
/// Whether `offered` matches `wanted`, by id (with or without `models/`), or by label.
fn model_matches(offered: &ModelDescriptor, wanted: &str) -> bool {
    let bare = offered.id.strip_prefix("models/").unwrap_or(&offered.id);
    let wanted_bare = wanted.strip_prefix("models/").unwrap_or(wanted);
    offered.id.eq_ignore_ascii_case(wanted)
        || bare.eq_ignore_ascii_case(wanted_bare)
        || offered.label.eq_ignore_ascii_case(wanted)
}

/// provider by its id or else by the name the person gave it, whatever the
/// case, switched on, and a model it offers. A model's own name may hold a
/// slash; a provider is named up to the first. Also matches a bare model name
/// across enabled providers.
fn named_selection<'a>(
    config: &'a agent_provider_config::ProviderConfig,
    named: &str,
) -> Option<(&'a ProviderProfile, &'a ModelDescriptor)> {
    let trimmed = named.trim();
    if trimmed.is_empty() {
        return None;
    }
    let on = || config.profiles.iter().filter(|profile| profile.enabled);
    if let Some((provider, model)) = trimmed.split_once('/') {
        let (provider, model) = (provider.trim(), model.trim());
        if let Some(profile) = on()
            .find(|profile| profile.id.eq_ignore_ascii_case(provider))
            .or_else(|| on().find(|profile| profile.display_label().eq_ignore_ascii_case(provider)))
            && let Some(found) = profile
                .models
                .iter()
                .find(|offered| model_matches(offered, model))
        {
            return Some((profile, found));
        }
        if let Some(found) = on().find_map(|profile| {
            profile
                .models
                .iter()
                .find(|offered| model_matches(offered, model))
                .map(|found| (profile, found))
        }) {
            return Some(found);
        }
    }
    on().find_map(|profile| {
        profile
            .models
            .iter()
            .find(|offered| model_matches(offered, trimmed))
            .map(|found| (profile, found))
    })
}

/// The other kind of a person's main thread: their assistant answers there
/// as a request and works there as a task, and Settings chooses one model
/// for both with one ask. A sub-agent with no dedicated model falls back to
/// the person's task model.
const fn beside_in_main_thread(kind: Interaction) -> Option<Interaction> {
    match kind {
        Interaction::Task => Some(Interaction::Request),
        Interaction::Request | Interaction::Subtask => Some(Interaction::Task),
        Interaction::OneShot | Interaction::Classifier | Interaction::Dictation => None,
    }
}

/// What the person whose store is `root` chose for `kind`, with its key:
/// the model `named` names where their settings hold it
/// ([`PersonModel::naming`]), else the model chosen for the kind, else their
/// default provider's default model, else for a kind of their main thread
/// the model they chose for its other kind ([`beside_in_main_thread`]): a
/// device that chooses the two with two asks leaves one of them unnamed for
/// a moment, and a message of that moment is answered by the one model its
/// person did choose, not told that none is linked.
fn resolve(
    root: &Path,
    kept: &kept::Key,
    (kind, named): (Interaction, Option<&str>),
    workspace: Option<WorkspaceCheck<'_>>,
    private_addresses: bool,
    provider: &dyn ModelProvider,
) -> Result<Resolved, ModelRefusal> {
    // A person who never set anything up keeps no store, and asking about
    // them makes none.
    if !root.is_dir() {
        return Err(ModelRefusal::NoModelLinked { kind });
    }
    let config = provider_config(root);
    let named = named.and_then(|named| named_selection(&config, named));
    let (profile, model) = match (named, config.interaction_models.get(kind.id())) {
        (Some(named), _) => named,
        (None, Some(chosen)) => {
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
        (None, None) => config
            .active_profile()
            .and_then(|profile| {
                let default = profile.default_model.as_ref()?;
                let model = profile.models.iter().find(|model| &model.id == default)?;
                Some((profile, model))
            })
            .or_else(|| {
                beside_in_main_thread(kind).and_then(|other| config.interaction_selection(other))
            })
            .ok_or(ModelRefusal::NoModelLinked { kind })?,
    };
    let named = || profile.display_label().to_owned();
    // Enforce the workspace's data-destination policy before unsealing any
    // vault key or allowing dispatch across any call kind.
    may_send(workspace, &config, profile, |candidate| {
        private_addresses || provider.reaches(&candidate.endpoint)
    })?;
    let public = provider.reaches(&profile.endpoint);
    if !public && !private_addresses {
        return Err(ModelRefusal::AddressRefused { provider: named() });
    }
    // A sign-in in place of a key is kept on the person's own computer, so
    // this host holds nothing to ask such a provider with.
    if profile.sign_in.is_some() {
        return Err(ModelRefusal::NoKey { provider: named() });
    }
    let key = kept.of(root, profile)?;
    // Only a server on the operator's own network may take no key.
    if key.is_none() && public {
        return Err(ModelRefusal::NoKey { provider: named() });
    }
    Ok(Resolved {
        profile: profile.clone(),
        model: model.clone(),
        key,
    })
}
