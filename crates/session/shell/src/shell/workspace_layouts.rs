//! Each workspace's layout record, kept in step with the other devices of
//! its room over the room's record route.
//!
//! The record is the room's shared desktop ([`tilo_desktop_model::ownership::DESKTOP_APP`]),
//! the one the room's browsers and agents keep too, so every member edits
//! one layout over one route: signed by this device, end-to-end encrypted
//! where the room's records are, and retained by the room's hosts for
//! devices that join later. The session holds it as a host holds a record
//! without a window ([`ui::app::record_routes::RouteDocument`]).
//!
//! Each pump writes what changed in this device's layout into the record
//! before taking in what others wrote, so nothing only the tree held is
//! undone by the fold that follows; a change and the tree it was made in
//! are saved together ([`crate::shell::state::workspace_layout`]). Nothing
//! here runs on a timer: a room is looked at when its tree changed or its
//! route woke the loop, and what the route did not take yet goes out when
//! it takes writes again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_KIND, DESKTOP_RECORD};
use ui::app::record_routes::RouteDocument;

use crate::shell::state::{RoomId, WorkspaceTreeHandle};

/// Wakes the loop holding the records when a route sent something.
static WAKE: Mutex<Option<crate::session_runtime::Wake>> = Mutex::new(None);

/// Wake the loop that holds the session's layout records with `wake`
/// whenever one of their routes sent something.
pub(crate) fn wake_with(wake: crate::session_runtime::Wake) {
    *WAKE.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
}

fn wake() {
    let wake = WAKE.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(wake) = wake {
        wake();
    }
}

/// One room's record route.
struct Held {
    route: RouteDocument,
    /// Set when the route sent something since the last pump.
    woken: Arc<AtomicBool>,
    /// The tree's generation the last pump wrote from, and the app reports'
    /// revision it read.
    generation: Option<(u64, (u64, u64))>,
}

/// Opens a room's record route.
type Connect = dyn Fn(RoomId, Arc<AtomicBool>) -> RouteDocument;

/// The layout records the session holds, on the thread that holds them.
pub(crate) struct WorkspaceLayouts {
    held: HashMap<RoomId, Held>,
    connect: Box<Connect>,
}

thread_local! {
    static SESSION: RefCell<Option<WorkspaceLayouts>> = const { RefCell::new(None) };
}

/// Keep every workspace's layout record in step with its room: on the
/// session's loop, each time it wakes. Cheap when no tree changed and no
/// route sent anything.
pub(crate) fn pump(tree: &WorkspaceTreeHandle) {
    SESSION.with_borrow_mut(|session| {
        session
            .get_or_insert_with(WorkspaceLayouts::session)
            .pump(tree);
    });
}

impl WorkspaceLayouts {
    /// The session's: each room's route reaches the session's broker as the
    /// shared desktop's app, as an app the session shows in its own process
    /// does.
    fn session() -> Self {
        Self::with_connect(|room, woken| {
            RouteDocument::connect(
                crate::shell::in_process::session_connector(
                    launcher::services::collab_socket(),
                    launcher::services::identity_socket(),
                    DESKTOP_APP,
                    &room.to_string(),
                ),
                ui::collaboration::RouteSpec::record(DESKTOP_APP, room.as_uuid(), DESKTOP_RECORD),
                Some(DESKTOP_KIND.to_owned()),
                move || {
                    woken.store(true, Ordering::Release);
                    wake();
                },
            )
        })
    }

    pub(crate) fn with_connect(
        connect: impl Fn(RoomId, Arc<AtomicBool>) -> RouteDocument + 'static,
    ) -> Self {
        Self {
            held: HashMap::new(),
            connect: Box::new(connect),
        }
    }

    /// Write this device's layout of every room it holds into the room's
    /// record, take in what the room's other devices wrote, and fold the
    /// room's layout into the tree.
    pub(crate) fn pump(&mut self, tree: &WorkspaceTreeHandle) {
        let rooms = tree.read().workspace_layout_rooms();
        self.held.retain(|room, _| rooms.contains(room));
        for room in rooms {
            let held = self.held.entry(room).or_insert_with(|| {
                let woken = Arc::new(AtomicBool::new(true));
                Held {
                    route: (self.connect)(room, Arc::clone(&woken)),
                    woken,
                    generation: None,
                }
            });
            let woken = held.woken.swap(false, Ordering::AcqRel);
            // What this device's windows resume with changes with its apps'
            // reports as well as with the tree.
            let reports = crate::shell::continuation_registry::global_registry()
                .lock()
                .map(|registry| registry.revision())
                .unwrap_or_default();
            if !woken && held.generation == Some((tree.generation(), reports)) {
                continue;
            }
            held.generation = Some((pump_room(tree, room, &mut held.route), reports));
        }
    }
}

/// One room: write, take in, fold, and save what changed. Returns the
/// tree's generation once this write of it is released, so the next pump
/// looks again only when someone else wrote it.
fn pump_room(handle: &WorkspaceTreeHandle, room: RoomId, route: &mut RouteDocument) -> u64 {
    let mut tree = handle.write();
    // Read while this write holds the tree: its release is the next one.
    let after = handle.generation().wrapping_add(1);
    let captured = tree.capture_workspace_layout(room);
    let captured = tree.capture_continuations(
        room,
        crate::shell::doc_open::continuation_file_reference_for_surface,
    ) || captured;
    let judge = tree.layout_judge(room);
    let mut doc = tree.layout_docs.remove(&room).unwrap_or_default();
    let pumped = route.pump(&mut doc, &mut |doc, arrival| {
        judge.admit(doc, arrival.author, arrival.mutation)
    });
    tree.layout_docs.insert(room, doc);
    for reason in &pumped.refused {
        tracing::info!(%room, %reason, "a change of the workspace's layout was refused");
    }
    if pumped.merged {
        tree.fold_workspace_layout(room);
    }
    tree.apply_continuations(room);
    if captured || pumped.merged {
        tree.queue_save();
    }
    after
}
