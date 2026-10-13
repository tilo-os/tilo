//! Deferred viewer-local replay calls, resolved by the retained native pane.
use std::{cell::RefCell, rc::Rc};

use super::search::action_api::ActionError;
use super::state::{RoomId, WorkspaceId};
use tilo_actions::EncodedAction;
use tilo_desktop::activity::view_actions::ReplayActionDescriptor;

use super::{
    chrome_state::Chrome,
    search::{ActionReceipt, PendingInvocation},
};

pub type ReplayAnswer = Rc<RefCell<Option<Result<ActionReceipt, ActionError>>>>;

#[derive(Debug)]
pub struct ReplayCall {
    pub scope: (WorkspaceId, RoomId),
    pub generation: u64,
    pub operation: EncodedAction,
    pub answer: ReplayAnswer,
}

#[derive(Debug, Default)]
pub struct ReplayHost {
    pub scope: Option<(WorkspaceId, RoomId)>,
    pub generation: u64,
    pub descriptors: Vec<ReplayActionDescriptor>,
    pub pending: Vec<ReplayCall>,
}

impl ReplayHost {
    pub fn enqueue(
        chrome: &Chrome,
        scope: (WorkspaceId, RoomId),
        operation: EncodedAction,
        generation: u64,
    ) -> Result<PendingInvocation, ActionError> {
        let mut state = chrome.borrow_mut();
        if state.replay_host.generation != generation
            || state.replay_host.scope != Some(scope)
            || !state
                .tree()
                .active_workspace()
                .is_some_and(|workspace| workspace.id == scope.0 && workspace.room == scope.1)
        {
            return Err(ActionError::Refused("Replay workspace changed".into()));
        }
        let answer = Rc::new(RefCell::new(None));
        state.replay_host.pending.push(ReplayCall {
            scope,
            generation,
            operation,
            answer: answer.clone(),
        });
        state.bump();
        Ok(Rc::new(move || match answer.borrow_mut().take() {
            Some(result) => result.map(Some),
            None => Ok(None),
        }))
    }
}
