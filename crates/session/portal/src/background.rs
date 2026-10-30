//! `org.freedesktop.impl.portal.Background`: apps that keep running
//! without a window, and apps that start when the person logs in.
//!
//! - `GetAppState` reports which apps run and which of them the person is
//!   using, as the session tells it ([`Background::set_running`] on
//!   [`AppStates`]); the frontend asks it when an app goes windowless.
//! - `NotifyBackground` asks the person (a question of kind
//!   [`BACKGROUND_QUESTION`]) whether an app may go on in the background:
//!   the answer's choice is the reply (0 forbid, 1 allow, 2 allow once).
//! - `EnableAutostart` writes an XDG autostart entry, after the frontend
//!   checked the app's permission. The entry runs the app's *installed*
//!   desktop entry (for a Flatpak, its `flatpak run` wrapper) with the
//!   app's arguments: the session never writes a program an app named.
//!   Each entry written is published (`portal:autostart:<app>`) so the
//!   person sees what starts at login and can remove it.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect;
use tilo_service::{Started, decode, offer};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::reply::{Ended, OrFail, Reply, Results, answer};
use crate::{Portal, plain_text};

/// The kind of the question letting an app run in the background asks.
pub const BACKGROUND_QUESTION: &str = "portal.background";

/// An app running with no window the person sees.
pub const BACKGROUND: u32 = 0;
/// An app with a window.
pub const RUNNING: u32 = 1;
/// The app the person is using.
pub const ACTIVE: u32 = 2;

/// What the sheet shows for an app going on in the background.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundQuestion {
    /// What the app says it is doing, in its words.
    pub name: String,
}

/// Which apps run and how, as the session last said.
#[derive(Clone, Default)]
pub struct AppStates {
    states: Arc<Mutex<BTreeMap<String, u32>>>,
}

impl AppStates {
    /// The apps running now, each [`BACKGROUND`], [`RUNNING`] or
    /// [`ACTIVE`]; whether that changed anything.
    pub fn set(&self, running: impl IntoIterator<Item = (String, u32)>) -> bool {
        let running: BTreeMap<String, u32> = running
            .into_iter()
            .filter(|(app, state)| !app.is_empty() && plain_text(app, 255) && *state <= ACTIVE)
            .collect();
        let mut states = self.states.lock_recover();
        let changed = *states != running;
        *states = running;
        changed
    }

    fn all(&self) -> BTreeMap<String, u32> {
        self.states.lock_recover().clone()
    }
}

/// The autostart entries apps asked for, kept as files in the person's
/// autostart directory.
#[derive(Clone, Debug)]
pub struct Autostart {
    directory: Option<PathBuf>,
}

/// What an autostart entry written for an app is named, so the backend
/// knows its own from the person's.
const AUTOSTART_SUFFIX: &str = ".portal.desktop";

impl Default for Autostart {
    fn default() -> Self {
        Self {
            directory: tilo_paths::xdg::config_home().map(|config| config.join("autostart")),
        }
    }
}

impl Autostart {
    /// Entries kept in `directory`: what tests use.
    #[must_use]
    pub fn at(directory: PathBuf) -> Self {
        Self {
            directory: Some(directory),
        }
    }

    fn path(&self, app: &str) -> Option<PathBuf> {
        let safe = !app.is_empty()
            && app
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        safe.then(|| {
            self.directory
                .as_ref()
                .map(|dir| dir.join(format!("{app}{AUTOSTART_SUFFIX}")))
        })
        .flatten()
    }

    /// The apps that start at login because they asked.
    #[must_use]
    pub fn apps(&self) -> Vec<String> {
        let Some(directory) = &self.directory else {
            return Vec::new();
        };
        let mut apps: Vec<String> = std::fs::read_dir(directory)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(AUTOSTART_SUFFIX)
                    .map(str::to_owned)
            })
            .collect();
        apps.sort();
        apps
    }

    /// Start `entry` (an installed app) at login with `arguments`.
    ///
    /// # Errors
    /// The app id cannot name a file, or it could not be written.
    pub fn enable(
        &self,
        entry: &desktop_entry::DesktopEntry,
        arguments: &[String],
    ) -> std::io::Result<()> {
        let path = self
            .path(&entry.id)
            .ok_or_else(|| std::io::Error::other("no autostart file for this app"))?;
        let program = entry
            .exec
            .as_deref()
            .and_then(|exec| desktop_entry::exec::split_exec(exec).ok())
            .ok_or_else(|| std::io::Error::other("the installed entry runs nothing"))?;
        // The installed entry's program and its own arguments, without the
        // field codes that would open files.
        let argv = program
            .into_iter()
            .filter(|arg| !(arg.len() == 2 && arg.starts_with('%')))
            .chain(arguments.iter().cloned());
        let exec = argv.map(|arg| quote(&arg)).collect::<Vec<_>>().join(" ");
        let text = format!(
            "[Desktop Entry]\nType=Application\nName={}\nExec={exec}\nX-Tilo-Portal-App={}\n",
            entry.name.replace('\n', " "),
            entry.id
        );
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        tilo_files::native::atomic::write_private(&path, text.as_bytes())
            .map_err(std::io::Error::other)
    }

    /// Stop `app` starting at login; `false` when it did not.
    pub fn disable(&self, app: &str) -> bool {
        self.path(app)
            .is_some_and(|path| std::fs::remove_file(path).is_ok())
    }
}

/// The resource prefix of an app's autostart entry.
const AUTOSTART_PREFIX: &str = "portal:autostart:";

/// What an autostart entry an app asked for offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.portal.autostart")]
pub enum AutostartEffect {
    /// Stop the app starting at login
    #[effect(own_device, idempotent)]
    Remove,
}

impl crate::Published for Autostart {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.apps()
            .into_iter()
            .map(|app| {
                offer::<AutostartEffect>(ResourceSpec::new(
                    format!("{AUTOSTART_PREFIX}{app}"),
                    "portal.autostart",
                    app,
                ))
            })
            .collect()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let app = resource.strip_prefix(AUTOSTART_PREFIX)?;
        self.apps()
            .iter()
            .any(|known| known == app)
            .then(|| serde_json::json!({ "app_id": app }))
    }

    fn invoke(
        &self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, DoorError>> {
        let app = resource.strip_prefix(AUTOSTART_PREFIX)?;
        Some(
            decode::<AutostartEffect>(published, resource, action, arguments).and_then(|_| {
                self.disable(app)
                    .then(|| Started::Done(ActionOutput::default()))
                    .ok_or_else(|| DoorError::Unknown(ActionAddress::new(resource, action)))
            }),
        )
    }
}

/// `arg` as one argument of an `Exec=` line.
fn quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@".contains(c));
    let escaped = arg.replace('%', "%%");
    if plain {
        return escaped;
    }
    let mut quoted = String::from("\"");
    for c in escaped.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    // A desktop entry escapes `\` once more in every string value.
    quoted.replace('\\', "\\\\")
}

/// The app's arguments, as the frontend forwards them: the first is the
/// program the app named, which is never run; the rest are bounded text.
fn arguments(commandline: &[String]) -> Option<Vec<String>> {
    (commandline.len() <= 64 && commandline.iter().all(|arg| plain_text(arg, 4096)))
        .then(|| commandline.iter().skip(1).cloned().collect())
}

/// The `org.freedesktop.impl.portal.Background` interface.
pub struct Background {
    portal: Portal,
}

impl Background {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }

    /// Tell the frontend the running apps changed, on `connection`.
    ///
    /// # Errors
    /// The signal was not sent.
    pub async fn running_changed(connection: &zbus::Connection) -> zbus::Result<()> {
        let emitter = zbus::object_server::SignalEmitter::new(connection, crate::OBJECT_PATH)?;
        Self::running_applications_changed(&emitter).await
    }
}

#[interface(name = "org.freedesktop.impl.portal.Background")]
impl Background {
    async fn get_app_state(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<HashMap<String, OwnedValue>> {
        self.portal.frontend(&header).await?;
        Ok(self
            .portal
            .app_states
            .all()
            .into_iter()
            .map(|(app, state)| (app, OwnedValue::from(state)))
            .collect())
    }

    async fn notify_background(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        name: String,
    ) -> Reply {
        answer(async {
            let path = handle.as_str();
            let auth = self
                .portal
                .authenticate(&header, path, &app_id, None)
                .await?;
            if name.is_empty() || !plain_text(&name, 512) {
                return Err(Ended::Failed);
            }
            let asked = self.portal.ask(
                &auth.caller,
                path,
                BACKGROUND_QUESTION,
                BackgroundQuestion { name },
            );
            let answered = self
                .portal
                .requests
                .run(connection, path, auth.owner.as_str(), asked)
                .await?;
            let result = u32::try_from(answered.choice.unwrap_or(1)).or_fail()?;
            if result > 2 {
                return Err(Ended::Failed);
            }
            let mut results = Results::new();
            results.insert("result".into(), OwnedValue::from(result));
            Ok(results)
        })
        .await
    }

    async fn enable_autostart(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        app_id: String,
        enable: bool,
        commandline: Vec<String>,
        flags: u32,
    ) -> bool {
        if self.portal.frontend(&header).await.is_err() || app_id.is_empty() || flags > 1 {
            return false;
        }
        if !enable {
            self.portal.autostart.disable(&app_id);
            return true;
        }
        let Some(arguments) = arguments(&commandline) else {
            return false;
        };
        let scan = desktop_entry::cache::cached_scan();
        let Some(entry) = scan.find(&app_id) else {
            tracing::debug!(app_id, "autostart refused: the app has no installed entry");
            return false;
        };
        self.portal
            .autostart
            .enable(entry, &arguments)
            .inspect_err(|error| tracing::warn!(%error, app_id, "autostart not written"))
            .is_ok()
    }

    #[zbus(signal)]
    async fn running_applications_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::Result<()>;
}

/// The file an autostart entry of `app` is in, under `directory`.
#[must_use]
pub fn autostart_file(directory: &Path, app: &str) -> PathBuf {
    directory.join(format!("{app}{AUTOSTART_SUFFIX}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_apps_are_bounded_and_change_only_when_they_do() {
        let states = AppStates::default();
        assert!(states.set([
            ("org.example.A".to_owned(), ACTIVE),
            ("bad\n".to_owned(), RUNNING)
        ]));
        assert_eq!(
            states.all(),
            BTreeMap::from([("org.example.A".to_owned(), ACTIVE)])
        );
        assert!(!states.set([("org.example.A".to_owned(), ACTIVE)]));
        assert!(
            states.set([("org.example.A".to_owned(), 7)]),
            "an unknown state is dropped"
        );
        assert!(states.all().is_empty());
    }

    #[test]
    fn an_autostart_entry_runs_the_installed_program_never_the_apps() {
        let dir = tempfile::tempdir().unwrap();
        let autostart = Autostart::at(dir.path().to_path_buf());
        let entry = desktop_entry::entry::parse(
            "[Desktop Entry]\nType=Application\nName=Chat\n\
             Exec=/usr/bin/flatpak run --branch=stable org.example.Chat %U\n",
            "org.example.Chat",
            Path::new("/var/lib/flatpak/exports/share/applications/org.example.Chat.desktop"),
            &[],
        )
        .unwrap();
        let commandline =
            arguments(&["/tmp/evil".into(), "--hidden".into(), "50% $HOME".into()]).unwrap();
        autostart.enable(&entry, &commandline).unwrap();
        let text = std::fs::read_to_string(autostart_file(dir.path(), "org.example.Chat")).unwrap();
        assert!(text.contains(
            "Exec=/usr/bin/flatpak run --branch=stable org.example.Chat --hidden \"50%% \\\\$HOME\"\n"
        ), "{text}");
        assert!(!text.contains("/tmp/evil"));
        assert_eq!(autostart.apps(), ["org.example.Chat"]);
        assert!(autostart.disable("org.example.Chat"));
        assert!(autostart.apps().is_empty());
        assert!(
            !autostart.disable("../../.bashrc"),
            "an id that is no file name"
        );
    }
}
