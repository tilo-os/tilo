//! Shared, device-local close admission. Saving an external document still goes through its declared
//! action; host chrome owns only the close decision.
use crate::{widget::Button, widget::Column, widget::ModalDialog, widget::Text};
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionAddress, ActionSpec, OperationKind, ResourceSpec};
use tilo_ui_base::widget::Widget;

/// An external editable document which cannot be replaced by an in-memory
/// record flush or a lossy recovery export. `revision` identifies the exact
/// content offered for discard; it must not mutate/commit the live document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsavedDocument {
    pub save: ActionAddress,
    pub identity: String,
    pub revision: Vec<u8>,
}

pub(crate) const OPERATION: &str = "tilo.host.close";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Command {
    pub session: uuid::Uuid,
    pub choice: Choice,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Choice {
    Save,
    Discard,
    Retry,
    KeepEditing,
}
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

/// How far a closing window has let go of the invocations the broker hands it.
pub(crate) enum Leaving {
    /// It takes invocations.
    Staying,
    /// It asked for no more; the ones already addressed to it are still coming.
    Stopping,
    /// None it was handed is left unanswered; the answers go out with the window's last changes
    /// ([`Barrier`]).
    Left,
}

/// One record a barrier confirms once the workspace answers it: the heads its route
/// had sent when the barrier was asked for.
#[cfg(feature = "model")]
pub(crate) struct Confirming {
    pub store: crate::resource::ResourceStore,
    pub id: crate::resource::DocumentId,
    pub route: tilo_ui_base::collaboration::RouteSpec,
    pub heads: Vec<automerge::ChangeHash>,
}

/// What a window knows of its records' changes reaching the workspace. Its routes publish them,
/// and a barrier on the window's connection answers once everything sent before it
/// was taken or refused: the save indicator says Saved only for what an answered barrier covers,
/// and a closing window goes only once one asked after its last change was answered.
#[cfg(feature = "model")]
#[derive(Default)]
pub(crate) struct Barrier {
    /// The barrier on its way, and what it confirms once answered.
    pub pending: Option<(
        tilo_ui_base::collaboration::DocumentPublicationReceipt,
        Vec<Confirming>,
    )>,
    /// The heads of each route the workspace confirmed last.
    pub confirmed: RouteHeads,
    /// The heads of each route the workspace refused: not asked about again until the route
    /// sends more, or the person tries again.
    pub refused: RouteHeads,
    /// Ask for a barrier even when no record waits for one: a closing window's last answers and
    /// changes ride it.
    pub wanted: bool,
}

/// Heads by route.
#[cfg(feature = "model")]
pub(crate) type RouteHeads =
    std::collections::HashMap<tilo_ui_base::collaboration::RouteSpec, Vec<automerge::ChangeHash>>;

/// What a closing window's components still wait on: writes they sent as they checkpointed,
/// not yet answered, and those the workspace refused.
#[derive(Debug, Default)]
pub struct Settling {
    /// A write is still on its way.
    pub waiting: bool,
    /// Why the workspace refused what it refused.
    pub refused: Vec<String>,
}

impl Settling {
    /// Take one store's answer ([`crate::resource::ResourceStore::settling`]).
    pub fn take(&mut self, answer: Result<bool, String>) {
        match answer {
            Ok(waiting) => self.waiting |= waiting,
            Err(reason) => self.refused.push(reason),
        }
    }

    /// Take what another component waits on.
    pub fn join(&mut self, other: Self) {
        self.waiting |= other.waiting;
        self.refused.extend(other.refused);
    }
}

/// Whether a closing window may go as far as its records are concerned.
pub(crate) enum Keeping {
    /// The workspace has not answered yet.
    Waiting,
    /// The workspace keeps every change, or the window holds none it needs to keep.
    Kept,
    /// A change is not kept, for this reason, said to the person.
    NotKept(String),
}

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
    pub leaving: Leaving,
    #[cfg(feature = "model")]
    pub barrier: Barrier,
    /// This close checkpointed the window's components and asked for a barrier behind what they
    /// committed: the close goes once it is answered.
    pub checkpointed: bool,
    /// The dialog up says that changes are not kept ([`Keeping::NotKept`]).
    pub not_kept: bool,
    /// The person chose to close without the changes the workspace did not take.
    pub discard_unkept: bool,
    /// The close waits on writes its components sent themselves as they checkpointed
    /// ([`Settling`]).
    pub settling: bool,
    session: uuid::Uuid,
    choices: Vec<Choice>,
}
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
            leaving: Leaving::Staying,
            #[cfg(feature = "model")]
            barrier: Barrier::default(),
            checkpointed: false,
            not_kept: false,
            discard_unkept: false,
            settling: false,
            session: uuid::Uuid::new_v4(),
            choices: Vec::new(),
        }
    }
}
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
        platform: tilo_ui_base::platform::PlatformConventions,
    ) {
        self.session = uuid::Uuid::new_v4();
        self.choices = choices.to_vec();
        // A decision stops the close: going on saves again.
        self.ready = false;
        self.checkpointed = false;
        self.not_kept = false;
        self.settling = false;
        let resource = resource_for(self.session, choices);
        let mut content = Column::new()
            .with_intrinsic_height()
            .push(Box::new(Text::new(detail)));
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
        let mut dialog = ModalDialog::new(
            tilo_ui_base::i18n::I18nManager::lookup_plain(title),
            content,
        )
        .open(true)
        .dismiss_action(
            tilo_actions::ActionBinding::unit_for(&resource, Choice::KeepEditing.id())
                .expect("host close unit action"),
        );
        dialog.set_platform_conventions(platform);
        let allowed = resource
            .actions
            .iter()
            .map(|action| ActionAddress::new(resource.id.as_str(), &action.id))
            .collect();
        tilo_ui_base::widget::controls::refresh_control_action_availability(&mut dialog, &allowed);
        self.dialog = Some(Box::new(dialog));
    }
    pub fn validate(&self, command: &Command) -> bool {
        self.dialog.is_some()
            && command.session == self.session
            && self.choices.contains(&command.choice)
    }
    pub fn resources(&self) -> Vec<ResourceSpec> {
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
        self.checkpointed = false;
        self.not_kept = false;
        self.discard_unkept = false;
        self.settling = false;
        self.choices.clear();
    }

    /// Whether the close waits on something that answers on its own: a save it started, or the
    /// workspace's answer to the window's last changes.
    #[cfg(feature = "native")]
    pub fn waiting(&self) -> bool {
        // Only Tilo's model asks the workspace for a barrier.
        #[cfg(feature = "model")]
        let barrier = self.barrier.pending.is_some();
        #[cfg(not(feature = "model"))]
        let barrier = false;
        self.save_waiting || (self.checkpointed && (self.settling || barrier))
    }

    /// Retire the current decision before an explicit choice re-evaluates the close request.
    pub(crate) fn retire_presentation(&mut self) {
        self.dialog = None;
        self.choices.clear();
    }
}

fn resource_for(session: uuid::Uuid, choices: &[Choice]) -> ResourceSpec {
    let mut resource = ResourceSpec::new(
        format!("tilo.host.close:{session}"),
        "tilo.host.close",
        tilo_ui_base::i18n::I18nManager::lookup_plain("close-title"),
    );
    for choice in choices {
        if let Ok(action) = ActionSpec::operation(
            choice.id(),
            tilo_ui_base::i18n::I18nManager::lookup_plain(choice.label()),
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

#[cfg(feature = "model")]
pub(crate) fn retry_reports(
    reports: &mut [crate::application::ComponentCloseReport],
) -> Vec<String> {
    let mut errors = Vec::new();
    for report in reports {
        let _ = report.retry_resource();
        let _ = report.retry_view();
        errors.extend(report.resource_error.iter().cloned());
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
        {
            let now = crate::time::now();
            let close = now + crate::time::Duration::from_millis(100);
            assert_eq!(wake_deadline(None, true, now), Some(close));
            let sooner = now + crate::time::Duration::from_millis(10);
            assert_eq!(wake_deadline(Some(sooner), true, now), Some(sooner));
            assert_eq!(wake_deadline(None, false, now), None);
        }

        {
            let mut admission = Admission::default();
            admission.show(
                "close-title",
                "close-unsaved",
                &[Choice::Discard, Choice::KeepEditing],
                tilo_ui_base::platform::PlatformConventions::default(),
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
                tilo_ui_base::platform::PlatformConventions::default(),
            );
            assert!(!admission.validate(&stale));
            assert!(tilo_actions::api::resolve(&admission.resources(), &stale_address).is_none());
            admission.save_waiting = true;
            admission.save_local = Some(tilo_actions::InvocationId::new());
            admission.dismiss();
            assert!(!admission.save_waiting && admission.save_local.is_none());
            assert!(admission.resources().is_empty());
        }
    }
}
