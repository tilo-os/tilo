//! The Browser windows a host places for its workspaces, as a turn reaches them: its open of
//! Browser has the window placed, its page actions are handed to that window, and a field of
//! the page is filled from its person's vault by reference. What places and holds the windows
//! is the host's own ([`Windows`]); the words a turn and the host share about them are here.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tilo_actions::remote::{ActionCallError, Caller, RemoteActionMessage, RemoteActionTarget};
use tilo_actions::selection::logical_resource_id;
use tilo_actions::{InvocationId, InvocationUpdate, remote::InvokeRequest};
use tilo_server_protocol::WorkspaceId;
use zeroize::Zeroizing;

/// What a call to the windows comes to, once it ends.
pub type Ends<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The Browser windows a host places for its workspaces, as a turn reaches them.
pub trait Windows: Send + Sync {
    /// The provider the window up in `workspace_id` publishes its page actions under
    /// (`browser@42`), where one is up.
    fn window_provider(&self, workspace_id: WorkspaceId) -> Option<String>;

    /// Hand the page action `request` to the window in `workspace_id`, for `person`: how it
    /// ended, as the window said it.
    ///
    /// # Errors
    /// The host's own refusals, in its own words.
    fn page_for_turn(
        &self,
        workspace_id: WorkspaceId,
        person: [u8; 32],
        request: InvokeRequest,
    ) -> Ends<'_, Result<InvocationUpdate, ActionCallError>>;

    /// Have the window in `workspace_id` fill the field `fill` names, the item's text decided
    /// by `releasing` for the site the window says its page is at: how the fill ended.
    ///
    /// # Errors
    /// The host's own refusals, in its own words.
    fn fill_for_turn(
        &self,
        workspace_id: WorkspaceId,
        fill: TurnFill,
        releasing: Releasing,
    ) -> Ends<'_, Result<InvocationUpdate, ActionCallError>>;

    /// The answer to the open `invocation` of Browser that ran in `workspace_id` as `caller`:
    /// `made`, with the window placed on the resource it made, or the refusal in its place.
    fn placed(
        self: Arc<Self>,
        workspace_id: WorkspaceId,
        invocation: InvocationId,
        made: RemoteActionMessage,
        caller: &Caller,
    ) -> Ends<'_, RemoteActionMessage>;
}

/// The resource the window's page actions are actions of (`browser.page`);
/// their provider names the window (`browser@42`).
pub const PAGE_RESOURCE: &str = "browser.page";

/// Whether `target` is an action of the page `provider`'s window publishes:
/// the window's provider and the page's resource.
#[must_use]
pub fn page_action(target: &RemoteActionTarget, provider: &str) -> bool {
    target.provider == provider && logical_resource_id(&target.address.resource) == PAGE_RESOURCE
}

/// The record an answered open of Browser made, when it made one: the
/// record its answer names.
#[must_use]
pub fn opened_resource(message: &RemoteActionMessage) -> Option<String> {
    let RemoteActionMessage::Receipt(receipt) = message else {
        return None;
    };
    let tilo_actions::remote::ReceiptStatus::Completed { output } = &receipt.status else {
        return None;
    };
    output
        .value
        .as_ref()
        .and_then(|value| value.get("record"))
        .and_then(|record| record.as_str())
        .map(str::to_owned)
}

/// What a turn asks a window to fill.
pub struct TurnFill {
    /// The turn's own call: the page action the window is handed carries its id.
    pub invocation: InvocationId,
    /// The field, by its reference in the page's outline.
    pub element: String,
    /// The item, by its reference.
    pub item: String,
    /// Why the agent says it needs the item; empty when it gave none.
    pub reason: String,
}

/// What comes of the vault item, for the site a window says its page is at.
pub enum Release {
    /// The text and the kind of field it is for, in the engine's word: held for the one
    /// sealing and wiped.
    Text {
        text: Zeroizing<String>,
        into: &'static str,
    },
    /// The person is asked first and nothing is released: what the agent is told.
    Asked(String),
    /// Not released there, and why, in words that name no secret.
    Refused(String),
}

/// What decides one fill once its window said where its page is: called at most once, with
/// that origin.
pub type Releasing =
    Box<dyn FnOnce(String) -> Pin<Box<dyn Future<Output = Release> + Send>> + Send>;
