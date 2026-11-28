//! Reaching an action once its address resolved: a resource named the way an answer names it
//! (`tasks.tracker/issues/7`), starting the app that publishes it when none runs, and the
//! call itself, followed until it settles.

use std::time::Duration;

use super::walk::{AppRef, Levels, ROOT_PEEK, Resolved, SETTLE_WAIT, START_WAIT, Tree, Unresolved};
use super::{Level, Node};
use crate::remote::{
    APPLICATION_RESOURCE_PREFIX, APPLICATION_START_ACTION, ActionCallError, ActionProvider,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget, workspace_provider,
};
use crate::{ActionAddress, InvocationId, InvocationUpdate};

/// What one call came to: its receipt, and the state it settled in (or stood in when the
/// wait ended) for an admitted one.
#[derive(Clone, Debug)]
pub struct Called {
    pub receipt: InvocationReceipt,
    pub state: Option<InvocationState>,
}

impl Called {
    /// Whether the call has ended: rejected, finished, or settled while it was followed.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.receipt.status.is_terminal()
            || self
                .state
                .as_ref()
                .is_some_and(InvocationState::is_terminal)
    }

    /// The update it ended with, when it ended in one.
    #[must_use]
    pub fn update(&self) -> Option<&InvocationUpdate> {
        self.state.as_ref().map(|state| &state.update)
    }
}

impl<P: ActionProvider + Sync + Clone + Send + 'static> Tree<P> {
    /// Call `target` with `argument` under `invocation`, at `revision` when the caller read it
    /// at one, and follow it for `wait` (none: [`SETTLE_WAIT`]).
    ///
    /// A revision is one the caller read a few calls ago, and a directory moves on whenever
    /// its app's page does: the caller is after the action, not that snapshot. A call refused
    /// as named at a revision the directory left behind is made once more without one, which
    /// the provider resolves against the directory as it is now.
    ///
    /// # Errors
    /// The provider refused it before admitting it, or dropped it unanswered.
    pub async fn invoke(
        &self,
        target: RemoteActionTarget,
        argument: InvokeArgument,
        invocation: InvocationId,
        revision: Option<String>,
        wait: Option<Duration>,
    ) -> Result<Called, Unresolved> {
        let request = |revision: Option<String>| InvokeRequest {
            asks: false,
            invocation,
            target: target.clone(),
            argument: argument.clone(),
            context: None,
            confirmation: None,
            revision,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let named = revision.clone().filter(|revision| !revision.is_empty());
        let mut receipt = self.admit(request(revision)).await;
        if named.is_some()
            && matches!(
                receipt,
                Err(Unresolved::Asked(ActionCallError::StaleRevision))
            )
        {
            receipt = self.admit(request(None)).await;
        }
        let receipt = receipt?;
        if let ReceiptStatus::Rejected { error } = &receipt.status {
            return Err(Unresolved::Asked(error.clone()));
        }
        let state = if receipt.status.is_terminal() {
            self.observed(&receipt.invocation).await
        } else {
            self.settled(&receipt.invocation, wait.unwrap_or(SETTLE_WAIT))
                .await
        };
        Ok(Called { receipt, state })
    }

    async fn admit(&self, request: InvokeRequest) -> Result<InvocationReceipt, Unresolved> {
        match self.provider().invoke(self.caller(), request).await {
            Some(Ok(receipt)) => Ok(receipt),
            Some(Err(refused)) => Err(Unresolved::Asked(refused)),
            None => Err(Unresolved::Stopped),
        }
    }

    /// The state of `invocation` once it settles, or as it stands when `wait` has passed:
    /// looked at again each time the provider says it may have moved, else after a pause.
    pub async fn settled(
        &self,
        invocation: &InvocationId,
        wait: Duration,
    ) -> Option<InvocationState> {
        let until = self.pause().now().saturating_add(wait);
        loop {
            let change = self.provider().next_change(self.caller(), invocation);
            let state = self.observed(invocation).await;
            let now = self.pause().now();
            if state.as_ref().is_none_or(InvocationState::is_terminal) || now >= until {
                return state;
            }
            let left = until.saturating_sub(now);
            let pause = self.pause().pause(left.min(Duration::from_millis(250)));
            match change {
                Some(change) => {
                    super::walk::within(
                        &**self.pause(),
                        left.min(Duration::from_millis(250)),
                        change,
                    )
                    .await;
                }
                None => pause.await,
            }
        }
    }

    /// The provider of a running instance of the installed app `app_id`, starting the app in
    /// the workspace when none runs yet, without a window: where an invocation of the app's
    /// own actions goes. `wait` is how long its first wait lasts.
    ///
    /// # Errors
    /// The caller may not start it, or it did not start.
    pub async fn started_provider(
        &self,
        app_id: &str,
        wait: Option<Duration>,
    ) -> Result<String, Unresolved> {
        let (app_id, opens) = match self.app_ref(app_id).await? {
            AppRef::Running { provider, .. } => return Ok(provider),
            AppRef::Installed { app_id, opens } => (app_id, opens),
        };
        if !opens {
            return Err(Unresolved::MayNotOpen { name: app_id });
        }
        // A start still pending is followed under its own id, which answers how it went
        // rather than starting the app again: a workspace starting many apps at once (hundreds
        // of fresh ones on a host, each its first) takes longer than one wait, and the call
        // waits for its app rather than failing.
        let start = InvocationId::new();
        let target = RemoteActionTarget {
            provider: workspace_provider(self.workspace()),
            address: ActionAddress::new(
                format!("{APPLICATION_RESOURCE_PREFIX}{app_id}"),
                APPLICATION_START_ACTION,
            ),
        };
        let deadline = self.pause().now().saturating_add(START_WAIT);
        let mut called = self
            .invoke(
                target.clone(),
                InvokeArgument::Json(serde_json::Map::new()),
                start,
                None,
                wait,
            )
            .await?;
        while !called.ended() && self.pause().now() < deadline {
            called = self
                .invoke(
                    target.clone(),
                    InvokeArgument::Json(serde_json::Map::new()),
                    start,
                    None,
                    None,
                )
                .await?;
        }
        let output = match (&called.receipt.status, called.update()) {
            (_, Some(InvocationUpdate::Succeeded { output, .. }))
            | (ReceiptStatus::Completed { output }, None) => output.value.clone(),
            (_, Some(InvocationUpdate::Failed { error, .. }))
            | (ReceiptStatus::Failed { error }, None) => {
                return Err(Unresolved::NotStarted {
                    app_id,
                    why: error.message.clone(),
                    code: error.code.clone(),
                });
            }
            (_, Some(InvocationUpdate::Cancelled { .. }))
            | (ReceiptStatus::Cancelled { .. }, None) => {
                return Err(Unresolved::NotStarted {
                    app_id,
                    why: "it was cancelled".to_owned(),
                    code: String::new(),
                });
            }
            _ => {
                return Err(Unresolved::NotStarted {
                    app_id,
                    why: "it is still starting; ask again".to_owned(),
                    code: String::new(),
                });
            }
        };
        // The instance the workspace started, as it names it; a host serving the workspace's
        // records names the record it started the app on.
        let output = output.unwrap_or_default();
        let started = output
            .get("peer_id")
            .and_then(serde_json::Value::as_u64)
            .map(|peer| peer.to_string())
            .or_else(|| {
                output
                    .get("provider")
                    .and_then(serde_json::Value::as_str)
                    .filter(|provider| *provider == app_id)
                    .map(str::to_owned)
            })
            .ok_or_else(|| {
                Unresolved::Said(format!("{app_id} started, but named no instance of it"))
            })?;
        // The app runs now on `started`: the call after this one invokes it where it is rather
        // than starting it again.
        self.saw_running(&app_id, &started);
        Ok(started)
    }

    /// The resource `address` names, written either as its listing publishes
    /// it (`chat.messages/messages/m1`) or in the stable form a link carries
    /// (`chat.messages.messages:m1`): both name one resource, so both are
    /// answered alike.
    ///
    /// # Errors
    /// A level on the way could not be read.
    pub async fn by_item_address(
        &self,
        app_id: &str,
        provider: &str,
        address: &str,
        levels: &mut Levels,
    ) -> Result<Option<Resolved>, Unresolved> {
        if let Some(found) = self.by_address(app_id, provider, address, levels).await? {
            return Ok(Some(found));
        }
        let Some(form) = crate::item_form_of(address) else {
            return Ok(None);
        };
        self.by_address(app_id, provider, &form, levels).await
    }

    /// The resource the app lists under exactly `address`, with its actions
    /// and what it holds: an answer's own naming of what it made or changed.
    ///
    /// # Errors
    /// A level on the way could not be read.
    pub async fn by_address(
        &self,
        app_id: &str,
        provider: &str,
        address: &str,
        levels: &mut Levels,
    ) -> Result<Option<Resolved>, Unresolved> {
        let root = self.app_children(provider, levels).await?;
        let mut found = root
            .children
            .iter()
            .find(|node| node.address_is(address))
            .cloned();
        // What a folder of a resource holds is a level further down than
        // the app's own listing reaches (`notes.notebook/page/<id>/body`).
        if found.is_none() {
            let containers: Vec<Node> = root
                .children
                .iter()
                .filter(|node| node.container)
                .take(ROOT_PEEK)
                .cloned()
                .collect();
            let addressed = |level: &Level| -> Result<Option<Node>, Unresolved> {
                Ok(level
                    .whole_children()
                    .iter()
                    .find(|node| node.address_is(address))
                    .cloned())
            };
            for container in containers {
                // An address that begins with a container's key names one of the
                // container's own, so its level is read on past its bound to find
                // it; any other container is looked through as far as it was read.
                let own = address
                    .strip_prefix(container.key.as_str())
                    .is_some_and(|rest| rest.starts_with('/'));
                let node = if own {
                    self.level_holding(provider, &container.id, levels, addressed)
                        .await?
                        .0
                } else {
                    addressed(&self.level(provider, &container.id, levels).await?)?
                };
                if let Some(node) = node {
                    found = Some(node);
                    break;
                }
            }
        }
        let Some(node) = found else {
            return Ok(None);
        };
        let here = self.level(provider, &node.id, levels).await?;
        Ok(Some(Resolved {
            app_id: app_id.to_owned(),
            provider: provider.to_owned(),
            path: address.to_owned(),
            actions: if here.own.is_empty() {
                node.actions.clone()
            } else {
                here.own.clone()
            },
            children: here.children.clone(),
            node: Some(node.clone()),
            chain: vec![node],
            levels: std::mem::take(levels),
            revision: here.revision,
        }))
    }
}
