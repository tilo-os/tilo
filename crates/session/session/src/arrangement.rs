//! The arrangement as this session runs it: the role itself is
//! [`tilo_session_core::arrangement`]'s, the same the page runs, and this is the native host's
//! side of it — the desk document the session keeps, the compositor's gesture and typing, what an
//! older build kept beside the desk, and the published door.

#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page publishes its own arrangement through this role next"
    )
)]

use std::rc::Rc;

#[cfg(target_os = "linux")]
use tilo_desktop_surface::DesktopHandle;
use tilo_session_core::arrangement::Door;
use tilo_session_core::door::{Look, Published};

#[cfg(target_os = "linux")]
use crate::activity::{Activity, now_seconds};

pub use tilo_session_core::arrangement::{
    Answer, AnswerRef, Arrangement, ArrangementAction, ArrangementQueryReading,
    ArrangementRefReading, Ask, AskOrigin, Busy, Catalog, Find, Found, Item, OPERATION, PLACE_KIND,
    PlaceReading, QUERY_KIND, RECORD_RESOURCE, RESOURCE, ROW_KIND, Reading, RowReading, STRIP_KIND,
    StripReading, Surface, Target, WorkspaceReading, found, place_kind_resource, placed, resources,
    row_kind_resource, strip_kind_resource, tab_app,
};

/// `person` 's arrangement of `handle` 's desk, whose changes `activity` keeps: a device that kept
/// its arrangement beside the desk before this build has what it holds opened on the desk, once
/// ([`crate::desk_migration::arrangement`]).
#[cfg(target_os = "linux")]
#[must_use]
pub(crate) fn open(
    person: &str,
    handle: DesktopHandle,
    activity: &Activity,
    state: Option<&std::path::Path>,
) -> Arrangement {
    if let Some(state) = state
        && let Some(adopted) =
            crate::desk_migration::arrangement::adopt(state, &handle.read(Clone::clone))
    {
        handle.replace_tree(adopted.desk.clone());
        activity.record_desk_now(&adopted.desk, now_seconds(), &adopted.shows);
    }
    let names = Rc::new(crate::app_names::window_name);
    Arrangement::new(person, handle, activity.desk(), names)
}

/// What this session's compositor knows of the person's gesture and typing, as the arrangement's
/// one fixed rule reads it.
#[cfg(target_os = "linux")]
pub(crate) const fn busy(underway: tilo_compositor::Underway) -> Busy {
    Busy {
        gesture: underway.gesture,
        typing: underway.typing,
    }
}

/// The arrangement's door as this session publishes it: the core's, with the host's feed of what
/// the person is in the middle of.
pub(crate) struct ArrangementDoor(Door);

impl ArrangementDoor {
    /// The door over `arrangement` , whose changes wait while `busy` says the person is arranging
    /// or typing.
    pub(crate) fn new(
        arrangement: Rc<std::cell::RefCell<Arrangement>>,
        busy: Rc<dyn Fn() -> Busy>,
        find: Find,
        ask: Ask,
        ask_origin: AskOrigin,
    ) -> Self {
        Self(Door::new(arrangement, busy, find, ask, ask_origin))
    }
}

impl Published for ArrangementDoor {
    fn resources(&mut self) -> Vec<tilo_actions::ResourceSpec> {
        self.0.resources()
    }

    fn revision(&self) -> Option<u64> {
        Some(self.0.revision())
    }

    fn invoke(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        self.0.invoke(address, arguments)
    }

    fn invoke_as(
        &mut self,
        actor: &str,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        self.0.invoke_as(actor, address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.0.accessible_labels()
    }
}
