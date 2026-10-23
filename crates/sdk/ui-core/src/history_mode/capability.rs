//! How a widget takes part in document History: the capabilities an app root
//! or a document owner provides, and the walks the runner uses to reach them.

use automerge::{AutoCommit, ObjId};

use crate::application_view::ApplicationViewState;
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

    /// Pure policy for physical text emitted by this owner. The host alone
    /// decides whether the exact author, instance and frontier may continue.
    /// Programmatic and agent document actions never consult this policy.
    fn physical_text_policy(
        &self,
        _previous: Option<&tilo_actions::EncodedAction>,
        _next: &tilo_actions::EncodedAction,
    ) -> (bool, bool) {
        (false, false)
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
pub fn history_registration(
    widget: &(impl Widget + ?Sized),
) -> Result<Option<super::HistoryRegistration>, super::HistoryRegistrationError> {
    capability::<dyn DocumentHistory>(widget)
        .map_or(Ok(None), DocumentHistory::history_registration)
}

/// `widget`'s own restore veto for `scope`.
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

pub fn visit_framework_history(
    widget: &dyn Widget,
    visitor: &mut dyn FnMut(&super::FrameworkHistorySurface, &dyn Widget),
) {
    if let Some(topology) = capability::<dyn FrameworkHistoryTopology>(widget) {
        topology.visit_framework_history(visitor);
        return;
    }
    widget.for_each_child(&mut |child| visit_framework_history(child, visitor));
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

/// A live widget that presents its document's changes in the shared History
/// view: the app says how each of its changes shows
/// ([`crate::widget::HistoryEntryPresenter`]) and takes up the ones a person
/// chooses to open. History stays the one service; this is only the app's
/// word on what its changes look like.
pub trait PresentsHistoryEntries {
    /// How this app's changes show in History.
    fn history_entry_presenter(
        &self,
    ) -> Option<std::rc::Rc<dyn crate::widget::HistoryEntryPresenter>>;

    /// A person chose `entry` in History and its face opens it
    /// ([`crate::widget::EntryFace::opens`]): History has closed, the app is
    /// live again, and takes the change up (a calculation goes back into the
    /// calculator). Returns whether it did.
    fn open_history_entry(&mut self, entry: &crate::widget::HistoryEntry) -> bool {
        let _ = entry;
        false
    }
}

/// The presenter of the first widget in `widget`'s tree that presents its
/// History entries.
pub fn history_entry_presenter(
    widget: &dyn Widget,
) -> Option<std::rc::Rc<dyn crate::widget::HistoryEntryPresenter>> {
    if let Some(presents) = capability::<dyn PresentsHistoryEntries>(widget) {
        return presents.history_entry_presenter();
    }
    widget
        .children()
        .into_iter()
        .find_map(history_entry_presenter)
}

/// The presentation of the first widget in `widget`'s tree that keeps one
/// ([`ApplicationViewState`]): how the window shows its record (a mode, the
/// open item), so History opens a past version as the window shows it.
pub fn live_presentation(
    widget: &dyn Widget,
) -> Option<crate::application_view::ApplicationLocalView> {
    if let Some(view) = capability::<dyn ApplicationViewState>(widget) {
        return view.capture_application_view().ok().flatten();
    }
    widget.children().into_iter().find_map(live_presentation)
}

/// Show `local`, a live window's presentation, in the first widget in
/// `widget`'s tree that keeps one, over the document `doc` it draws.
/// Returns whether one took it.
pub fn adopt_presentation(
    widget: &mut dyn Widget,
    local: &crate::application_view::ApplicationLocalView,
    doc: &AutoCommit,
) -> bool {
    if let Some(view) = capability_mut::<dyn ApplicationViewState>(&mut *widget) {
        return view.restore_application_view(local, doc).is_ok()
            && view.finish_application_view_restore().is_ok();
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| adopt_presentation(child, local, doc))
}

/// Hand `entry` to the first widget in `widget`'s tree that presents History
/// entries. Returns whether it took it up.
pub fn open_history_entry(widget: &mut dyn Widget, entry: &crate::widget::HistoryEntry) -> bool {
    if let Some(presents) = capability_mut::<dyn PresentsHistoryEntries>(&mut *widget) {
        return presents.open_history_entry(entry);
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| open_history_entry(child, entry))
}
