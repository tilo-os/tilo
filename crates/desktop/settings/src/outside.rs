//! The settings as a process outside the session reaches them: the lock screen, the panels, the
//! launcher, the device daemon, the model gateway, a service the session pipes.
//!
//! The session leaves what it reads in a file of the profile each time it changes ([`publish`]),
//! with the device it reads them on; [`Outside`] reads that file, and a change made here is the
//! setter of its setting (`settings/appearance#set-mode`), which the session runs through its
//! control socket as it runs `tilo do`. A sandboxed app reaches neither: the control socket turns
//! it away, and its settings are its channel's.
//!
//! A device whose session is not running (a server, a device not signed in yet) sets a setting
//! here ([`set_on_this_device`]): it waits on the device for the session, which writes it once it
//! holds the workspace, and what the device's processes read has it at once.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_lock::MutexRecover as _;

use crate::{ChangeOrigin, ChangeSignal, SettingsSource, placement};

/// The file below the profile the session leaves its settings in.
pub const FILE: &str = "settings-published.json";

/// What the session leaves.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Left {
    /// The device the session reads them on, by its key in lowercase hex: where a device section
    /// is kept.
    device: String,
    settings: Map<String, Value>,
}

/// The file below the profile that keeps what was set on the device while no session held the
/// settings' workspace ([`crate::workspace::waiting`]).
pub const WAITING: &str = "settings-waiting.json";

/// Where the session leaves its settings for this profile.
#[must_use]
pub fn path() -> Option<PathBuf> {
    crate::config_path(Path::new(FILE))
}

/// Leave `settings`, as the session reads them on `device`, at `path`, unless they are there
/// already.
///
/// # Errors
/// The file could not be written.
pub fn publish_at(path: &Path, device: &str, settings: &Map<String, Value>) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&Left {
        device: device.to_owned(),
        settings: settings.clone(),
    })
    .map_err(std::io::Error::other)?;
    if std::fs::read(path).is_ok_and(|held| held == bytes) {
        return Ok(());
    }
    tilo_files::native::atomic::write_creating_parent(path, &bytes)
}

/// [`publish_at`] this profile's file.
///
/// # Errors
/// As [`publish_at`].
pub fn publish(device: &str, settings: &Map<String, Value>) -> std::io::Result<()> {
    path().map_or(Ok(()), |path| publish_at(&path, device, settings))
}

/// The device this profile's session reads its settings on, by its key in lowercase hex, as it
/// last left them: where a device section's resource is. `None` before it left any.
#[must_use]
pub fn device() -> Option<String> {
    let left: Left = serde_json::from_slice(&std::fs::read(path()?).ok()?).ok()?;
    Some(left.device).filter(|device| !device.is_empty())
}

/// Run the setter `action` of `resource` with `value` on a device whose session is not running:
/// kept for the session, which writes it once it holds the workspace, and left at once for what
/// the device's processes read.
///
/// # Errors
/// `resource#action` is no setter of a setting of this device or its person, or the profile has
/// no place to keep it.
pub fn set_on_this_device(resource: &str, action: &str, value: &Value) -> Result<(), String> {
    let left = path().ok_or("this device has no profile to keep settings in")?;
    let waiting = crate::config_path(Path::new(WAITING)).ok_or("this device has no profile")?;
    set_at(&left, &waiting, resource, action, value)
}

fn set_at(
    left: &Path,
    waiting: &Path,
    resource: &str,
    action: &str,
    value: &Value,
) -> Result<(), String> {
    let field = action
        .strip_prefix("set-")
        .filter(|field| !field.is_empty())
        .ok_or_else(|| format!("{action} sets no setting"))?
        .replace('-', "_");
    let held: Left = std::fs::read(left)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let (section, app) = section_at(resource, &held.device)
        .ok_or_else(|| format!("{resource} is no settings section of this device or its person"))?;
    let mut values = Map::from_iter([(field, value.clone())]);
    if let Some(app) = app {
        values = Map::from_iter([(app, Value::Object(values))]);
    }
    let mut kept = crate::workspace::waiting(waiting);
    kept.push(crate::workspace::WaitingStep {
        sections: std::collections::BTreeMap::from([(section.clone(), values.clone())]),
        // A process outside the session names no person: the change is nobody's to take back.
        actor: None,
        action: Some(format!("{resource}#{action}")),
    });
    crate::workspace::keep_waiting(waiting, &kept).map_err(|error| error.to_string())?;
    let mut settings = held.settings;
    let Value::Object(kept) = settings
        .entry(section)
        .or_insert_with(|| Value::Object(Map::new()))
    else {
        return Err(format!("{resource} holds no section"));
    };
    for (key, value) in values {
        match (kept.get_mut(&key), value) {
            (Some(Value::Object(kept)), Value::Object(value)) => kept.extend(value),
            (_, value) => {
                kept.insert(key, value);
            }
        }
    }
    publish_at(left, &held.device, &settings).map_err(|error| error.to_string())
}

/// The section, and the app whose preferences it is, kept at `resource` on the device `device`
/// (any device while it is not known): the section whose [`placement::address`] it is.
fn section_at(resource: &str, device: &str) -> Option<(String, Option<String>)> {
    let (holder, device) = match resource.strip_prefix("devices/") {
        None => (placement::Holder::Person, device),
        Some(rest) => {
            let (named, _) = rest.split_once("/settings/")?;
            if !device.is_empty() && named != device {
                return None;
            }
            (placement::Holder::Device, named)
        }
    };
    if resource.contains("/apps/") {
        let (section, app) = placement::section_of(holder, placement::APP_KIND, resource)?;
        let named = app.as_deref().filter(|app| !app.is_empty())?;
        return (placement::address(&section, Some(named), device).id == resource)
            .then_some((section, app));
    }
    placement::SECTIONS
        .iter()
        .filter(|(section, kept)| *kept == holder && !placement::per_app(section))
        .find(|(section, _)| placement::address(section, None, device).id == resource)
        .map(|(section, _)| ((*section).to_owned(), None))
}

/// What a setter's run answered: `Ok` once it ran, the session's words otherwise.
pub type Setter = dyn Fn(&str, &str, &Value) -> Result<(), String> + Send + Sync;

/// The settings the session left, read anew whenever the file changed, and changed by their
/// setters.
pub struct Outside {
    path: Option<PathBuf>,
    /// The file's length and time when last read, and what it held.
    read: Mutex<(Option<(u64, std::time::SystemTime)>, Left)>,
    revision: AtomicU64,
    changes: std::sync::Arc<ChangeSignal>,
    /// Runs the setter of `resource#action` with its value.
    set: Box<Setter>,
    /// Rings [`Self::changes`] whenever the session leaves its settings again.
    #[cfg(feature = "watch")]
    _watch: Mutex<Option<crate::watch::SettingsWatch>>,
}

impl Outside {
    /// The settings left at `path`, changed through `set`.
    #[must_use]
    pub fn at(path: Option<PathBuf>, set: Box<Setter>) -> Self {
        let changes = std::sync::Arc::new(ChangeSignal::default());
        #[cfg(feature = "watch")]
        let watch = path.as_deref().and_then(|path| {
            let changes = std::sync::Arc::clone(&changes);
            crate::watch::watch_file_at(path, move || changes.ring())
                .inspect_err(
                    |error| tracing::debug!(%error, "the session's settings are not watched"),
                )
                .ok()
        });
        Self {
            path,
            read: Mutex::new((None, Left::default())),
            revision: AtomicU64::new(1),
            changes,
            set,
            #[cfg(feature = "watch")]
            _watch: Mutex::new(watch),
        }
    }

    /// The settings this profile's session left, changed through its control socket.
    #[must_use]
    pub fn of_this_profile() -> Self {
        Self::at(path(), Box::new(run_through_control_socket))
    }

    /// What the file holds now.
    fn left(&self) -> Left {
        let stamp = self
            .path
            .as_deref()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|meta| Some((meta.len(), meta.modified().ok()?)));
        let mut read = self.read.lock_recover();
        if read.0 != stamp || stamp.is_none() {
            let left = self
                .path
                .as_deref()
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            if left != read.1 {
                self.revision.fetch_add(1, Ordering::AcqRel);
            }
            *read = (stamp, left);
        }
        read.1.clone()
    }
}

impl SettingsSource for Outside {
    fn load(&self) -> Map<String, Value> {
        self.left().settings
    }

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        self.update_section_by(name, values, &ChangeOrigin::default())
    }

    fn update_section_by(
        &self,
        name: &str,
        values: Map<String, Value>,
        _origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        let device = self.left().device;
        let refused =
            |words: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, words);
        if placement::per_app(name) {
            for (app, preferences) in values {
                let Value::Object(preferences) = preferences else {
                    continue;
                };
                let resource = placement::address(name, Some(&app), &device).id;
                for (field, value) in preferences {
                    (self.set)(&resource, &crate::workspace::verb(&field), &value)
                        .map_err(refused)?;
                }
            }
            return Ok(());
        }
        let resource = placement::address(name, None, &device).id;
        for (field, value) in values {
            (self.set)(&resource, &crate::workspace::verb(&field), &value).map_err(refused)?;
        }
        Ok(())
    }

    fn revision(&self) -> u64 {
        self.left();
        self.revision.load(Ordering::Acquire)
    }

    fn changes(&self) -> &ChangeSignal {
        &self.changes
    }
}

/// Run the setter `action` of `resource` with `value` through this login's control socket, as
/// `tilo do` runs it.
///
/// With no socket serving this login at all — no session and no daemon, as on a computer whose
/// session is not running yet — the change is kept on the device instead, for the session that
/// runs next, which reads it as it starts.
fn run_through_control_socket(resource: &str, action: &str, value: &Value) -> Result<(), String> {
    if !control_socket_serves() {
        return set_on_this_device(resource, action, value);
    }
    call(
        resource,
        action,
        Map::from_iter([("value".to_owned(), value.clone())]),
    )
}

/// Whether a socket serves this login's `tilo do`: a session's, or the device's daemon's native
/// endpoint (a socket file or its link on Unix, a named pipe on Windows).
fn control_socket_serves() -> bool {
    tilo_control::client::sockets_from_env()
        .iter()
        .any(|socket| {
            #[cfg(unix)]
            let served = socket.path().exists();
            #[cfg(not(unix))]
            let served = tilo_files::native::socket::is_bound(socket.path());
            served
        })
}

/// Run the verb `action` of this device's section `section` with `change` (its one parameter)
/// through the session, as `tilo do` runs it.
///
/// # Errors
/// The session refused it, or this device's session never left its settings.
pub fn run_on_this_device(section: &str, action: &str, change: Value) -> std::io::Result<()> {
    let device = device().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no session has held this device's settings yet",
        )
    })?;
    let resource = placement::address(section, None, &device).id;
    call(
        &resource,
        action,
        Map::from_iter([("change".to_owned(), change)]),
    )
    .map_err(|words| std::io::Error::new(std::io::ErrorKind::PermissionDenied, words))
}

/// Run `resource#action` with `arguments` through this login's control socket.
fn call(resource: &str, action: &str, arguments: Map<String, Value>) -> Result<(), String> {
    if let Some(reason) = crate::unisolated_test_write() {
        return Err(reason.to_owned());
    }
    let target = tilo_actions::remote::RemoteActionTarget {
        provider: tilo_actions::remote::settings_resources::PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(resource, action),
    };
    tilo_control::front::call_target(
        &tilo_control::client::sockets_from_env(),
        target,
        action,
        &Value::Object(arguments),
        &tilo_control::front::Spelling::tilo(),
    )
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_process_outside_reads_what_the_session_left_and_sets_through_the_setters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let device = "ab".repeat(32);
        let ran = Arc::new(Mutex::new(Vec::new()));
        let outside = {
            let ran = Arc::clone(&ran);
            Outside::at(
                Some(path.clone()),
                Box::new(move |resource, action, value| {
                    ran.lock_recover().push((
                        resource.to_owned(),
                        action.to_owned(),
                        value.clone(),
                    ));
                    Ok(())
                }),
            )
        };
        assert!(
            outside.load().is_empty(),
            "nothing left yet: every default holds"
        );
        let before = outside.revision();

        let Value::Object(settings) = serde_json::json!({"appearance": {"mode": "dark"}}) else {
            unreachable!()
        };
        publish_at(&path, &device, &settings).unwrap();
        assert_eq!(outside.load()["appearance"]["mode"], "dark");
        assert_ne!(
            outside.revision(),
            before,
            "a reader sees the settings moved"
        );

        let Value::Object(mode) = serde_json::json!({"mode": "light", "high_contrast": true})
        else {
            unreachable!()
        };
        outside.update_section("appearance", mode).unwrap();
        let Value::Object(lock) = serde_json::json!({"lock_minutes": 3}) else {
            unreachable!()
        };
        outside.update_section("power", lock).unwrap();
        let Value::Object(own) = serde_json::json!({"so.tilo.osk": {"size": "large"}}) else {
            unreachable!()
        };
        outside
            .update_section(placement::APP_PERSONAL_SECTION, own)
            .unwrap();
        // A section's fields run in no promised order.
        let mut ran = ran.lock_recover().clone();
        ran.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        assert_eq!(
            ran,
            [
                (
                    format!("devices/{device}/settings/power"),
                    "set-lock-minutes".into(),
                    3.into()
                ),
                (
                    "settings/appearance".into(),
                    "set-high-contrast".into(),
                    Value::Bool(true)
                ),
                (
                    "settings/appearance".into(),
                    "set-mode".into(),
                    "light".into()
                ),
                (
                    "settings/apps/so.tilo.osk".into(),
                    "set-size".into(),
                    "large".into()
                ),
            ],
            "each change is its setting's setter, at the section's address"
        );
    }

    #[test]
    fn a_device_without_its_session_keeps_a_setting_for_it_and_reads_it_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let (left, waiting) = (dir.path().join(FILE), dir.path().join(WAITING));
        let device = "cd".repeat(32);
        let Value::Object(before) = serde_json::json!({"appearance": {"mode": "dark"}}) else {
            unreachable!()
        };
        publish_at(&left, &device, &before).unwrap();

        set_at(
            &left,
            &waiting,
            "settings/appearance",
            "set-high-contrast",
            &Value::Bool(true),
        )
        .unwrap();
        set_at(
            &left,
            &waiting,
            &format!("devices/{device}/settings/power"),
            "set-lock-minutes",
            &3.into(),
        )
        .unwrap();
        set_at(
            &left,
            &waiting,
            "settings/apps/so.tilo.osk",
            "set-size",
            &"large".into(),
        )
        .unwrap();
        assert!(
            set_at(
                &left,
                &waiting,
                &format!("devices/{}/settings/power", "ef".repeat(32)),
                "set-lock-minutes",
                &3.into()
            )
            .is_err(),
            "another device's settings are its own session's to set"
        );
        assert!(set_at(&left, &waiting, "settings/appearance", "undo", &Value::Null).is_err());

        let read = Outside::at(Some(left), Box::new(|_, _, _| Err("no session".into()))).load();
        assert_eq!(
            read["appearance"],
            serde_json::json!({"mode": "dark", "high_contrast": true})
        );
        assert_eq!(read["power"]["lock_minutes"], 3);
        assert_eq!(
            read[placement::APP_PERSONAL_SECTION]["so.tilo.osk"]["size"],
            "large"
        );
        let mut kept = Map::new();
        for step in crate::workspace::waiting(&waiting) {
            for (section, values) in &step.sections {
                crate::merge_section(&mut kept, section, values.clone());
            }
        }
        assert_eq!(
            kept["appearance"]["high_contrast"], true,
            "the session writes it once it runs"
        );
        assert_eq!(kept["power"]["lock_minutes"], 3);
        assert_eq!(
            kept[placement::APP_PERSONAL_SECTION]["so.tilo.osk"]["size"],
            "large"
        );
    }
}
