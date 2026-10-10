//! Shared, device-local close admission. Saving an external document still
//! goes through its declared action; host chrome owns only the close decision.
#[cfg(not(target_arch = "wasm32"))]
use crate::widget::{Button, Column, ModalDialog, Text, Widget};
#[cfg(not(target_arch = "wasm32"))]
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
use tilo_actions::ActionAddress;
#[cfg(not(target_arch = "wasm32"))]
use tilo_actions::{ActionSpec, OperationKind, Resource};

/// An external editable document which cannot be replaced by an in-memory
/// record flush or a lossy recovery export. `revision` identifies the exact
/// content offered for discard; it must not mutate/commit the live document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsavedDocument {
    pub save: ActionAddress,
    pub identity: String,
    pub revision: Vec<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) const OPERATION: &str = "tilo.host.close";
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Command {
    pub session: uuid::Uuid,
    pub choice: Choice,
}
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Choice {
    Save,
    Discard,
    Retry,
    KeepEditing,
}
#[cfg(not(target_arch = "wasm32"))]
impl Choice {
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::Discard => "discard",
            Self::Retry => "retry",
            Self::KeepEditing => "keep-editing",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Save => "close-save",
            Self::Discard => "close-discard",
            Self::Retry => "close-retry",
            Self::KeepEditing => "close-keep-editing",
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Admission {
    pub dialog: Option<Box<ModalDialog<Column>>>,
    pub reviewed: Vec<UnsavedDocument>,
    pub discarded: Vec<UnsavedDocument>,
    pub admitted: bool,
    pub ready: bool,
    pub save_local: Option<tilo_actions::InvocationId>,
    pub save_broker_request: Option<u64>,
    pub save_broker_id: Option<tilo_actions::InvocationId>,
    pub save_poll_at: crate::time::Instant,
    pub save_waiting: bool,
    pub save_result: Option<Result<(), String>>,
    session: uuid::Uuid,
    choices: Vec<Choice>,
    pending: Rc<RefCell<VecDeque<Command>>>,
}
#[cfg(not(target_arch = "wasm32"))]
impl Default for Admission {
    fn default() -> Self {
        Self {
            dialog: None,
            reviewed: Vec::new(),
            discarded: Vec::new(),
            admitted: false,
            ready: false,
            save_local: None,
            save_broker_request: None,
            save_broker_id: None,
            save_poll_at: crate::time::now(),
            save_waiting: false,
            save_result: None,
            session: uuid::Uuid::new_v4(),
            choices: Vec::new(),
            pending: Rc::default(),
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Admission {
    #[expect(
        clippy::expect_used,
        reason = "fixed nullary host action always has a unit binding"
    )]
    pub fn show(
        &mut self,
        title: &str,
        detail: &str,
        choices: &[Choice],
        platform: crate::platform::PlatformConventions,
    ) {
        self.session = uuid::Uuid::new_v4();
        self.choices = choices.to_vec();
        self.pending.borrow_mut().clear();
        let resource = resource_for(self.session, choices);
        let mut content = Column::new()
            .with_intrinsic_height()
            .push(Box::new(Text::new(detail)));
        // These are the existing retained Buttons, so all choices participate
        // in the host's focus trap. Keep Editing is the safe initial focus.
        for choice in [
            Choice::KeepEditing,
            Choice::Save,
            Choice::Retry,
            Choice::Discard,
        ] {
            if !choices.contains(&choice) {
                continue;
            }
            content = content.push(Box::new(
                Button::new(choice.label(), choice.label()).on_action(
                    tilo_actions::ActionBinding::unit_for(&resource, choice.id())
                        .expect("host close unit action"),
                ),
            ));
        }
        let mut dialog =
            ModalDialog::new(crate::i18n::I18nManager::lookup_plain(title), content).open(true);
        let pending = self.pending.clone();
        let command = Command {
            session: self.session,
            choice: Choice::KeepEditing,
        };
        dialog = dialog.on_dismiss(move || pending.borrow_mut().push_back(command.clone()));
        dialog.set_platform_conventions(platform);
        let allowed = resource
            .actions
            .iter()
            .map(|action| ActionAddress::new(resource.id.as_str(), &action.id))
            .collect();
        crate::widget::controls::refresh_control_action_availability(&mut dialog, &allowed);
        self.dialog = Some(Box::new(dialog));
    }
    pub fn take_command(&self) -> Option<Command> {
        self.pending.borrow_mut().pop_front()
    }
    pub fn validate(&self, command: &Command) -> bool {
        self.dialog.is_some()
            && command.session == self.session
            && self.choices.contains(&command.choice)
    }
    pub fn resources(&self) -> Vec<Resource> {
        if self.dialog.is_none() {
            return Vec::new();
        }
        vec![resource_for(self.session, &self.choices)]
    }
    pub fn dismiss(&mut self) {
        // A previously admitted Save may finish normally, but its result must
        // no longer close a window after the user chose Keep Editing.
        self.dialog = None;
        self.reviewed.clear();
        self.discarded.clear();
        self.ready = false;
        self.save_waiting = false;
        self.save_local = None;
        self.save_broker_request = None;
        self.save_broker_id = None;
        self.save_result = None;
        self.choices.clear();
        self.pending.borrow_mut().clear();
    }

    /// Retire the current decision before an explicit choice re-evaluates the
    /// close request. Ordinary duplicate native close requests leave the
    /// presentation intact, including its session and focused control.
    pub(crate) fn retire_presentation(&mut self) {
        self.dialog = None;
        self.choices.clear();
        self.pending.borrow_mut().clear();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn resource_for(session: uuid::Uuid, choices: &[Choice]) -> Resource {
    let mut resource = Resource::new(
        format!("tilo.host.close:{session}"),
        "tilo.host.close",
        crate::i18n::I18nManager::lookup_plain("close-title"),
    );
    for choice in choices {
        if let Ok(action) = ActionSpec::operation(
            choice.id(),
            crate::i18n::I18nManager::lookup_plain(choice.label()),
            OperationKind::View,
            OPERATION,
            &Command {
                session,
                choice: *choice,
            },
        ) {
            resource.actions.push(action);
        }
    }
    resource
}

pub(crate) fn retry_reports(
    reports: &mut [crate::application::ComponentCloseReport],
) -> Vec<String> {
    let mut errors = Vec::new();
    for report in reports {
        let _ = report.retry_record();
        let _ = report.retry_view();
        errors.extend(report.record_error.iter().cloned());
        errors.extend(report.view_error.iter().cloned());
        errors.extend(report.teardown_error.iter().cloned());
    }
    errors
}

#[cfg(feature = "native")]
pub(crate) fn wake_deadline(
    scheduled: Option<crate::time::Instant>,
    waiting: bool,
    now: crate::time::Instant,
) -> Option<crate::time::Instant> {
    let close = waiting.then(|| now + crate::time::Duration::from_millis(100));
    scheduled.into_iter().chain(close).min()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_admission_keeps_deadlines_and_refuses_stale_session_closes() {
        // Case `pending_save_deadline_survives_an_otherwise_idle_host`.
        {
            let now = crate::time::now();
            let close = now + crate::time::Duration::from_millis(100);
            assert_eq!(wake_deadline(None, true, now), Some(close));
            let sooner = now + crate::time::Duration::from_millis(10);
            assert_eq!(wake_deadline(Some(sooner), true, now), Some(sooner));
            assert_eq!(wake_deadline(None, false, now), None);
        }

        // Case `stale_discard_and_late_save_cannot_close_a_new_session`.
        {
            let mut admission = Admission::default();
            admission.show(
                "close-title",
                "close-unsaved",
                &[Choice::Discard, Choice::KeepEditing],
                crate::platform::PlatformConventions::default(),
            );
            let stale = Command {
                session: admission.session,
                choice: Choice::Discard,
            };
            let stale_address = ActionAddress::new(admission.resources()[0].id.as_str(), "discard");
            admission.dismiss();
            admission.show(
                "close-title",
                "close-unsaved",
                &[Choice::Discard, Choice::KeepEditing],
                crate::platform::PlatformConventions::default(),
            );
            assert!(!admission.validate(&stale));
            assert!(tilo_actions::door::resolve(&admission.resources(), &stale_address).is_none());
            admission.save_waiting = true;
            admission.save_local = Some(tilo_actions::InvocationId::new());
            admission.dismiss();
            assert!(!admission.save_waiting && admission.save_local.is_none());
            assert!(admission.resources().is_empty());
        }
    }
}
