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
    /// The window up in `workspace_id`, as its page actions name it, where one is up.
    fn page_window(&self, workspace_id: WorkspaceId) -> Option<PageWindow>;

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

/// A Browser window up in a workspace, as its page actions name it: the provider the window
/// publishes under (`42`) and its page, the Browser's own `Page` record
/// (`tl:so.tilo.browser:<page-id>`), whose actions are the page actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageWindow {
    pub provider: String,
    pub page: String,
}

impl PageWindow {
    /// The window `provider` names, holding the page the open of Browser answered as `record`.
    #[must_use]
    pub fn new(provider: impl Into<String>, record: &str) -> Self {
        Self {
            provider: provider.into(),
            page: page_resource(record),
        }
    }

    /// The Browser's page action `action` of this window's page.
    #[must_use]
    pub fn target(&self, action: &str) -> RemoteActionTarget {
        RemoteActionTarget {
            provider: self.provider.clone(),
            address: tilo_actions::ActionAddress::new(&self.page, action),
        }
    }

    /// Whether `target` is an action of this window's page: its provider and its page record.
    #[must_use]
    pub fn names(&self, target: &RemoteActionTarget) -> bool {
        target.provider == self.provider
            && logical_resource_id(&target.address.resource) == self.page.as_str()
    }
}

/// The Browser's `Page` record `record`, as its page actions name it
/// (`tl:so.tilo.browser:<page-id>`).
#[must_use]
pub fn page_resource(record: &str) -> String {
    format!("tl:{}:{record}", tilo_app_id::apps::BROWSER)
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

/// What an agent is told of a page fill that waits for its person: nothing was filled yet.
const ASKED_PAGE: &str = "Your person is asked whether you may fill this vault item into this \
     site's page. Nothing was filled yet.";

/// What decides the fill `request` that `caller` asked of what `wanted` names, once the window
/// says the site its page is at: the workspace's one decision on an agent's use of its vault
/// ([`tilo_person_services::PersonServices::release_for`]), as the agent itself, on its grants
/// and its person's. A released text is handed on once, to be sealed to the window; a fill that
/// waits for the person leaves what they are shown in `asks`.
#[must_use]
pub fn releasing(
    workspaces: tilo_workspace_service::workspaces::Workspaces,
    (caller, request): (Caller, InvokeRequest),
    wanted: tilo_workspace_records::vault::FillField,
    asks: Arc<std::sync::Mutex<Option<tilo_actions::remote::asked::AskedFacts>>>,
) -> Releasing {
    Box::new(move |origin: String| {
        let decided: Pin<Box<dyn Future<Output = Release> + Send>> = Box::pin(async move {
            let fill = tilo_person_services::hosted_use::PageFill {
                site: &origin,
                item: &wanted.item,
                reason: wanted.reason.trim(),
            };
            let released = tilo_person_services::PersonServices::of(&workspaces)
                .release_for(&caller, fill, &request)
                .await;
            match released {
                Ok(text) => Release::Text {
                    text: Zeroizing::new(text.text().to_owned()),
                    into: text.field().as_str(),
                },
                Err(ActionCallError::ConfirmationRequired(facts)) => {
                    *asks
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(*facts);
                    Release::Asked(ASKED_PAGE.to_owned())
                }
                Err(ActionCallError::Refused { reason }) => Release::Refused(reason),
                Err(error) => Release::Refused(error.to_string()),
            }
        });
        decided
    })
}
