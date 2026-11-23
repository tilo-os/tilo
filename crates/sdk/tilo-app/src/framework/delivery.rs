//! Delivering an admitted invocation to a widget without a host, for a test
//! that drives a widget directly: an action a published tree encoded, or a
//! press on a control. Apps answer their own operations as a
//! typed effect vocabulary; this reaches only the toolkit's widgets inside
//! them, the way the host does once it admitted an invocation.

use ui_core::automerge::{AutoCommit, ObjId};
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart};
use ui_core::widget::Widget;

use super::Effected;

/// Delivering an admitted invocation to a widget, as its host does. Answers
/// how the widget started it — done at once with its output, or pending to
/// finish later — or `None` when nothing in the widget answers it.
pub trait HostDelivery {
    /// Start the encoded operation (`type_id`, `payload`) of `kind`.
    fn start_encoded(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<Effected>, String>;
}

/// Press the toolkit control `control` (its test id) under `root` the way an
/// assistive technology does, through its accessibility activation. Whether a
/// control answered it.
pub fn press_control<W: Widget + ?Sized>(
    root: &mut W,
    control: &str,
    doc: &mut AutoCommit,
    path: &ObjId,
) -> bool {
    if root.test_id() == Some(control) {
        return root.perform_action(
            &ui_core::a11y::Action::Activate,
            &[],
            &mut ui_core::widget::Edits::lend(doc),
            path,
        );
    }
    root.children_mut()
        .into_iter()
        .any(|child| press_control(child, control, doc, path))
}

impl<W: Widget + ?Sized> HostDelivery for W {
    fn start_encoded(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<Effected>, String> {
        let started = ui_core::host_operation::start_operation(
            self,
            &ResourceOperation::local(tilo_actions::InvocationId::new(), kind, type_id, payload),
            &mut ui_core::widget::Edits::lend(doc),
            path,
        )?;
        Ok(started.map(|started| match started {
            ResourceOperationStart::Completed(output) => Effected::Done(output),
            ResourceOperationStart::Accepted => Effected::Pending,
        }))
    }
}
