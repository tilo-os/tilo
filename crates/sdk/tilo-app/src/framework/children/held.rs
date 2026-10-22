//! One child as a record a part of the view edits like the app's own
//! ([`ViewCx::record`](crate::framework::ViewCx::record)).
//!
//! The part is built as a view of the child: its controls bind the child's
//! fields and verbs, which are carried out on the child's record through the
//! container's door, checked against the participant's roles in the room as
//! the app's own actions are and written into the child's history. A view
//! shows a child beside its parent, or instead of it (an issue kept in a
//! record of its own on the same page as every other issue), with the code
//! that shows the parent's.

use std::rc::Rc;

use tilo_actions::ActionError;
use tilo_resource::Id;

use super::{ChildRecord, Children, child_resource_id, route_child};
use crate::framework::record::{Held, RecordDoor, sealed};

/// Child `id` of a container, as a record a view part edits
/// ([`Children::record`]).
pub struct ChildOf<C: ChildRecord> {
    pub(super) children: Children<C>,
    pub(super) id: Id,
}

impl<C: ChildRecord> sealed::Hold<C> for ChildOf<C> {
    fn held(&self, _: sealed::Storage<'_>) -> Option<Rc<Held<C>>> {
        let state = Rc::clone(&self.children.state);
        state.hold(&self.id).ok()?;
        let live = state.held.borrow().get(&self.id)?.clone();
        let label = tilo_resource::Model::label(&*live.get()).unwrap_or_else(|| C::LABEL.into());
        let (synced, applying) = (Rc::downgrade(&state), Rc::downgrade(&state));
        let (id, door) = (self.id.clone(), state.id().to_owned());
        let routed = self.id.clone();
        let held = Held::new(
            child_resource_id::<C>(&self.id),
            label,
            true,
            live,
            move |_| {
                if let Some(state) = synced.upgrade() {
                    state.refresh();
                }
            },
            move |action, invoker| {
                applying
                    .upgrade()
                    .ok_or_else(|| ActionError::Refused("the container is closed".into()))?
                    .apply_to(&id, action, invoker)
                    .map(drop)
            },
        )
        .in_room(move |published| route_child(published, &door, &routed));
        Some(Rc::new(held))
    }

    fn door(&self, storage: sealed::Storage<'_>) -> Option<sealed::Door> {
        let held = self.held(storage)?;
        Some(sealed::Door(held as Rc<dyn RecordDoor>))
    }
}
