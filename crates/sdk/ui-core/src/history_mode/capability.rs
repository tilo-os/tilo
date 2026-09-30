//! How a widget takes part in document History: the capabilities an app root
//! or a document owner provides, and the walks the runner uses to reach them.

use automerge::{AutoCommit, ObjId};

use crate::widget::Widget;
use crate::widget::capability::{capability, capability_mut};

/// A widget that owns a document's History: its registration, restore
/// readiness and presentation.
pub trait DocumentHistory {
    /// Explicit root-owned History custody, including selected external
    /// resources. Runners consume this bundle; preview renderers never
    /// receive the root.
    ///
    /// # Errors
    /// Returns a registration error when the root cannot provide valid
    /// History custody.
    fn history_registration(
        &self,
    ) -> Result<Option<super::HistoryRegistration>, super::HistoryRegistrationError> {
        Ok(None)
    }

    /// Additional read-only veto for uncommitted app-local drafts in this
    /// scope. This does not confer authority or recursively consult
    /// independent owners.
    ///
    /// # Errors
    /// Returns a reason when this exact root is not ready to restore the scope.
    fn history_restore_readiness(&self, _scope: &str) -> Result<(), String> {
        Ok(())
    }

    /// Presentation declaration for a legacy tile's eventual checked record.
    /// The `TileHost` resolves custody after binding; this grants no authority.
    fn document_history_definition(&self) -> Option<&'static super::HistoryDefinition> {
        None
    }

    /// Whether this widget presents its document's History itself, so the
    /// host must not add the shared standalone History control beside it:
    /// one document gets one History surface.
    fn provides_document_history(&self) -> bool {
        false
    }

    /// A successful, locally applied resource Document action on this exact
    /// owner. Immutable document access permits cache reconciliation only.
    /// Not called for submissions, failed execution, replay, remote merge,
    /// or History restore.
    fn document_action_applied(
        &mut self,
        _action: &tilo_actions::EncodedAction,
        _before: &[automerge::ChangeHash],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) {
    }
}

/// Framework History lifecycle traversal for owners whose History surfaces
/// differ from their visible presentation children.
pub trait FrameworkHistoryTopology {
    fn visit_framework_history(
        &self,
        visitor: &mut dyn FnMut(&super::FrameworkHistorySurface, &dyn Widget),
    );

    fn visit_framework_history_mut(
        &mut self,
        visitor: &mut dyn FnMut(&mut super::FrameworkHistorySurface, &mut dyn Widget),
    );
}

/// `widget`'s History registration (none unless it owns History).
///
/// # Errors
/// Returns the widget's registration error.
pub fn history_registration(
    widget: &(impl Widget + ?Sized),
) -> Result<Option<super::HistoryRegistration>, super::HistoryRegistrationError> {
    capability::<dyn DocumentHistory>(widget)
        .map_or(Ok(None), DocumentHistory::history_registration)
}

/// `widget`'s own restore veto for `scope`.
///
/// # Errors
/// Returns the widget's reason it is not ready.
pub fn history_restore_readiness(
    widget: &(impl Widget + ?Sized),
    scope: &str,
) -> Result<(), String> {
    capability::<dyn DocumentHistory>(widget)
        .map_or(Ok(()), |history| history.history_restore_readiness(scope))
}

/// `widget`'s History presentation declaration.
pub fn document_history_definition(
    widget: &(impl Widget + ?Sized),
) -> Option<&'static super::HistoryDefinition> {
    capability::<dyn DocumentHistory>(widget)?.document_history_definition()
}

/// Whether `widget` or a descendant presents its document's History itself.
pub fn provides_document_history(widget: &dyn Widget) -> bool {
    capability::<dyn DocumentHistory>(widget)
        .is_some_and(DocumentHistory::provides_document_history)
        || widget.children().into_iter().any(provides_document_history)
}

/// Tell `widget` a Document action it owns was applied.
pub fn document_action_applied(
    widget: &mut (impl Widget + ?Sized),
    action: &tilo_actions::EncodedAction,
    before: &[automerge::ChangeHash],
    doc: &AutoCommit,
    path: &ObjId,
) {
    if let Some(history) = capability_mut::<dyn DocumentHistory>(widget) {
        history.document_action_applied(action, before, doc, path);
    }
}

/// Visit every framework History surface under `widget`.
pub fn visit_framework_history(
    widget: &dyn Widget,
    visitor: &mut dyn FnMut(&super::FrameworkHistorySurface, &dyn Widget),
) {
    if let Some(topology) = capability::<dyn FrameworkHistoryTopology>(widget) {
        topology.visit_framework_history(visitor);
        return;
    }
    for child in widget.children() {
        visit_framework_history(child, visitor);
    }
}

/// Mutable [`visit_framework_history`].
pub fn visit_framework_history_mut(
    widget: &mut dyn Widget,
    visitor: &mut dyn FnMut(&mut super::FrameworkHistorySurface, &mut dyn Widget),
) {
    if let Some(topology) = capability_mut::<dyn FrameworkHistoryTopology>(&mut *widget) {
        topology.visit_framework_history_mut(visitor);
        return;
    }
    for child in widget.children_mut() {
        visit_framework_history_mut(child, visitor);
    }
}
