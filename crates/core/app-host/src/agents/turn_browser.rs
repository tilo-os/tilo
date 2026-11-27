//! A turn's open of Browser.
//!
//! A workspace's Browser window is a session in the workspace's own machine, which this server
//! starts after an open of Browser made the Browser resource
//! ([`Windows`]). A caller on a connection has its open placed
//! where the transport serves it. A turn this server carries out arrives on no connection: its
//! tools act on the workspace's action service, which makes the resource and starts nothing.
//!
//! [`TurnBrowser`] is that open as a provider of the turn's own ([`TurnProvider`]). The
//! workspace's actions decide the open and carry it out as the turn's caller, under the rules
//! the turn carries, as they do for every caller. Only an open they carried out has its window
//! placed, and only in the workspace the turn works in. On a server that has no machine to
//! place a window in, the open is refused in words that say so, before any resource is made.
//!
//! It places a window and nothing else: no page action of a window is carried for a turn here.

use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, InvocationReceipt, InvokeRequest,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget, provider_workspace,
};
use tilo_actions::{InvocationId, InvocationUpdate};

use super::turn_provider::{Ending, Rows};
use super::windows::{Windows, opened_resource};
use super::{Reply, TurnProvider, WorkspaceActionProvider, WorkspaceId, later, unavailable};
use tilo_workspace_service::app_serving::opens_browser_at;

/// What a turn is told where this server has no machine to open a Browser window in.
const NO_MACHINE: &str = "This server has no machine to open a Browser window in, so no web \
     page can be opened from here. Nothing was opened. Tell your person so: they can open \
     Browser on a device of their own.";
/// What a turn is told when the open made nothing a window could be placed on.
const NOTHING_MADE: &str = "Opening Browser did not say what it made, so no window was opened.";
/// What a turn is told when the open did not end in time.
const NOT_ENDED: &str = "Opening Browser did not finish in time, so no window was opened.";
/// How long an open has to make the Browser resource.
const OPEN_WAIT: Duration = Duration::from_secs(30);
/// How long this waits before it looks again at how an open went.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// One turn's open of Browser: what has the window placed once the open made its resource.
pub(super) struct TurnBrowser {
    /// The windows this server places for its workspaces, where it places any.
    browsers: Option<Arc<dyn Windows>>,
    /// The workspace's own actions, as the turn's tools reach them.
    service: WorkspaceActionProvider,
    /// The workspace the turn works in: a window is placed there and nowhere else.
    workspace: String,
    workspace_id: WorkspaceId,
}

impl TurnBrowser {
    /// The open of Browser for the turn `reply` answers, placed through `browsers` where this
    /// server places windows.
    pub(super) fn for_turn(browsers: Option<Arc<dyn Windows>>, reply: &Reply) -> Arc<Self> {
        Arc::new(Self {
            browsers,
            service: reply.provider.clone(),
            workspace: reply.workspace.clone(),
            workspace_id: reply.provider.workspace,
        })
    }

    /// Open Browser as `caller` asks with `request`, and place its window: how the open ended,
    /// or the refusal.
    async fn open(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let invocation = request.invocation;
        // A turn opens Browser in the workspace it works in. An open that names another
        // workspace is answered as one that names nothing: unknown and forbidden are one answer.
        if provider_workspace(&request.target.provider) != Some(self.workspace.as_str()) {
            return Err(ActionCallError::UnknownAction);
        }
        let Some(browsers) = &self.browsers else {
            return Err(ActionCallError::Refused {
                reason: NO_MACHINE.to_owned(),
            });
        };
        // The workspace's actions decide the open, as the turn's caller and under the rules the
        // turn carries, and make the Browser resource.
        let answer = self
            .service
            .serve(caller, RemoteActionMessage::Invoke(request))
            .await;
        let Some(RemoteActionMessage::Receipt(receipt)) = answer else {
            return Err(unavailable());
        };
        let output = match receipt.status {
            ReceiptStatus::Completed { output } => output,
            ReceiptStatus::Accepted => match self.ended(caller, invocation).await? {
                InvocationUpdate::Succeeded { output, .. } => output,
                // Refused, asked of its person first, or stopped: nothing is placed.
                other => return Ok(other),
            },
            ReceiptStatus::Failed { error } => {
                return Ok(InvocationUpdate::Failed { invocation, error });
            }
            ReceiptStatus::Cancelled { message } => {
                return Ok(InvocationUpdate::Cancelled {
                    invocation,
                    message,
                });
            }
            ReceiptStatus::Rejected { error } => return Err(error),
        };
        let made = RemoteActionMessage::Receipt(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Completed { output },
        });
        if opened_resource(&made).is_none() {
            return Err(ActionCallError::Unavailable {
                reason: NOTHING_MADE.to_owned(),
            });
        }
        // The open made its resource: the window is placed on it, in this turn's workspace.
        match Arc::clone(browsers)
            .placed(self.workspace_id, invocation, made, caller)
            .await
        {
            RemoteActionMessage::Receipt(InvocationReceipt {
                status: ReceiptStatus::Completed { output },
                ..
            }) => Ok(InvocationUpdate::Succeeded { invocation, output }),
            RemoteActionMessage::Receipt(InvocationReceipt {
                status: ReceiptStatus::Rejected { error },
                ..
            }) => Err(error),
            _ => Err(unavailable()),
        }
    }

    /// How the open `invocation` of `caller`'s ended, once the workspace's actions say so.
    async fn ended(
        &self,
        caller: &Caller,
        invocation: InvocationId,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let until = later(OPEN_WAIT);
        loop {
            if let Some(state) = self.service.actions.observe(caller, &invocation)
                && state.update.is_terminal()
            {
                return Ok(state.update);
            }
            if tilo_clock::now() > until {
                return Err(ActionCallError::Unavailable {
                    reason: NOT_ENDED.to_owned(),
                });
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }
}

impl TurnProvider for TurnBrowser {
    /// It names no provider of its own: a workspace lists Browser among its apps where this
    /// server places its windows.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        Vec::new()
    }

    /// It answers no listing: the workspace's own names Browser.
    fn rows(&self, _caller: &Caller, _query: &CatalogueQuery) -> Option<Rows> {
        None
    }

    /// An open of Browser is this provider's to answer, also where it refuses it.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        opens_browser_at(target)
    }

    /// A turn's caller changes when its person writes again while it works, so the open is
    /// taken as the caller of the tool call that asked for it.
    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.open(&caller, request).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionAddress;
    use tilo_actions::remote::{
        APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionProvider, InvokeArgument,
        Pending, workspace_provider,
    };

    /// The workspace the turn works in, and another one.
    const HERE: &str = "5f0c1c4e-6f0b-4c53-9d0a-0d1f6a1e2b01";
    const ELSEWHERE: &str = "5f0c1c4e-6f0b-4c53-9d0a-0d1f6a1e2b02";

    /// A dispatch no call reaches: what a turn's open is refused with comes before it.
    struct Unreached;

    impl ActionProvider for Unreached {
        fn query(
            &self,
            _caller: &Caller,
            _query: tilo_actions::remote::CatalogueQuery,
        ) -> Pending<Result<tilo_actions::remote::CataloguePage, ActionCallError>> {
            unreachable!("a refused open asks the dispatch nothing")
        }

        fn invoke(
            &self,
            _caller: &Caller,
            _request: InvokeRequest,
        ) -> Pending<Result<tilo_actions::remote::InvocationReceipt, ActionCallError>> {
            unreachable!("a refused open asks the dispatch nothing")
        }

        fn observe(
            &self,
            _caller: &Caller,
            _id: &InvocationId,
        ) -> Option<tilo_actions::remote::InvocationState> {
            None
        }

        fn cancel(
            &self,
            _caller: &Caller,
            _id: &InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            unreachable!("a refused open asks the dispatch nothing")
        }
    }

    /// No agent key is ever asked for here.
    struct NoKeys;

    impl crate::agents::agent_keys::AgentKeys for NoKeys {
        fn grant(
            &self,
            _workspace: WorkspaceId,
            _agent: &str,
            _name: &str,
            _person: [u8; 32],
        ) -> Result<[u8; 32], String> {
            Err("no agent keys here".into())
        }

        fn sign(
            &self,
            _workspace: WorkspaceId,
            _key: &[u8; 32],
            _request: &mut InvokeRequest,
        ) -> Result<(), String> {
            Err("no agent keys here".into())
        }

        fn made(
            &self,
            _key: &[u8; 32],
        ) -> Option<(tilo_user_identity::DeviceCert, ed25519_dalek::SigningKey)> {
            None
        }
    }

    /// A turn's open of Browser in the workspace `HERE`, on a server that places no windows.
    fn on_a_server_with_no_machine() -> Arc<TurnBrowser> {
        let workspace_id = WorkspaceId::parse(HERE).expect("a workspace id");
        let actions = crate::agents::EngineActions::new(Arc::new(Unreached), Arc::new(NoKeys));
        Arc::new(TurnBrowser {
            browsers: None,
            service: WorkspaceActionProvider::new(actions, workspace_id),
            workspace: HERE.to_owned(),
            workspace_id,
        })
    }

    /// The open of the app `app`, addressed to the workspace `workspace`.
    fn open_of(app: &str, workspace: &str) -> InvokeRequest {
        InvokeRequest {
            asks: false,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: workspace_provider(workspace),
                address: ActionAddress {
                    resource: format!("{APPLICATION_RESOURCE_PREFIX}{app}"),
                    action: APPLICATION_OPEN_ACTION.to_owned(),
                },
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// The person's session in the workspace `HERE`, as a turn's tools call.
    fn caller() -> Caller {
        Caller::authenticated([1; 32], [2; 32]).in_workspace(HERE)
    }

    /// Whether a turn's open of Browser takes a call of `target`.
    fn takes(target: &RemoteActionTarget) -> bool {
        on_a_server_with_no_machine().takes(target)
    }

    /// How an open of Browser addressed to the workspace `workspace` ends for a turn that works
    /// in `HERE`, on a server that places no windows.
    async fn opened_in(workspace: &str) -> Result<InvocationUpdate, ActionCallError> {
        let request = open_of(tilo_app_id::apps::BROWSER, workspace);
        on_a_server_with_no_machine()
            .invoke(caller(), request)
            .await
    }

    #[tokio::test]
    async fn an_open_on_a_server_with_no_machine_is_refused_in_words_that_say_so() {
        assert_eq!(
            opened_in(HERE).await.err(),
            Some(ActionCallError::Refused {
                reason: NO_MACHINE.to_owned()
            }),
        );
        assert!(
            NO_MACHINE.contains("no machine") && NO_MACHINE.contains("Nothing was opened"),
            "the words say why and that nothing was opened: {NO_MACHINE}"
        );
    }

    #[tokio::test]
    async fn an_open_that_names_another_workspace_is_answered_as_unknown() {
        assert_eq!(
            opened_in(ELSEWHERE).await.err(),
            Some(ActionCallError::UnknownAction)
        );
    }

    #[test]
    fn only_the_open_of_browser_is_taken() {
        assert!(takes(&open_of(tilo_app_id::apps::BROWSER, HERE).target));
        assert!(!takes(&open_of("so.tilo.chat", HERE).target));
        let mut read = open_of(tilo_app_id::apps::BROWSER, HERE).target;
        read.address.action = "read_page".to_owned();
        assert!(!takes(&read), "no page action is taken here");
    }
}
