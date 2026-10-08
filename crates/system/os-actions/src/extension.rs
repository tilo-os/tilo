//! Downloaded system extensions, as the desktop sees them.
//!
//! Some optional `/usr` content is not part of the image: the owner turns it
//! on, and the root helper `/usr/libexec/tilo-extension` downloads it on this
//! device from the publishers' servers, verifies it against a manifest the
//! image's own release key signed, and merges it as read-only system
//! extensions. The desktop never runs that helper itself: like every other
//! privileged change Settings makes, it starts one of the fixed unit
//! instances polkit lets a local, active administrator start
//! (`tilo-extension@{install,remove,refresh}:<id>.service`) and reads the
//! helper's world-readable status file. See
//! `/usr/share/tilo/system-extensions/README.md`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The root helper. Its presence (with a catalogue entry for the id) is what
/// makes an extension offerable at all: a host-desktop session or a
/// development build without it shows nothing.
pub const HELPER_PATH: &str = "/usr/libexec/tilo-extension";
/// The image-owned catalogue: one `<id>.json` per extension this image allows.
pub const CATALOGUE_DIR: &str = "/usr/share/tilo/system-extensions/sysext.d";
/// Root-owned, world-readable status directory; exists on every Tilo image
/// (tmpfiles.d), so it can be watched before anything was ever installed.
pub const STATE_DIR: &str = "/var/lib/tilo/extensions";
/// Resolving, downloading and assembling an extension takes minutes on a
/// slow link.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_TEXT: usize = 300;
const MAX_PARTS: usize = 16;
const MAX_ID: usize = 63;

/// A catalogue id: `[a-z0-9][a-z0-9-]{0,62}`. Only valid ids reach unit
/// names and paths.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtensionId(String);

impl ExtensionId {
    /// `None` unless `id` is a well-formed catalogue id.
    #[must_use]
    pub fn new(id: &str) -> Option<Self> {
        let mut bytes = id.bytes();
        let first = bytes.next()?;
        let valid = id.len() <= MAX_ID
            && (first.is_ascii_lowercase() || first.is_ascii_digit())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        valid.then(|| Self(id.to_owned()))
    }

    /// An id this crate knows to be well formed.
    pub(crate) fn known(id: &'static str) -> Self {
        debug_assert!(Self::new(id).is_some(), "{id} is not a well-formed id");
        Self(id.to_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The helper's status file for this extension.
    #[must_use]
    pub fn status_path(&self) -> PathBuf {
        Path::new(STATE_DIR).join(self.status_file_name())
    }

    /// The image's catalogue entry for this extension.
    #[must_use]
    pub fn catalogue_path(&self) -> PathBuf {
        Path::new(CATALOGUE_DIR).join(format!("{}.json", self.0))
    }

    fn status_file_name(&self) -> String {
        format!("{}.json", self.0)
    }
}

impl fmt::Display for ExtensionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the owner asked for. Each maps to exactly one unit instance per
/// extension, the only ones polkit lets a desktop session start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExtensionAction {
    /// Opt in and install (also the retry after a failure).
    Install,
    /// Opt out: unmerge and delete everything the extension added.
    Remove,
    /// Re-resolve now, e.g. to look for a part that was waiting.
    Refresh,
}

impl ExtensionAction {
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Remove => "remove",
            Self::Refresh => "refresh",
        }
    }

    /// `tilo-extension@<verb>:<id>.service`.
    #[must_use]
    pub fn unit(self, id: &ExtensionId) -> String {
        format!("tilo-extension@{}:{id}.service", self.verb())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExtensionState {
    #[default]
    NotInstalled,
    /// The owner opted in while offline; the helper installs as soon as the
    /// device is online (`NetworkManager` starts it) and the choice survives
    /// reboots.
    WaitingForNetwork,
    Installing,
    Installed,
    Removing,
    Failed,
}

/// One part (one systemd-sysext) of an extension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PartState {
    #[default]
    Off,
    Enabled,
    /// No compatible upstream build for this base exists yet.
    Waiting,
    /// Never available on this base.
    Unsupported,
}

/// Where a running install is, as the helper reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// 1-based step currently running.
    pub step: u32,
    pub steps: u32,
    /// What the step is doing, in plain language ("Downloading").
    pub label: String,
    /// Bytes downloaded so far and in total, while the step downloads.
    pub bytes: Option<(u64, u64)>,
}

impl Progress {
    /// Completed fraction for a progress bar: a step counts once it
    /// finishes, and a download moves the bar through its own step.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.steps == 0 {
            return 0.0;
        }
        let done = self.step.saturating_sub(1).min(self.steps);
        #[allow(
            clippy::cast_precision_loss,
            reason = "step counts are single digits; byte counts only need a bar's precision"
        )]
        let within = self
            .bytes
            .filter(|(_, total)| *total > 0 && done < self.steps)
            .map_or(0.0, |(bytes, total)| bytes.min(total) as f32 / total as f32);
        #[allow(clippy::cast_precision_loss, reason = "step counts are single digits")]
        let fraction = (done as f32 + within) / self.steps as f32;
        fraction
    }

    /// Downloaded and total megabytes, rounded for a status line.
    #[must_use]
    pub fn megabytes(&self) -> Option<(u64, u64)> {
        const MB: u64 = 1_000_000;
        self.bytes
            .filter(|(_, total)| *total > 0)
            .map(|(bytes, total)| (bytes.min(total) / MB, total.div_ceil(MB)))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtensionStatus {
    pub state: ExtensionState,
    /// Whether the owner's opt-in is recorded (it survives failures and
    /// offline periods until they remove the extension).
    pub desired: bool,
    pub progress: Option<Progress>,
    /// Part name to state; parts the helper did not mention are off.
    pub parts: BTreeMap<String, PartState>,
    /// The helper's own plain-language note on the current state.
    pub message: String,
    pub last_error: String,
    /// The accepted manifest revision.
    pub revision: u64,
    /// Increases after every merge or unmerge.
    pub generation: u64,
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(bounded_text)
        .unwrap_or_default()
}

pub(crate) fn bounded_text(raw: &str) -> String {
    raw.chars()
        .filter(|character| !character.is_control())
        .take(MAX_TEXT)
        .collect()
}

fn number(value: &serde_json::Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default()
}

fn progress(value: &serde_json::Value) -> Option<Progress> {
    let progress = value.get("progress")?;
    let number = |key: &str| {
        progress
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let steps = number("steps")?.min(64);
    let step = number("step")?.min(steps);
    let bytes = progress
        .get("bytes")
        .and_then(serde_json::Value::as_u64)
        .zip(progress.get("total").and_then(serde_json::Value::as_u64));
    (steps > 0).then(|| Progress {
        step,
        steps,
        label: text(progress, "label"),
        bytes,
    })
}

fn parts(value: &serde_json::Value) -> BTreeMap<String, PartState> {
    let Some(parts) = value.get("parts").and_then(serde_json::Value::as_object) else {
        return BTreeMap::new();
    };
    parts
        .iter()
        .filter(|(name, _)| ExtensionId::new(name).is_some())
        .take(MAX_PARTS)
        .map(|(name, state)| {
            let state = match state.as_str() {
                Some("enabled") => PartState::Enabled,
                Some("waiting") => PartState::Waiting,
                Some("unsupported") => PartState::Unsupported,
                _ => PartState::Off,
            };
            (name.clone(), state)
        })
        .collect()
}

impl ExtensionStatus {
    /// Parse the helper's status JSON. Anything unreadable is "not
    /// installed": the controls then offer Install, and the helper decides
    /// what is true.
    #[must_use]
    pub fn parse(json: &str) -> Self {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            return Self::default();
        };
        let state = match text(&value, "state").as_str() {
            "waiting-for-network" => ExtensionState::WaitingForNetwork,
            "installing" => ExtensionState::Installing,
            "installed" => ExtensionState::Installed,
            "removing" => ExtensionState::Removing,
            "failed" => ExtensionState::Failed,
            _ => ExtensionState::NotInstalled,
        };
        Self {
            state,
            desired: value
                .get("desired")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            progress: (state == ExtensionState::Installing)
                .then(|| progress(&value))
                .flatten(),
            parts: parts(&value),
            message: text(&value, "message"),
            last_error: text(&value, "last_error"),
            revision: number(&value, "revision"),
            generation: number(&value, "generation"),
        }
    }

    /// The status of `id` on this machine, or "not installed" when there is
    /// none.
    #[must_use]
    pub fn read(id: &ExtensionId) -> Self {
        Self::read_from(&id.status_path())
    }

    #[must_use]
    pub fn read_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|json| Self::parse(&json))
            .unwrap_or_default()
    }

    /// A part's state; parts the helper did not report are off.
    #[must_use]
    pub fn part(&self, name: &str) -> PartState {
        self.parts.get(name).copied().unwrap_or_default()
    }

    /// Whether the owner has already made the choice to install: first-login
    /// setup does not ask again (the installer may have recorded it).
    #[must_use]
    pub fn opted_in(&self) -> bool {
        self.desired
            || matches!(
                self.state,
                ExtensionState::WaitingForNetwork
                    | ExtensionState::Installing
                    | ExtensionState::Installed
            )
    }
}

/// Whether this system can install `id` at all: the helper exists and the
/// image carries a catalogue entry for it.
#[must_use]
pub fn available(id: &ExtensionId) -> bool {
    Path::new(HELPER_PATH).is_file() && id.catalogue_path().is_file()
}

/// Start the one unit instance for `action` on `id` and wait until it
/// finishes.
///
/// The helper reports progress in the status file while this runs; [`watch`]
/// delivers those updates. Stopping the caller does not stop the
/// transaction: the unit keeps running in the system manager.
///
/// # Errors
/// Returns the unit's failure reason when systemd, polkit or the helper refuse
/// or fail.
pub fn run(action: ExtensionAction, id: &ExtensionId) -> Result<(), String> {
    let unit = action.unit(id);
    let output = bounded_command::run(
        "systemctl",
        &["start", "--wait", &unit],
        None,
        MAX_OUTPUT,
        OPERATION_TIMEOUT,
    )
    .map_err(|error| format!("systemctl command failed: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let status = ExtensionStatus::read(id);
    if !status.last_error.is_empty() {
        return Err(status.last_error);
    }
    let stderr = bounded_text(String::from_utf8_lossy(&output.stderr).trim());
    Err(if stderr.is_empty() {
        format!("{unit} failed; see journalctl -u {unit}")
    } else {
        stderr
    })
}

/// Call `on_change` whenever the helper rewrites the status of `id`, from a
/// background thread, until it returns `false`. Event-driven (inotify on
/// [`STATE_DIR`]): nothing polls.
///
/// # Errors
/// Returns an error when the state directory cannot be watched (not a Tilo
/// image); callers then re-read the status after their own actions only.
pub fn watch(
    id: &ExtensionId,
    on_change: impl FnMut() -> bool + Send + 'static,
) -> Result<(), String> {
    watch_dir(Path::new(STATE_DIR), id, on_change)
}

/// [`watch`] on an arbitrary directory (tests).
///
/// # Errors
/// As [`watch`].
#[cfg(target_os = "linux")]
pub fn watch_dir(
    dir: &Path,
    id: &ExtensionId,
    mut on_change: impl FnMut() -> bool + Send + 'static,
) -> Result<(), String> {
    use rustix::fs::inotify;
    let fd = inotify::init(inotify::CreateFlags::CLOEXEC)
        .map_err(|error| format!("inotify: {error}"))?;
    inotify::add_watch(
        &fd,
        dir,
        inotify::WatchFlags::CLOSE_WRITE
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::DELETE,
    )
    .map_err(|error| format!("watch {}: {error}", dir.display()))?;
    let file_name = id.status_file_name();
    std::thread::Builder::new()
        .name(format!("extension-watch-{id}"))
        .spawn(move || {
            let mut buffer = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
            let mut reader = inotify::Reader::new(&fd, &mut buffer);
            loop {
                let relevant = match reader.next() {
                    Ok(event) => event
                        .file_name()
                        .is_some_and(|name| name.to_bytes() == file_name.as_bytes()),
                    Err(rustix::io::Errno::INTR) => false,
                    Err(_) => return,
                };
                if relevant && !on_change() {
                    return;
                }
            }
        })
        .map(|_| ())
        .map_err(|error| format!("extension watcher: {error}"))
}

/// Downloaded system extensions exist only on the Tilo OS image; elsewhere
/// there is no status to watch.
///
/// # Errors
/// Always.
#[cfg(not(target_os = "linux"))]
pub fn watch_dir(
    dir: &Path,
    _id: &ExtensionId,
    _on_change: impl FnMut() -> bool + Send + 'static,
) -> Result<(), String> {
    Err(format!(
        "watch {}: system extensions exist only on Tilo OS",
        dir.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn id(raw: &str) -> ExtensionId {
        ExtensionId::new(raw).unwrap()
    }

    #[test]
    fn ids_are_strict() {
        for good in ["media-codecs", "a", "0", "x1-2-3", &"a".repeat(63)] {
            assert!(ExtensionId::new(good).is_some(), "{good}");
        }
        for bad in [
            "",
            "-codecs",
            "Media",
            "media_codecs",
            "media.codecs",
            "../etc",
            "a/b",
            "media codecs",
            "medía",
            "a:b",
            "a@b",
            &"a".repeat(64),
        ] {
            assert!(ExtensionId::new(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn units_and_paths_follow_the_id() {
        let codecs = id("media-codecs");
        assert_eq!(
            ExtensionAction::Install.unit(&codecs),
            "tilo-extension@install:media-codecs.service"
        );
        assert_eq!(
            ExtensionAction::Remove.unit(&codecs),
            "tilo-extension@remove:media-codecs.service"
        );
        assert_eq!(
            ExtensionAction::Refresh.unit(&codecs),
            "tilo-extension@refresh:media-codecs.service"
        );
        assert_eq!(
            codecs.status_path(),
            Path::new("/var/lib/tilo/extensions/media-codecs.json")
        );
        assert_eq!(
            codecs.catalogue_path(),
            Path::new("/usr/share/tilo/system-extensions/sysext.d/media-codecs.json")
        );
    }

    #[test]
    fn the_full_status_parses() {
        let status = ExtensionStatus::parse(
            r#"{"schema":1,"id":"media-codecs","state":"installed","desired":true,
                "parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting","Bad Name":"enabled"},
                "message":"Hardware video is waiting","last_error":"","revision":20261003120000,
                "base":{"os_id":"fedora"},"packages":{"tilo-codecs":["x"]},"generation":7,
                "updated":"2026-10-03T12:00:00Z"}"#,
        );
        assert_eq!(status.state, ExtensionState::Installed);
        assert!(status.desired);
        assert_eq!(status.part("tilo-codecs"), PartState::Enabled);
        assert_eq!(status.part("tilo-codecs-mesa"), PartState::Waiting);
        assert_eq!(status.part("unknown"), PartState::Off);
        assert_eq!(status.parts.len(), 2, "malformed part names are dropped");
        assert_eq!(status.message, "Hardware video is waiting");
        assert_eq!(status.revision, 20_261_003_120_000);
        assert_eq!(status.generation, 7);
    }

    #[test]
    fn missing_or_garbled_status_is_not_installed() {
        assert_eq!(ExtensionStatus::parse(""), ExtensionStatus::default());
        assert_eq!(
            ExtensionStatus::parse("[1,2]").state,
            ExtensionState::NotInstalled
        );
        assert_eq!(
            ExtensionStatus::parse(r#"{"state":"exploded","parts":[1]}"#),
            ExtensionStatus::default()
        );
    }

    #[test]
    fn progress_is_reported_while_installing_only() {
        let installing = ExtensionStatus::parse(
            r#"{"state":"installing","progress":{"step":3,"steps":6,"label":"Downloading"}}"#,
        );
        let progress = installing.progress.unwrap();
        assert_eq!(progress.step, 3);
        assert!((progress.fraction() - 2.0 / 6.0).abs() < f32::EPSILON);
        assert_eq!(progress.megabytes(), None);
        let downloading = ExtensionStatus::parse(
            r#"{"state":"installing","progress":{"step":3,"steps":5,"label":"Downloading","bytes":12500000,"total":40000000}}"#,
        )
        .progress
        .unwrap();
        assert_eq!(downloading.bytes, Some((12_500_000, 40_000_000)));
        assert_eq!(downloading.megabytes(), Some((12, 40)));
        assert!((downloading.fraction() - (2.0 + 0.3125) / 5.0).abs() < 1e-6);
        let stale = ExtensionStatus::parse(
            r#"{"state":"installed","progress":{"step":3,"steps":6,"label":"x"}}"#,
        );
        assert_eq!(stale.progress, None);
        let hostile = ExtensionStatus::parse(
            r#"{"state":"installing","progress":{"step":90,"steps":0,"label":"x"}}"#,
        );
        assert_eq!(hostile.progress, None);
    }

    #[test]
    fn opting_in_is_remembered_across_failures_and_offline_periods() {
        assert!(!ExtensionStatus::parse(r#"{"state":"not-installed"}"#).opted_in());
        assert!(ExtensionStatus::parse(r#"{"state":"waiting-for-network"}"#).opted_in());
        assert!(ExtensionStatus::parse(r#"{"state":"failed","desired":true}"#).opted_in());
        assert!(!ExtensionStatus::parse(r#"{"state":"failed","desired":false}"#).opted_in());
    }

    #[test]
    fn text_is_bounded_and_printable() {
        let long = "x".repeat(1000);
        let status = ExtensionStatus::parse(&format!(
            r#"{{"state":"failed","last_error":"a\u001b[31m{long}","message":"{long}"}}"#
        ));
        assert!(status.last_error.len() <= MAX_TEXT);
        assert!(status.message.len() <= MAX_TEXT);
        assert!(!status.last_error.contains('\u{1b}'));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_watcher_reports_an_atomic_rewrite_of_its_own_status_only() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        watch_dir(dir.path(), &id("media-codecs"), move || tx.send(()).is_ok()).unwrap();
        // Another extension's status is not this watcher's business.
        std::fs::write(dir.path().join("other.json"), "{}").unwrap();
        // The helper writes a temporary file and renames it over the status.
        let temporary = dir.path().join(".media-codecs.json.tmp");
        std::fs::write(&temporary, r#"{"state":"installing"}"#).unwrap();
        std::fs::rename(&temporary, dir.path().join("media-codecs.json")).unwrap();
        rx.recv_timeout(tilo_test_env::hang_bound())
            .expect("rename over the status is reported");
        assert!(
            rx.try_recv().is_err(),
            "only the watched status is reported"
        );
        assert_eq!(
            ExtensionStatus::read_from(&dir.path().join("media-codecs.json")).state,
            ExtensionState::Installing
        );
    }
}
