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
//!
//! Where a room stands is read from the workspace tree, so it is looked at
//! by the room's waiter, never by the thread asking for the launch: launches
//! are asked for from the frame loop with the tree held (a saved window
//! reopened by `peer_windows::examine` holds it for writing), and a look
//! there would wait on the tree its own thread holds. Nothing here takes the
//! tree on the caller's thread.

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
    launches: Vec<(HeldLaunch, Start)>,
    /// Ends the room's waiter once nothing waits for the room any more.
    let_go: Arc<Notify>,
    /// What the room waits on, as its waiter last saw it; none until the
    /// waiter has looked and found the room not ready. Only then do the
    /// room's launches wait: before, they are about to start or wait.
    waits_on: Option<RoomWait>,
}

/// Say where rooms stand for this thread's session's launches from now on.
pub(crate) fn set_source(source: RoomStatusSource) {
    lock(&LaunchSession::current().rooms.0).source = Some(source);
}

/// Whether `package_id` has a launch waiting for its room: its window is
/// not late while it does.
pub(super) fn waits(session: &LaunchSession, package_id: &str) -> bool {
    lock(&session.rooms.0).held.values().any(|held| {
        held.waits_on.is_some()
            && held
                .launches
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

/// Start `launch` with `start` once its room is ready: now, when it names
/// no room or no room waits in this session; otherwise through its room's
/// waiter, which looks at the room off this thread and starts the launch
/// at once when the room is ready, or holds it while the room waits.
///
/// Takes no lock but this module's: the caller may hold the workspace tree.
///
/// # Errors
///
/// As `start`, for a launch started on this thread.
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
    hold(session, (source, room), (launch, start), None);
    Ok(())
}

/// Hold `launch` for `room` until the room's waiter starts or refuses it,
/// waking a waiter for the room when none waits yet: one that starts from
/// `seen`, the room as already looked at, or that looks first.
fn hold(
    session: &LaunchSession,
    (source, room): (RoomStatusSource, RoomId),
    (launch, start): (HeldLaunch, Start),
    seen: Option<(RoomStatus, RoomWait)>,
) {
    let mut rooms = lock(&session.rooms.0);
    let first = !rooms.held.contains_key(&room);
    let held = rooms.held.entry(room).or_insert_with(|| Held {
        launches: Vec::new(),
        let_go: Arc::new(Notify::new()),
        waits_on: seen.as_ref().map(|(_, wait)| *wait),
    });
    let let_go = Arc::clone(&held.let_go);
    if !held
        .launches
        .iter()
        .any(|(waiting, _)| same_launch(waiting, &launch))
    {
        held.launches.push((launch, start));
    }
    drop(rooms);
    if first {
        wait_for(session, source, room, (seen.map(|(seen, _)| seen), let_go));
    }
}

/// Wait for `room`, last seen as `seen` or not looked at yet, on a thread
/// of its own: woken by the room's changes only, it ends with the room's
/// launches.
fn wait_for(
    session: &LaunchSession,
    source: RoomStatusSource,
    room: RoomId,
    (seen, let_go): (Option<RoomStatus>, Arc<Notify>),
) {
    let waiter = session.clone();
    let spawned = std::thread::Builder::new()
        .name("tilo-room-launches".into())
        .spawn(move || {
            waiter.clone().adopt();
            waiter.wait_for_room(&source, room, seen, &let_go);
            drop((waiter, source));
            LaunchSession::disown();
        });
    if let Err(error) = spawned {
        // Nothing can wait for the room: its launches start as they would
        // have, and the room's apps say why they cannot open anything.
        tracing::warn!(%room, %error, "no thread to wait for a room; its launches start now");
        let held = lock(&session.rooms.0).held.remove(&room);
        for (launch, start) in held.into_iter().flat_map(|held| held.launches) {
            let _ = start(launch, session);
        }
    }
}

impl LaunchSession {
    fn wait_for_room(
        &self,
        source: &RoomStatusSource,
        room: RoomId,
        mut seen: Option<RoomStatus>,
        let_go: &Notify,
    ) {
        loop {
            let now = match &seen {
                // The first look, here rather than where the launch was
                // asked for: that thread may hold the tree.
                None => source.status(room),
                Some(seen) => {
                    let change = std::pin::pin!(source.change(room, seen.clone()));
                    let released = std::pin::pin!(let_go.notified());
                    match pollster::block_on(select(change, released)) {
                        Either::Left((now, _)) => now,
                        // Looked at below: nothing waits any more.
                        Either::Right(((), _)) => seen.clone(),
                    }
                }
            };
            // Looked at with no lock of this module held, which the tree's
            // writers may themselves wait on.
            let waits_on = matches!(now, RoomStatus::Waiting { .. }).then(|| {
                if source.placing(room) {
                    RoomWait::Placing
                } else {
                    RoomWait::Joining
                }
            });
            let mut rooms = lock(&self.rooms.0);
            let Some(held) = rooms.held.get_mut(&room) else {
                return;
            };
            if held.launches.is_empty() {
                rooms.held.remove(&room);
                return;
            }
            if let Some(wait) = waits_on {
                if held.waits_on.is_none() {
                    for (launch, _) in &held.launches {
                        tracing::info!(%room, package_id = %launch.package_id, "a launch waits for its room");
                    }
                }
                held.waits_on = Some(wait);
                seen = Some(now);
                continue;
            }
            let waited = held.waits_on.is_some();
            let launches = rooms
                .held
                .remove(&room)
                .map(|held| held.launches)
                .unwrap_or_default();
            drop(rooms);
            match now {
                // A room refused while its launches waited for it: they
                // will not start. One refused already when it was first
                // looked at starts its apps, which say why they cannot open
                // anything, as before rooms were waited for.
                RoomStatus::Refused(reason) if waited => {
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
                    if waited {
                        tracing::info!(%room, "launches waiting for a room start");
                    }
                    for (launch, start) in launches {
                        if waited {
                            // Its window is due as any start's is, from now.
                            super::hold_presentation_for(&launch.package_id, Duration::ZERO);
                        }
                        // A failure to start is reported through the session.
                        let _ = start(launch, self);
                    }
                }
            }
            return;
        }
    }
}

/// The device certificate this device holds `room` under, once the room's
/// enrollment has committed: at once when it has, otherwise when it commits.
/// An app opened as the session starts asks for its workspace while this
/// device's own host is still placing it, or while it is still being
/// joined; its opening waits for that enrollment rather than being refused
/// for coming first. Woken by the owner's publications
/// ([`tilo_identity_host::policy_change`], which every commit announces) and
/// by the room's status changing ([`RoomStatusSource::change`]), never by a
/// clock. Blocks: call it off the frame loop.
///
/// # Errors
///
/// No native owner is open; the room is fenced; its join was refused; or
/// it will never be enrolled here: it holds no committed enrollment and is
/// neither being placed, joined nor enrolled.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the room status source is read on every turn of the wait, up to its return"
)]
pub(super) fn committed_device(
    session: &LaunchSession,
    tree: &crate::shell::state::WorkspaceTreeHandle,
    room: RoomId,
) -> Result<tilo_user_identity::DeviceCert, super::LaunchError> {
    use super::LaunchError;
    let source = lock(&session.rooms.0)
        .source
        .clone()
        .unwrap_or_else(|| RoomStatusSource::new(tree.clone(), None));
    let published = Arc::new(Notify::new());
    let watch = tilo_identity_host::policy_change::Watch::new({
        let published = Arc::clone(&published);
        move || published.notify_one()
    });
    loop {
        // Armed, then looked: a commit after the look announces, and wakes
        // the wait below at once.
        watch.arm();
        // The status before the owner: a commit installs the room in the
        // owner before it takes the room's placement or join off the tree,
        // so a room no longer waiting and still not committed below will
        // not be.
        let status = source.status(room);
        let owner = tree.read().installed_native_owner().ok_or_else(|| {
            LaunchError::IdentityUnavailable {
                detail: crate::shell::i18n::t("launch-error-identity-unavailable"),
            }
        })?;
        if owner.rooms().contains(&room) {
            return owner
                .with_current(room, |view| Ok(view.device().clone()))
                .map_err(|detail| LaunchError::IdentityUnavailable { detail });
        }
        match &status {
            RoomStatus::Refused(reason) => {
                return Err(LaunchError::DeviceNotAdmitted {
                    detail: reason.clone(),
                });
            }
            RoomStatus::Ready if !owner.contains_room(room) => {
                return Err(LaunchError::DeviceNotAdmitted {
                    detail: crate::shell::i18n::t("launch-error-room-not-enrolled"),
                });
            }
            RoomStatus::Ready | RoomStatus::Waiting { .. } => {}
        }
        drop(owner);
        tracing::info!(%room, "an app opening waits for its room's enrollment to commit");
        let committed = std::pin::pin!(published.notified());
        let changed = std::pin::pin!(source.change(room, status));
        pollster::block_on(select(committed, changed));
    }
}

/// Whether no launch of this session waits for its room.
#[cfg(test)]
pub(super) fn nothing_waits(session: &LaunchSession) -> bool {
    lock(&session.rooms.0).held.is_empty()
}

/// What a launch held for its room waits on: the room being placed on
/// this device's host, or being joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoomWait {
    Placing,
    Joining,
}

/// What `package_id`'s window waits on, when its launch waits for its room.
/// The window's place says it as where the window comes from
/// (`PendingWindow::arrives_from`). Drawn on the frame loop, with the tree
/// held: it reads what the room's waiter saw, never the tree.
pub(crate) fn room_wait(package_id: &str) -> Option<RoomWait> {
    let session = LaunchSession::current();
    let rooms = lock(&session.rooms.0);
    rooms.held.values().find_map(|held| {
        held.launches
            .iter()
            .any(|(launch, _)| launch.package_id == package_id)
            .then_some(held.waits_on)
            .flatten()
    })
}

/// Visual review: a launch of `package_id` waits for `room` as a real one
/// does, against `tree`'s rooms, and starts nothing once it may. Looks at
/// the room here, so the frame drawn next shows the launch waiting.
pub fn hold_for_shots(
    tree: crate::shell::state::WorkspaceTreeHandle,
    package_id: &str,
    room: RoomId,
) {
    let started: Start = |_, _| Ok(());
    let source = RoomStatusSource::new(tree, None);
    set_source(source.clone());
    let launch = HeldLaunch {
        authority: None,
        #[cfg(target_os = "linux")]
        retained: None,
        package_id: package_id.into(),
        display_name: package_id.into(),
        command: super::LaunchCommand {
            program: std::path::PathBuf::new(),
            args: vec!["--workspace-id".into(), room.to_string()],
            env: Vec::new(),
            service_identity: None,
            portal_workspace_id: None,
            managed_agent_id: None,
            x11_workspace: None,
        },
    };
    let status = source.status(room);
    let seen = matches!(status, RoomStatus::Waiting { .. }).then(|| {
        let wait = if source.placing(room) {
            RoomWait::Placing
        } else {
            RoomWait::Joining
        };
        (status, wait)
    });
    hold(
        &LaunchSession::current(),
        (source, room),
        (launch, started),
        seen,
    );
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
