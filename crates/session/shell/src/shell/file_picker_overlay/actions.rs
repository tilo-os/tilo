//! Session-local admission and tracked operations for the mounted portal chooser.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use ui::widget::file_picker::semantic::PickerOperation;

use crate::shell::search::action_api::ActionError;
use crate::shell::search::{ActionReceipt, PendingInvocation};
use crate::shell::state::{AppId, WorkspaceId};

#[derive(Clone)]
pub(crate) struct Subject {
    pub id: uuid::Uuid,
    pub workspace: WorkspaceId,
    pub app: AppId,
}

pub(super) struct Command {
    pub subject: uuid::Uuid,
    pub state: Option<String>,
    pub operation: PickerOperation,
    pub reply: Rc<RefCell<Option<Result<serde_json::Value, String>>>>,
}

#[derive(Default)]
struct State {
    subject: Option<Subject>,
    commands: VecDeque<Command>,
}

#[derive(Clone, Default)]
pub(crate) struct PickerActions(Rc<RefCell<State>>);

impl PickerActions {
    pub(crate) fn subject(&self) -> Option<Subject> {
        self.0.borrow().subject.clone()
    }
    pub(super) fn publish(&self, subject: Option<Subject>) {
        self.0.borrow_mut().subject = subject;
    }
    pub(super) fn drain(&self) -> VecDeque<Command> {
        std::mem::take(&mut self.0.borrow_mut().commands)
    }

    pub(crate) fn begin(
        &self,
        subject: uuid::Uuid,
        state: Option<String>,
        operation: PickerOperation,
    ) -> Result<PendingInvocation, ActionError> {
        let mut bridge = self.0.borrow_mut();
        if bridge
            .subject
            .as_ref()
            .is_none_or(|current| current.id != subject)
        {
            return Err(ActionError::Explained(crate::shell::i18n::t(
                "file-picker-expired",
            )));
        }
        if bridge.commands.len() >= 16 {
            return Err(ActionError::Explained(crate::shell::i18n::t(
                "file-picker-busy",
            )));
        }
        let reply = Rc::new(RefCell::new(None));
        bridge.commands.push_back(Command {
            subject,
            state,
            operation,
            reply: reply.clone(),
        });
        drop(bridge);
        Ok(Rc::new(move || match reply.borrow_mut().take() {
            None => Ok(None),
            Some(Ok(value)) => Ok(Some(ActionReceipt::completed_with(value))),
            Some(Err(error)) => Err(ActionError::Refused(error)),
        }))
    }
}
