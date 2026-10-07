//! Delivering an admitted invocation to a widget without a host, for a test
//! that drives a widget directly: handing a control's invocation back, or an
//! action a published tree encoded. Apps answer their own operations as a
//! typed effect vocabulary; this reaches only the toolkit's widgets inside
//! them, the way the host does once it admitted an invocation.

use ui_core::automerge::{AutoCommit, ObjId};
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart};
use ui_core::widget::Widget;

use super::Effected;

/// The operation type a toolkit control's invocation travels under.
const CONTROL_INVOKE: &str = "tilo.ui.control.invoke";

/// Delivering an admitted invocation to a widget, as its host does. Answers
/// how the widget started it — done at once with its output, or pending to
/// finish later — or `None` when nothing in the widget answers it.
pub trait HostDelivery {
    /// Start the encoded operation (`type_id`, `payload`) of `kind`.
    ///
    /// # Errors
    ///
    /// Returns the widget's refusal or failure.
    fn start_encoded(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<Effected>, String>;

    /// Hand back the invocation of the toolkit control `control` (its id,
    /// as its `tilo.control:` resource names it).
    ///
    /// # Errors
    ///
    /// Returns the widget's refusal or failure.
    fn deliver_control(
        &mut self,
        kind: tilo_actions::OperationKind,
        control: &str,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<Effected>, String> {
        let payload = serde_json::to_vec(control).map_err(|error| error.to_string())?;
        self.start_encoded(kind, CONTROL_INVOKE, &payload, doc, path)
    }
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
        let started = self.start_operation(
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
