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
//! The instance is the app's own component, mounted in the session's process on the record its
//! windows open ([`record`]: the workspace's own record of Tasks) as a tab of the app would be ([`crate::shell::in_process::InProcessApps::mount`]), with no
//! window and never drawn. Its collaboration connection is recorded as the app's in that room,
//! and as one with no window ([`crate::shell::launcher::register_windowless_launch`]): the broker
//! attests it as it attests a launched app's, delivers the app's actions on its records to it,
//! and never an `open`. The overlay host mounts it on its loop, where the session's in-process
//! apps live, and runs it on every wake ([`ShellOverlayHost::run_windowless`]). It is kept for the
//! session: the app's actions in that room stay reachable without a window from then on.
//!
//! [`ShellOverlayHost::run_windowless`]: crate::shell::overlay_host::ShellOverlayHost

use std::collections::HashMap;
use std::sync::Mutex;

use tilo_lock::MutexRecover as _;

/// The instance every windowless app is mounted as: no tab of any workspace.
pub(crate) const INSTANCE: u64 = u64::MAX - 11;

/// The apps this session runs with no window: Tasks, whose tracker every ask and every run's
/// report changes.
#[must_use]
pub(crate) fn definition(app: &str) -> Option<&'static ui::application::ApplicationDefinition> {
    (app == tasks::APP_ID).then(tilo_app::framework::definition::<tasks::App>)
}

/// The record a windowless instance of `definition` opens: the one its windows open when
/// nothing names another. For an app that keeps the workspace's one record (Tasks' tracker)
/// that is the workspace's own record, which its windows, the room's readers (the broker's
/// tracker runs) and every device share; a record of its own beside it would hold what was
/// asked where nothing reads it.
#[must_use]
pub(crate) fn record(definition: &ui::application::ApplicationDefinition) -> ui::record::RecordId {
    if definition.keeps_room_record() {
        ui::record::RecordId::package_primary()
    } else {
        ui::record::RecordId::room_default()
    }
}

/// Where one app's windowless instance in one room stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Instance {
    /// Asked for; the overlay host mounts it on its next turn.
    Asked,
    /// Mounted and connected.
    Running,
    /// It could not be mounted: why. Said to the action that asked for it, then forgotten, so
    /// the next action tries again.
    Failed(String),
}

type Key = (String, String);

static INSTANCES: Mutex<Option<HashMap<Key, Instance>>> = Mutex::new(None);

/// Have `app` run with no window in `room` (the broker's ask): `Ok(true)` when it runs or will,
/// `Ok(false)` for an app this session runs no such instance of.
///
/// # Errors
/// Why its instance there could not be mounted, said once.
pub(crate) fn request(app: &str, room: &str) -> Result<bool, String> {
    if definition(app).is_none() {
        return Ok(false);
    }
    let key = (app.to_owned(), room.to_owned());
    let mut instances = INSTANCES.lock_recover();
    let instances = instances.get_or_insert_with(HashMap::new);
    match instances.get(&key) {
        Some(Instance::Failed(_)) => {
            let Some(Instance::Failed(why)) = instances.remove(&key) else {
                return Ok(true);
            };
            return Err(why);
        }
        Some(Instance::Running | Instance::Asked) => return Ok(true),
        None => {
            instances.insert(key, Instance::Asked);
        }
    }
    // The overlay host mounts it on its loop, which this wakes.
    crate::shell::wake_compositor();
    Ok(true)
}

/// The instances asked for and not mounted yet.
pub(crate) fn asked() -> Vec<(String, String)> {
    INSTANCES
        .lock_recover()
        .as_ref()
        .map(|instances| {
            instances
                .iter()
                .filter(|(_, instance)| **instance == Instance::Asked)
                .map(|(key, _)| key.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Where `app`'s instance in `room` stands now.
pub(crate) fn settle(app: &str, room: &str, instance: Instance) {
    INSTANCES
        .lock_recover()
        .get_or_insert_with(HashMap::new)
        .insert((app.to_owned(), room.to_owned()), instance);
}

/// Where `app`'s instance in `room` stands, if it was asked for.
#[cfg(test)]
pub(crate) fn state(app: &str, room: &str) -> Option<Instance> {
    INSTANCES
        .lock_recover()
        .as_ref()?
        .get(&(app.to_owned(), room.to_owned()))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tasks_runs_without_a_window_and_a_failure_is_said_once() {
        let room = uuid::Uuid::new_v4().to_string();
        assert_eq!(request("so.tilo.notes", &room), Ok(false));
        assert_eq!(request(tasks::APP_ID, &room), Ok(true));
        assert_eq!(state(tasks::APP_ID, &room), Some(Instance::Asked));
        assert!(asked().contains(&(tasks::APP_ID.to_owned(), room.clone())));
        // Asked again while it is mounted, nothing more is asked.
        assert_eq!(request(tasks::APP_ID, &room), Ok(true));
        settle(tasks::APP_ID, &room, Instance::Failed("no broker".into()));
        assert_eq!(request(tasks::APP_ID, &room), Err("no broker".into()));
        // Said once: the next action tries again.
        assert_eq!(request(tasks::APP_ID, &room), Ok(true));
        assert_eq!(state(tasks::APP_ID, &room), Some(Instance::Asked));
    }

    /// Tasks without a window keeps the tracker its windows and the broker's
    /// tracker runs read: the workspace's own record, not one of its own.
    #[test]
    fn tasks_without_a_window_opens_the_record_its_windows_open() {
        let definition = definition(tasks::APP_ID).unwrap();
        assert!(definition.keeps_room_record());
        assert!(record(definition).is_package_primary());
    }
}
