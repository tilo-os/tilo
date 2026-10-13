//! Deferred viewer-local replay calls, resolved by the retained native pane.
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

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
    pub owner: Weak<()>,
    pub chrome: Weak<RefCell<super::chrome_state::ChromeState>>,
    pub scope: Option<(WorkspaceId, RoomId)>,
    pub generation: u64,
    pub descriptors: Vec<ReplayActionDescriptor>,
    pub pending: Vec<ReplayCall>,
}

impl ReplayHost {
    pub fn enqueue(
        chrome: &Chrome,
        binding: &(Weak<()>, Weak<RefCell<super::chrome_state::ChromeState>>),
        scope: (WorkspaceId, RoomId),
        operation: EncodedAction,
        generation: u64,
    ) -> Result<PendingInvocation, ActionError> {
        let owner = binding
            .0
            .upgrade()
            .ok_or_else(|| ActionError::Refused("Replay pane closed".into()))?;
        let original = binding
            .1
            .upgrade()
            .ok_or_else(|| ActionError::Refused("Replay host closed".into()))?;
        if !Rc::ptr_eq(&original, chrome) {
            return Err(ActionError::Refused("Replay host changed".into()));
        }
        let mut state = original.borrow_mut();
        if !state
            .replay_host
            .owner
            .upgrade()
            .is_some_and(|current| Rc::ptr_eq(&current, &owner))
            || state.replay_host.generation != generation
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
        let binding = binding.clone();
        Ok(Rc::new(move || {
            let Some(owner) = binding.0.upgrade() else {
                return Err(ActionError::Refused("Replay pane closed".into()));
            };
            let Some(chrome) = binding.1.upgrade() else {
                return Err(ActionError::Refused("Replay host closed".into()));
            };
            if !chrome
                .borrow()
                .replay_host
                .owner
                .upgrade()
                .is_some_and(|current| Rc::ptr_eq(&current, &owner))
            {
                return Err(ActionError::Refused("Replay pane changed".into()));
            }
            match answer.borrow_mut().take() {
                Some(result) => result.map(Some),
                None => Ok(None),
            }
        }))
    }
}
