//! Apps the session runs with no window, to take actions on their records.
//!
//! Asking an agent makes an issue in Tasks, and the agent tells that issue how its run goes: each
//! an action of Tasks' own Rust on the room's tracker, and none of them shows anything. When no
//! Tasks window is open in the room, the broker used to launch Tasks for them, so a window came
//! up for every ask (also one that then failed), over where the person asked. Now the broker asks
//! the host for an instance of the app with no window instead
//! ([`tilo_action_broker::services::LauncherServices::windowless`]), for every action but `open`:
//! opening something is the one action that shows it, so `open` still launches a window.
//!
//! The instance is the app's own component, mounted in the session's process on the room's record
//! as a tab of the app would be ([`crate::shell::in_process::InProcessApps::mount`]), with no
//! window and never drawn. Its collaboration connection is recorded as the app's in that room,
//! and as one with no window ([`crate::shell::launcher::register_windowless_launch`]): the broker
//! attests it as it attests a launched app's, delivers the app's actions on its records to it,
//! and never an `open`. The overlay host mounts it on its loop, where the session's in-process
//! apps live, and runs it on every wake ([`ShellOverlayHost::run_windowless`]). It is kept for the
//! session: the app's actions in that room stay reachable without a window from then on.
//!
//! [`ShellOverlayHost::run_windowless`]: crate::shell::overlay_host::ShellOverlayHost

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Mutex;

use tilo_action_broker::services::WindowlessStart;
use tilo_lock::MutexRecover as _;

/// The instance every windowless app is mounted as: no tab of any workspace.
pub(crate) const INSTANCE: u64 = u64::MAX - 11;

/// The apps this session runs with no window: Tasks, whose tracker every ask and every run's
/// report changes.
#[must_use]
pub(crate) fn definition(app: &str) -> Option<&'static ui::application::ApplicationDefinition> {
    (app == tasks::APP_ID).then(tilo_app::framework::definition::<tasks::App>)
}

/// How the overlay host's mount of one app's windowless instance in one room ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Instance {
    /// Mounted and connected.
    Running,
    /// It could not be mounted: why. Said to every action waiting on its start, then
    /// forgotten, so the next action tries again.
    Failed(String),
}

/// Where one app's windowless instance in one room stands.
enum Slot {
    /// Asked for; the overlay host mounts it on its next turn. The starts of the actions
    /// waiting on it, each told how the mount ends.
    Asked(Vec<tokio::sync::oneshot::Sender<Result<(), String>>>),
    /// Mounted and connected.
    Running,
}

type Key = (String, String);

static INSTANCES: Mutex<Option<HashMap<Key, Slot>>> = Mutex::new(None);

/// Have `app` run with no window in `room` (the broker's ask): how its start ends, or `None`
/// for an app this session runs no such instance of.
pub(crate) fn request(app: &str, room: &str) -> Option<WindowlessStart> {
    definition(app)?;
    let (tell, start) = tokio::sync::oneshot::channel();
    let key = (app.to_owned(), room.to_owned());
    let mut instances = INSTANCES.lock_recover();
    match instances.get_or_insert_with(HashMap::new).entry(key) {
        Entry::Occupied(mut slot) => match slot.get_mut() {
            Slot::Running => tell_start(tell, Ok(())),
            Slot::Asked(waiting) => waiting.push(tell),
        },
        Entry::Vacant(slot) => {
            slot.insert(Slot::Asked(vec![tell]));
            drop(instances);
            // The overlay host mounts it on its loop, which this wakes.
            crate::shell::wake_compositor();
        }
    }
    Some(start)
}

/// Tell one waiting action how the start ended. An action that stopped waiting is told nothing.
fn tell_start(tell: tokio::sync::oneshot::Sender<Result<(), String>>, ended: Result<(), String>) {
    if tell.send(ended).is_err() {
        tracing::debug!("an action stopped waiting for an app's start without a window");
    }
}

/// The instances asked for and not mounted yet.
pub(crate) fn asked() -> Vec<(String, String)> {
    INSTANCES
        .lock_recover()
        .as_ref()
        .map(|instances| {
            instances
                .iter()
                .filter(|(_, slot)| matches!(slot, Slot::Asked(_)))
                .map(|(key, _)| key.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// How the mount of `app`'s instance in `room` ended: every action waiting on its start is
/// told; a failed one is forgotten, so the next action asks again.
pub(crate) fn settle(app: &str, room: &str, instance: Instance) {
    let key = (app.to_owned(), room.to_owned());
    let waiting = {
        let mut instances = INSTANCES.lock_recover();
        let instances = instances.get_or_insert_with(HashMap::new);
        let previous = match &instance {
            Instance::Running => instances.insert(key, Slot::Running),
            Instance::Failed(_) => instances.remove(&key),
        };
        match previous {
            Some(Slot::Asked(waiting)) => waiting,
            Some(Slot::Running) | None => Vec::new(),
        }
    };
    let ended = match instance {
        Instance::Running => Ok(()),
        Instance::Failed(why) => Err(why),
    };
    for tell in waiting {
        tell_start(tell, ended.clone());
    }
}

/// Whether `app`'s instance in `room` was asked for and not mounted yet (`Some(false)`), runs
/// (`Some(true)`), or stands nowhere.
#[cfg(test)]
pub(crate) fn state(app: &str, room: &str) -> Option<bool> {
    INSTANCES
        .lock_recover()
        .as_ref()?
        .get(&(app.to_owned(), room.to_owned()))
        .map(|slot| matches!(slot, Slot::Running))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tasks_runs_without_a_window_and_every_waiting_action_hears_how_it_started() {
        let room = uuid::Uuid::new_v4().to_string();
        assert!(request("so.tilo.notes", &room).is_none());
        let mut first = request(tasks::APP_ID, &room).expect("Tasks runs without a window");
        assert_eq!(state(tasks::APP_ID, &room), Some(false));
        assert!(asked().contains(&(tasks::APP_ID.to_owned(), room.clone())));
        // Asked again while it is being mounted, it waits on the same mount.
        let mut second = request(tasks::APP_ID, &room).expect("Tasks runs without a window");
        assert!(
            first.try_recv().is_err(),
            "nothing said before the mount ends"
        );
        settle(tasks::APP_ID, &room, Instance::Failed("no broker".into()));
        assert_eq!(first.try_recv(), Ok(Err("no broker".into())));
        assert_eq!(second.try_recv(), Ok(Err("no broker".into())));
        // Forgotten: the next action asks again, and hears it runs.
        let mut third = request(tasks::APP_ID, &room).expect("Tasks runs without a window");
        assert_eq!(state(tasks::APP_ID, &room), Some(false));
        settle(tasks::APP_ID, &room, Instance::Running);
        assert_eq!(third.try_recv(), Ok(Ok(())));
        let mut running = request(tasks::APP_ID, &room).expect("Tasks runs without a window");
        assert_eq!(
            running.try_recv(),
            Ok(Ok(())),
            "a running instance is said at once"
        );
    }
}
