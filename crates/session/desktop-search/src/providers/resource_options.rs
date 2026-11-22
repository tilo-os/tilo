//! What a resource-kind parameter may take: the items the workspace already keeps of that kind,
//! by the list action its kind names.
//!
//! An action's parameter can name a kind of resource instead of asking for its address
//! ([`tilo_actions::ParamType::Resource`]): a vault item, a folder, a page. Whoever fills it in
//! picks one of the workspace's own rather than typing its address. This source answers that
//! pick: for the kind the parameter names, it asks the list action the kind is listed by
//! ([`LISTS`]) through the session's action host, follows the invocation until it settles, and
//! answers what a row shows of each item.
//!
//! It is wired where the catalogue's rows are ([`super::HostActions`]): one provider over the
//! session's action host, mounted by every host that serves a workspace's catalogue. A host
//! without one leaves the parameter to the app's own code, as a type the palette has no control
//! for.

use std::sync::Arc;

use serde_json::Value;
use tilo_actions::remote::{
    ActionProvider, Caller, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionTarget, pending, person_vault, settle_with,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use ui::time::Duration;

/// How long the list's invocation is followed before it is taken as unfinished.
const LIST_WAIT: Duration = Duration::from_secs(2);

/// How long a provider holds the invocation's admission before answering, so a quick list
/// answers in one round trip.
const ANSWER_WAIT_MS: u32 = 2_000;

/// One item a resource-kind parameter may take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceOption {
    /// What fills the parameter: the item's stable address (`vault.item:<id>`).
    pub value: String,
    /// What the item is called, where it has a name.
    pub title: String,
    /// What kind of thing it is ("login", "ssh-key").
    pub kind: String,
    /// Where it belongs, each a site or a server.
    pub places: Vec<String>,
}

impl ResourceOption {
    /// What its row leads with: its title, else what kind of thing it is and where it belongs.
    #[must_use]
    pub fn label(&self) -> String {
        if self.title.is_empty() {
            self.detail()
        } else {
            self.title.clone()
        }
    }

    /// What its row says under the title: what kind of thing it is and where it belongs, when
    /// the title said it by name.
    #[must_use]
    pub fn subtitle(&self) -> Option<String> {
        (!self.title.is_empty()).then(|| self.detail())
    }

    /// The kind and the places, as one line.
    fn detail(&self) -> String {
        let mut parts = Vec::with_capacity(2);
        if !self.kind.is_empty() {
            parts.push(self.kind.clone());
        }
        if !self.places.is_empty() {
            parts.push(self.places.join(", "));
        }
        parts.join(" \u{b7} ")
    }

    /// Whether the person's `needle`, lowercased, names it.
    pub(crate) fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.label().to_lowercase().contains(needle)
            || self.kind.to_lowercase().contains(needle)
            || self
                .places
                .iter()
                .any(|place| place.to_lowercase().contains(needle))
    }
}

/// One list action a resource kind's items come from.
pub struct ResourceList {
    /// The kind of resource a parameter names (`vault.item`).
    pub kind: &'static str,
    /// The provider that answers the list.
    pub provider: &'static str,
    /// The resource the list is an action of.
    pub resource: &'static str,
    /// The action that lists them.
    pub action: &'static str,
    /// How what it answers reads as the items.
    read: fn(Option<&Value>) -> Result<Vec<ResourceOption>, String>,
}

/// The list actions a resource-kind parameter is filled from, by the kind it names.
pub const LISTS: &[ResourceList] = &[ResourceList {
    kind: person_vault::ITEM_KIND,
    provider: person_vault::PROVIDER,
    resource: person_vault::RESOURCE,
    action: person_vault::LIST,
    read: read_kept_items,
}];

/// What a resource-kind parameter's field lists: the items of the kind it names.
pub trait ResourceSource: Send + Sync {
    /// Whether `kind` is one this lists.
    fn lists(&self, kind: &str) -> bool;

    /// Ask for the items of `kind`, or `None` when nothing lists it. The answer comes on the
    /// host's clock: poll it from a frame ([`Pending::poll_ready`]), which the answer's wake
    /// asks for.
    fn ask(&self, kind: &str) -> Option<Pending<Result<Vec<ResourceOption>, String>>>;
}

/// The workspace's list actions, asked through the session's action host ([`ActionProvider`]).
pub struct ResourceLists {
    provider: Arc<dyn ActionProvider + Send + Sync>,
    caller: Caller,
    /// What asks the device's shell for the pass that lists an answer.
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl ResourceLists {
    /// `provider`, the session's action host, asked as `caller`, in the workspace they act in;
    /// `wake` asks for the pass that lists an answer.
    #[must_use]
    pub fn new(
        provider: Arc<dyn ActionProvider + Send + Sync>,
        caller: Caller,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            provider,
            caller,
            wake,
        }
    }
}

impl ResourceSource for ResourceLists {
    fn lists(&self, kind: &str) -> bool {
        LISTS.iter().any(|list| list.kind == kind)
    }

    fn ask(&self, kind: &str) -> Option<Pending<Result<Vec<ResourceOption>, String>>> {
        let list = LISTS.iter().find(|list| list.kind == kind)?;
        let read = list.read;
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: list.provider.to_owned(),
                address: ActionAddress::new(list.resource, list.action),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: Some(ANSWER_WAIT_MS),
            appearance: None,
            signed: None,
        };
        let (completer, answer) = pending();
        let provider = Arc::clone(&self.provider);
        let caller = self.caller.clone();
        let wake = Arc::clone(&self.wake);
        self.provider
            .invoke(&self.caller, request)
            .then(move |receipt| {
                match receipt {
                    Some(Ok(receipt)) => match receipt.status {
                        ReceiptStatus::Accepted => {
                            let now = ui::time::now();
                            let until = now.checked_add(LIST_WAIT).unwrap_or(now);
                            settle_with(&provider, &caller, receipt.invocation, None, until).then(
                                move |state| {
                                    completer.complete(settled(state.flatten(), read));
                                    wake();
                                },
                            );
                        }
                        status => {
                            completer.complete(answered(status, read));
                            wake();
                        }
                    },
                    Some(Err(error)) => {
                        completer.complete(Err(error.to_string()));
                        wake();
                    }
                    // The host abandoned the answer: nothing reads it.
                    None => {}
                }
            });
        Some(answer)
    }
}

/// What the list answered at once, or why it did not.
fn answered(
    status: ReceiptStatus,
    read: fn(Option<&Value>) -> Result<Vec<ResourceOption>, String>,
) -> Result<Vec<ResourceOption>, String> {
    match status {
        ReceiptStatus::Completed { output } => read(output.value.as_ref()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        ReceiptStatus::Cancelled { .. } => {
            Err("the list was stopped before it answered".to_owned())
        }
        // Handled by the caller, which follows an accepted invocation instead.
        ReceiptStatus::Accepted => Err("the list is still to answer".to_owned()),
    }
}

/// What an invocation that had to be followed answered, or why it did not settle.
fn settled(
    state: Option<InvocationState>,
    read: fn(Option<&Value>) -> Result<Vec<ResourceOption>, String>,
) -> Result<Vec<ResourceOption>, String> {
    match state.map(|state| state.update) {
        Some(InvocationUpdate::Succeeded { output, .. }) => read(output.value.as_ref()),
        Some(InvocationUpdate::Failed { error, .. }) => Err(error.message),
        Some(InvocationUpdate::Cancelled { .. }) => {
            Err("the list was stopped before it answered".to_owned())
        }
        _ => Err("the list did not finish".to_owned()),
    }
}

/// A list of the workspace's vault items, as [`person_vault::LIST`] answers it
/// ([`person_vault::KeptItems`]): each item's reference, what it is called and where it belongs,
/// never its secret.
fn read_kept_items(value: Option<&Value>) -> Result<Vec<ResourceOption>, String> {
    let Some(value) = value else {
        return Err("the vault listed nothing".to_owned());
    };
    let kept: person_vault::KeptItems = serde_json::from_value(value.clone())
        .map_err(|_| "the vault answered something that is no list of items".to_owned())?;
    Ok(kept
        .items
        .into_iter()
        .map(|item| ResourceOption {
            value: item.reference.to_string(),
            title: item.title.unwrap_or_default(),
            kind: item.kind.word().to_owned(),
            places: item.belongs_to,
        })
        .collect())
}
