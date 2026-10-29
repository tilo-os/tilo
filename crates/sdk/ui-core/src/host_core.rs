//! The app host core: what the native [`crate::app::App`] and the headless
//! [`crate::headless::FrameHarness`] do alike, written once. Each host keeps
//! its own fields and lends the parts this core reads through [`HostCore`].

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};

use crate::history_mode::embedded::{PendingCommand, PendingCommandOutcome};
use crate::history_mode::pending_controls::{ContinuationCommand, PreparedContinuation};
use crate::standalone_history::{HistoryError, HistoryReadContext, HistoryTarget};
use crate::standalone_history_host::{HostHistoryLayout, StandaloneHistoryHost};
use crate::widget::Widget;

/// A host's document and laid-out widget tree, borrowed for one read.
pub(crate) struct HostTree<'a> {
    pub(crate) state: &'a crate::State,
    pub(crate) root: &'a dyn Widget,
    pub(crate) bindings: &'a crate::bound_dispatch::BoundNodeMap,
    pub(crate) node_map: &'a HashMap<NodeId, *mut dyn Widget>,
    pub(crate) taffy: &'a TaffyTree<()>,
    pub(crate) records: Option<&'a crate::record::RecordSet>,
    pub(crate) permissions: &'a crate::permissions::PermissionContext,
    pub(crate) focused: Option<NodeId>,
    pub(crate) history_layout: Option<HostHistoryLayout>,
}

impl<'a> HostTree<'a> {
    pub(crate) fn history_context(&self) -> HistoryReadContext<'a> {
        HistoryReadContext::of(self.state, self.records, self.permissions)
    }

    /// The document History opens on: the focused component's, else the
    /// root's, else the first bound component laid out, else the host's own.
    pub(crate) fn capture_history_target(&self) -> Result<HistoryTarget, HistoryError> {
        crate::standalone_history_host::capture_history_target(
            self.focused
                .and_then(|node| self.bindings.binding_for(node))
                .or_else(|| self.root.callback_binding()),
            self.bindings,
            self.node_map.keys().copied(),
            self.records,
            self.state.document_identity(),
        )
    }

    /// Whether `node` belongs to the standalone History chrome rather than
    /// the app.
    pub(crate) fn is_history_node(&self, node: NodeId) -> bool {
        self.history_layout
            .is_some_and(|layout| layout.contains(self.taffy, node))
    }
}

pub(crate) trait HostCore {
    /// The host's document and laid-out tree.
    fn tree(&self) -> HostTree<'_>;

    /// The standalone History host beside the tree it presents.
    fn document_history_with_tree(&mut self) -> (&mut StandaloneHistoryHost, HostTree<'_>);

    /// The live app root, for History surfaces it embeds.
    fn host_root_mut(&mut self) -> &mut dyn Widget;

    /// Run a pending History control whose meaning
    /// [`Self::dispatch_prepared_history_control`] found unchanged.
    fn dispatch_current_history_control(
        &mut self,
        prepared: PreparedContinuation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>;

    fn with_history<R>(
        &mut self,
        visit: impl FnOnce(&mut StandaloneHistoryHost, &HistoryReadContext<'_>) -> R,
    ) -> R {
        let (host, tree) = self.document_history_with_tree();
        visit(host, &tree.history_context())
    }

    fn capture_history_target(&self) -> Result<HistoryTarget, HistoryError> {
        self.tree().capture_history_target()
    }

    fn is_history_node(&self, node: NodeId) -> bool {
        self.tree().is_history_node(node)
    }

    /// Note what History would open on and where focus is in the app, unless
    /// focus is in History itself.
    fn remember_history_content(&mut self) {
        let (host, tree) = self.document_history_with_tree();
        if tree.focused.is_some_and(|node| tree.is_history_node(node)) {
            return;
        }
        let target = tree.capture_history_target();
        let focus = crate::widget::layout::CompositionFocus::capture_live(tree.root);
        host.remember_content_target(target, focus);
    }

    /// Run a control History held back while it was open, refused when its
    /// meaning or confirmation policy changed since.
    fn dispatch_prepared_history_control(
        &mut self,
        prepared: PreparedContinuation,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let captured =
            prepared
                .witness
                .as_ref()
                .map_err(|reason| tilo_actions::DoorError::Unavailable {
                    code: "stale_pending_control".into(),
                    reason: reason.clone(),
                })?;
        let tree = self.tree();
        let current = crate::bound_dispatch::control_witness(
            tree.root,
            &tree.state.doc,
            &prepared.target,
            &prepared.invocation,
        )?;
        if &current != captured {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "stale_pending_control".into(),
                reason: "The action meaning or confirmation policy changed while History was open"
                    .into(),
            });
        }
        self.dispatch_current_history_control(prepared)
    }

    /// Serve what History surfaces embedded in the app asked of their
    /// held-back controls: review, confirm or cancel each.
    fn service_embedded_history_commands(&mut self, embedded: Vec<PendingCommand>) {
        use crate::history_mode::embedded as surfaces;
        for pending in embedded {
            let (token, confirmation) = match pending.command {
                ContinuationCommand::Review(token) => (token, None),
                ContinuationCommand::Confirm(token, value) => (token, Some(value)),
                ContinuationCommand::Cancel(token) => {
                    surfaces::cancel_pending_command(self.host_root_mut(), pending.surface, &token);
                    continue;
                }
            };
            let prepared = match confirmation.as_deref() {
                Some(value) => surfaces::prepare_pending_command_typed(
                    self.host_root_mut(),
                    pending.surface,
                    &token,
                    value,
                ),
                None => surfaces::prepare_pending_command(
                    self.host_root_mut(),
                    pending.surface,
                    &token,
                    false,
                ),
            };
            let Some(prepared) = prepared else {
                continue;
            };
            let confirmed = confirmation.is_some();
            let id = prepared.id;
            let outcome = match self.dispatch_prepared_history_control(prepared) {
                Err(tilo_actions::DoorError::ConfirmationRequired) if !confirmed => {
                    PendingCommandOutcome::NeedsConfirmation
                }
                Ok(_) => PendingCommandOutcome::Consumed,
                Err(error) => PendingCommandOutcome::Refused(error.to_string()),
            };
            surfaces::finish_pending_command(self.host_root_mut(), pending.surface, id, &outcome);
        }
    }
}
