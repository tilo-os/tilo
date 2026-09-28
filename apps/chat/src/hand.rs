//! A participant without a window, working through Chat.
//!
//! An agent that takes part in a conversation — the feedback triage agent is
//! one — runs Chat itself, headless, as its own participant: its own device,
//! its own comment records, its own grants. It reads what that Chat reads and
//! asks for what a person would click. [`ChatHand`] is that seam and nothing
//! more:
//!
//! - **Reading.** After every model pass Chat publishes its channels and
//!   every message it holds, the conversation document's and those joined
//!   from people's comment records alike, with the statuses and approvals
//!   Editors set. The agent reads a message's address (`reference`), its
//!   attachment and its thread, never its prose.
//! - **Asking.** [`ChatHand::ask`] queues one of Chat's own published
//!   actions by address, exactly as a click on its control does. Chat's host
//!   resolves it against the resource tree filtered for this participant and
//!   runs it through the same door, so an action the participant may not
//!   take is absent, and what it writes is checked by the broker like any
//!   other participant's write.
//!
//! There is no side door: nothing here writes a document.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tilo_actions::{ActionAddress, ControlInvocation};
use ui::permissions::{PermissionContext, PermissionScope};

use crate::model::{Channel, Message};

#[derive(Default)]
struct Held {
    channels: Vec<Channel>,
    messages: Vec<Message>,
    requests: VecDeque<ControlInvocation>,
    refused: Vec<(ActionAddress, String)>,
    permissions: Option<PermissionContext>,
    /// Model passes published so far; a reader waits on a change of it.
    passes: u64,
}

/// A headless participant's view of Chat and its queue of requests. Cheap to
/// clone; every clone is the same hand.
#[derive(Clone, Default)]
pub struct ChatHand {
    held: Arc<Mutex<Held>>,
}

impl std::fmt::Debug for ChatHand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatHand").finish_non_exhaustive()
    }
}

impl ChatHand {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every channel Chat holds.
    #[must_use]
    pub fn channels(&self) -> Vec<Channel> {
        self.held().channels.clone()
    }

    /// The messages of `channel`, replies included, oldest first.
    #[must_use]
    pub fn messages(&self, channel: &str) -> Vec<Message> {
        let mut out: Vec<Message> = self
            .held()
            .messages
            .iter()
            .filter(|m| m.channel == channel)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// How many model passes Chat has published.
    #[must_use]
    pub fn passes(&self) -> u64 {
        self.held().passes
    }

    /// Whether the broker last said this participant may do what `scope`
    /// names. `false` until it has said anything.
    #[must_use]
    pub fn permits(&self, scope: &PermissionScope) -> bool {
        self.held()
            .permissions
            .as_ref()
            .is_some_and(|ctx| ctx.check(scope))
    }

    /// Ask Chat to run its action `address` with `arguments`, as a click on
    /// its control would.
    pub fn ask(
        &self,
        address: ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) {
        self.held()
            .requests
            .push_back(ControlInvocation { address, arguments });
    }

    /// Requests Chat's host refused since the last call, with its reason.
    #[must_use]
    pub fn take_refused(&self) -> Vec<(ActionAddress, String)> {
        std::mem::take(&mut self.held().refused)
    }

    pub(crate) fn publish(&self, channels: &[Channel], messages: &[Message]) {
        let mut held = self.held();
        held.channels = channels.to_vec();
        held.messages = messages.to_vec();
        held.passes = held.passes.wrapping_add(1);
    }

    pub(crate) fn permissions_changed(&self, permissions: &PermissionContext) {
        self.held().permissions = Some(permissions.clone());
    }

    pub(crate) fn next_request(&self) -> Option<ControlInvocation> {
        self.held().requests.pop_front()
    }

    pub(crate) fn has_request(&self) -> bool {
        !self.held().requests.is_empty()
    }

    pub(crate) fn refused(&self, address: &ActionAddress, reason: String) {
        self.held().refused.push((address.clone(), reason));
    }
}
