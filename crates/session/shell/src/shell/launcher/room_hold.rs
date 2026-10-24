//! Launches into a room that is not ready for its apps yet.
//!
//! An app opens its documents in its room. A room still being joined, or
//! placed on a host elsewhere, has none yet, so an app started into it
//! would have nothing to open. Its launch waits here instead: one waiter
//! per room, woken only when the room's status changes
//! ([`RoomStatusSource::change`]), starts the room's launches once the room
//! is ready, or reports them failed with the reason once it will not be.
//! Meanwhile the window's place says why it waits ([`room_wait`]), its
//! start-up deadline does not run, and closing it lets the launch go
//! ([`cancel`]). A room this device's own host places is held before apps
//! start, so its launches never wait here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::{Either, select};
use tokio::sync::Notify;

use runtime_collab::RoomStatus;

use super::{HeldLaunch, LaunchSession, lock};
use crate::shell::broker_services::RoomStatusSource;
use crate::shell::state::RoomId;

/// The launches waiting for their rooms in one session, and where those
/// rooms stand.
#[derive(Default)]
pub(crate) struct State(Mutex<Rooms>);

#[derive(Default)]
struct Rooms {
    /// Where rooms stand; none before the session's services are up, and
    /// then no launch waits.
    source: Option<RoomStatusSource>,
    held: HashMap<RoomId, Held>,
}

/// How a launch starts once it may.
pub(super) type Start = fn(HeldLaunch, &LaunchSession) -> Result<(), String>;

struct Held {
    /// The room's status as its waiter last saw it: waiting, and why.
    status: RoomStatus,
    launches: Vec<(HeldLaunch, Start)>,
    /// Ends the room's waiter once nothing waits for the room any more.
    let_go: Arc<Notify>,
}

/// Say where rooms stand for this thread's session's launches from now on.
pub(crate) fn set_source(source: RoomStatusSource) {
    lock(&LaunchSession::current().rooms.0).source = Some(source);
}

/// Whether `package_id` has a launch waiting for its room: its window is
/// not late while it does.
pub(super) fn waits(session: &LaunchSession, package_id: &str) -> bool {
    lock(&session.rooms.0).held.values().any(|held| {
        held.launches
            .iter()
            .any(|(launch, _)| launch.package_id == package_id)
    })
}

/// The room `launch` opens its app in, when it names one.
fn room_of(launch: &HeldLaunch) -> Option<RoomId> {
    let args = &launch.command.args;
    let at = args.iter().position(|arg| arg == "--workspace-id")?;
    RoomId::parse(args.get(at.checked_add(1)?)?)
}

/// Whether `a` and `b` are the same launch asked again: the same app, the
/// same program and the same arguments.
fn same_launch(a: &HeldLaunch, b: &HeldLaunch) -> bool {
    a.package_id == b.package_id
        && a.command.program == b.command.program
        && a.command.args == b.command.args
}

/// Start `launch` with `start`, or hold it while its room waits.
///
/// # Errors
///
/// As `start`, for a launch started now.
pub(super) fn admit(
    session: &LaunchSession,
    launch: HeldLaunch,
    start: Start,
) -> Result<(), String> {
    let Some(room) = room_of(&launch) else {
        return start(launch, session);
    };
    let Some(source) = lock(&session.rooms.0).source.clone() else {
        return start(launch, session);
    };
    // Looked at with no lock of this module held, which the room's status
    // may itself wait on. A waiter that released the room meanwhile is
    // answered by the next one: it starts from this look, and a room ready
    // since then is no longer what it saw.
    let status = source.status(room);
    if !matches!(status, RoomStatus::Waiting { .. }) {
        return start(launch, session);
    }
    let mut rooms = lock(&session.rooms.0);
    let first = !rooms.held.contains_key(&room);
    let held = rooms.held.entry(room).or_insert_with(|| Held {
        status: status.clone(),
        launches: Vec::new(),
        let_go: Arc::new(Notify::new()),
    });
    let let_go = Arc::clone(&held.let_go);
    if !held
        .launches
        .iter()
        .any(|(waiting, _)| same_launch(waiting, &launch))
    {
        tracing::info!(%room, package_id = %launch.package_id, "a launch waits for its room");
        held.launches.push((launch, start));
    }
    drop(rooms);
    if first {
        wait_for(session.clone(), source, room, (status, let_go));
    }
    Ok(())
}

/// Wait for `room`, last seen as `seen`, on a thread of its own: woken by
/// the room's changes only, it ends with the room's launches.
fn wait_for(
    session: LaunchSession,
    source: RoomStatusSource,
    room: RoomId,
    (seen, let_go): (RoomStatus, Arc<Notify>),
) {
    let waiter = session.clone();
    let spawned = std::thread::Builder::new()
        .name("tilo-room-launches".into())
        .spawn(move || {
            waiter.clone().adopt();
            waiter.wait_for_room(&source, room, seen, &let_go);
        });
    if let Err(error) = spawned {
        // Nothing can wait for the room: its launches start as they would
        // have, and the room's apps say why they cannot open anything.
        tracing::warn!(%room, %error, "no thread to wait for a room; its launches start now");
        let held = lock(&session.rooms.0).held.remove(&room);
        for (launch, start) in held.into_iter().flat_map(|held| held.launches) {
            let _ = start(launch, &session);
        }
    }
}

impl LaunchSession {
    fn wait_for_room(
        &self,
        source: &RoomStatusSource,
        room: RoomId,
        mut seen: RoomStatus,
        let_go: &Notify,
    ) {
        loop {
            let change = std::pin::pin!(source.change(room, seen.clone()));
            let released = std::pin::pin!(let_go.notified());
            let now = match pollster::block_on(select(change, released)) {
                Either::Left((now, _)) => now,
                // Looked at below: nothing waits any more.
                Either::Right(((), _)) => seen.clone(),
            };
            let mut rooms = lock(&self.rooms.0);
            let Some(held) = rooms.held.get_mut(&room) else {
                return;
            };
            if held.launches.is_empty() {
                rooms.held.remove(&room);
                return;
            }
            if matches!(now, RoomStatus::Waiting { .. }) {
                held.status = now.clone();
                seen = now;
                continue;
            }
            let launches = rooms
                .held
                .remove(&room)
                .map(|held| held.launches)
                .unwrap_or_default();
            drop(rooms);
            match now {
                RoomStatus::Refused(reason) => {
                    tracing::info!(%room, %reason, "launches waiting for a room are refused");
                    for (launch, _) in launches {
                        self.startup.fail(
                            super::super::launch_readiness::command_token(&launch.command),
                            reason.clone(),
                        );
                        self.push(super::room_refused(launch, &reason));
                    }
                }
                _ => {
                    tracing::info!(%room, "launches waiting for a room start");
                    for (launch, start) in launches {
                        // Its window is due as any start's is, from now.
                        super::hold_presentation_for(&launch.package_id, Duration::ZERO);
                        // A failure to start is reported through the session.
                        let _ = start(launch, self);
                    }
                }
            }
            return;
        }
    }
}

/// Why `package_id`'s window waits for its room, when its launch does: the
/// room's reason, then what last held it up.
pub(crate) fn room_wait(package_id: &str) -> Option<String> {
    let session = LaunchSession::current();
    let status = lock(&session.rooms.0)
        .held
        .values()
        .find(|held| {
            held.launches
                .iter()
                .any(|(launch, _)| launch.package_id == package_id)
        })
        .map(|held| held.status.clone())?;
    match status {
        RoomStatus::Waiting { reason, detail } => Some(match detail {
            Some(detail) => format!("{reason}\n{detail}"),
            None => reason,
        }),
        _ => None,
    }
}

/// The person closed the window `package_id` was to open in `room`: its
/// launch, if it waits for the room, goes, and nothing starts later.
pub(crate) fn cancel(package_id: &str, room: RoomId) {
    let session = LaunchSession::current();
    let mut rooms = lock(&session.rooms.0);
    let Some(held) = rooms.held.get_mut(&room) else {
        return;
    };
    let before = held.launches.len();
    held.launches
        .retain(|(launch, _)| launch.package_id != package_id);
    if held.launches.len() == before {
        return;
    }
    tracing::info!(%room, %package_id, "a launch waiting for its room was let go");
    if held.launches.is_empty() {
        held.let_go.notify_one();
    }
    drop(rooms);
    super::clear_pending_placement(package_id);
}
