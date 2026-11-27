//! What a widget offers the session it collaborates through: its continuation state, and the
//! requests and events of the live activity it owns. Both are capabilities a widget provides
//! ([`Widget::provide`]); the toolkit base looks them up by type and knows nothing of the session.

use tilo_ui_base::widget::capability::capability_mut;
use tilo_ui_base::widget::{AutoCommit, Edits, ObjId, Widget};

/// Optional cross-device continuation capability for an app root, deliberately separate
/// from [`Widget`]: ordinary controls have no resumable app state, while roots that do expose it through
/// [`Widget::provide`] without weakening their UI integration.
pub trait Continuable {
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        path: &ObjId,
    ) -> Option<::collaboration::ContinuationMetadata>;

    /// Apply a continuation projection after launch. Implementations must not
    /// change focus or start media; malformed or unknown metadata is ignored.
    fn restore_continuation_metadata(
        &mut self,
        metadata: &::collaboration::ContinuationMetadata,
        state: &mut Edits<'_>,
        path: &ObjId,
    ) -> bool;
}

/// Optional session collaboration capability for widgets that own live broker-backed
/// activity.
pub trait Collaborative {
    fn take_collab_requests(&mut self) -> Vec<::collaboration::CollabRequest> {
        Vec::new()
    }

    fn handle_collab_event(&mut self, _event: &::collaboration::CollabEvent) {}
}

/// Drain collaboration capabilities throughout a retained widget tree.
pub fn take_collab_requests(widget: &mut dyn Widget) -> Vec<::collaboration::CollabRequest> {
    let Ok(_entry) = tilo_ui_base::widget::bound_nodes::enter_widget(widget) else {
        return Vec::new();
    };
    let mut requests = capability_mut::<dyn Collaborative>(&mut *widget)
        .map_or_else(Vec::new, Collaborative::take_collab_requests);
    for child in tilo_ui_base::widget::presence_topology::collaboration_children(widget) {
        requests.extend(take_collab_requests(child));
    }
    requests
}

/// Deliver one session event to each explicit collaboration capability once.
/// Bound callback context remains entered for the complete descendant walk.
pub fn handle_collab_event(widget: &mut dyn Widget, event: &::collaboration::CollabEvent) {
    let Ok(_entry) = tilo_ui_base::widget::bound_nodes::enter_widget(widget) else {
        return;
    };
    if let Some(collaborative) = capability_mut::<dyn Collaborative>(&mut *widget) {
        collaborative.handle_collab_event(event);
    }
    for child in tilo_ui_base::widget::presence_topology::collaboration_children(widget) {
        handle_collab_event(child, event);
    }
}
