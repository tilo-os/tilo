//! The opt-in additional codecs pack, as the desktop sees it.
//!
//! Tilo ships no patented codecs. When the owner asks, the root helper
//! `/usr/libexec/tilo-codec-pack` downloads them on this device from RPM Fusion
//! and from Cisco, verifies every package against pinned keys and merges
//! them as local system extensions. The desktop never runs that helper itself:
//! like every other privileged change Settings makes (battery cap, update
//! policy, rollback), it starts one of three fixed unit instances that polkit
//! lets a local administrator start (`49-tilo-codec-pack.rules`), and reads the
//! helper's world-readable status file. First-login setup and Settings share
//! this module. See `/usr/share/doc/tilo/codec-pack.md`.

use std::path::Path;
use std::time::Duration;

/// The root helper. Its presence is what makes the pack offerable at all: a
/// host-desktop session or a development build without it shows nothing.
pub const HELPER_PATH: &str = "/usr/libexec/tilo-codec-pack";
/// Root-owned, world-readable state directory; exists on every Tilo image
/// (tmpfiles.d), so it can be watched before the pack was ever installed.
pub const STATE_DIR: &str = "/var/lib/tilo/codec-pack";
/// Root-owned, world-readable status the helper rewrites atomically.
pub const STATUS_PATH: &str = "/var/lib/tilo/codec-pack/status.json";
/// Resolving, downloading and unpacking the pack takes minutes on a slow link.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_TEXT: usize = 300;

/// What the owner asked for. Each maps to exactly one unit instance, the only
/// three the codec-pack polkit rule lets a desktop session start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodecAction {
    /// Opt in and install (also the retry after a failure).
    Install,
    /// Opt out: unmerge and delete everything the pack added.
    Remove,
    /// Re-resolve now, e.g. to look for a Mesa build that matches the image.
    Refresh,
}

impl CodecAction {
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::Install => "tilo-codec-pack@install.service",
            Self::Remove => "tilo-codec-pack@remove.service",
            Self::Refresh => "tilo-codec-pack@refresh.service",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PackState {
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

/// The freeworld Mesa part, which must match the base Mesa exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MesaState {
    #[default]
    Off,
    Enabled,
    /// No RPM Fusion build of the base's Mesa version exists yet.
    Waiting,
    Unsupported,
}

/// Where a running install is, as the helper reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// 1-based step currently running.
    pub step: u32,
    pub steps: u32,
    /// What the step is doing, in plain language ("Downloading codecs").
    pub label: String,
}

impl Progress {
    /// Completed fraction for a progress bar: a step counts once it finishes.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.steps == 0 {
            return 0.0;
        }
        let done = self.step.saturating_sub(1).min(self.steps);
        #[allow(clippy::cast_precision_loss, reason = "step counts are single digits")]
        let fraction = done as f32 / self.steps as f32;
        fraction
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodecPackStatus {
    pub state: PackState,
    pub mesa: MesaState,
    /// Whether the owner's opt-in is recorded (it survives failures and
    /// offline periods until they remove the pack).
    pub desired: bool,
    pub last_error: String,
    pub progress: Option<Progress>,
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

fn bounded_text(raw: &str) -> String {
    raw.chars()
        .filter(|character| !character.is_control())
        .take(MAX_TEXT)
        .collect()
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
    (steps > 0).then(|| Progress {
        step,
        steps,
        label: text(progress, "label"),
    })
}

impl CodecPackStatus {
    /// Parse the helper's status JSON. Anything unreadable is "not installed":
    /// the controls then offer Install, and the helper decides what is true.
    #[must_use]
    pub fn parse(json: &str) -> Self {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            return Self::default();
        };
        let state = match text(&value, "state").as_str() {
            "waiting-for-network" => PackState::WaitingForNetwork,
            "installing" => PackState::Installing,
            "installed" => PackState::Installed,
            "removing" => PackState::Removing,
            "failed" => PackState::Failed,
            _ => PackState::NotInstalled,
        };
        let mesa = match text(&value, "mesa").as_str() {
            "enabled" => MesaState::Enabled,
            "waiting" => MesaState::Waiting,
            "unsupported" => MesaState::Unsupported,
            _ => MesaState::Off,
        };
        Self {
            state,
            mesa,
            desired: value
                .get("desired")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            last_error: text(&value, "last_error"),
            progress: (state == PackState::Installing)
                .then(|| progress(&value))
                .flatten(),
            generation: value
                .get("generation")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
        }
    }

    /// The status on this machine, or "not installed" when there is none.
    #[must_use]
    pub fn read() -> Self {
        Self::read_from(Path::new(STATUS_PATH))
    }

    #[must_use]
    pub fn read_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|json| Self::parse(&json))
            .unwrap_or_default()
    }

    /// Whether the owner has already made the choice to install: first-login
    /// setup does not ask again (the installer may have recorded it).
    #[must_use]
    pub fn opted_in(&self) -> bool {
        self.desired
            || matches!(
                self.state,
                PackState::WaitingForNetwork | PackState::Installing | PackState::Installed
            )
    }

    /// The one status line the controls show.
    #[must_use]
    pub fn summary(&self) -> String {
        match (self.state, self.mesa) {
            (PackState::NotInstalled, _) => "Not installed".into(),
            (PackState::WaitingForNetwork, _) => {
                "Waiting for an internet connection. Installation starts automatically once this device is online."
                    .into()
            }
            (PackState::Installing, _) => match &self.progress {
                Some(progress) if !progress.label.is_empty() => format!(
                    "{} (step {} of {})\u{2026}",
                    progress.label, progress.step, progress.steps
                ),
                _ => "Downloading and installing\u{2026}".into(),
            },
            (PackState::Removing, _) => "Removing\u{2026}".into(),
            (PackState::Failed, _) if self.last_error.is_empty() => "Installation failed".into(),
            (PackState::Failed, _) => format!("Installation failed: {}", self.last_error),
            (PackState::Installed, MesaState::Enabled) => {
                "Installed, including hardware H.264/HEVC".into()
            }
            (PackState::Installed, MesaState::Waiting) => {
                "Installed. Hardware H.264/HEVC is waiting for a matching Mesa build from RPM Fusion."
                    .into()
            }
            (PackState::Installed, MesaState::Off | MesaState::Unsupported) => {
                "Installed (software codecs)".into()
            }
        }
    }

    /// A background refresh that failed while the pack kept working.
    #[must_use]
    pub fn refresh_problem(&self) -> Option<String> {
        (self.state == PackState::Installed && !self.last_error.is_empty()).then(|| {
            format!(
                "The last update of the codec pack failed: {}",
                self.last_error
            )
        })
    }

    /// When the new codecs take effect, stated exactly. `None` while nothing
    /// is installed.
    #[must_use]
    pub fn when_it_applies(&self) -> Option<&'static str> {
        if self.state != PackState::Installed {
            return None;
        }
        Some(match self.mesa {
            MesaState::Enabled => {
                "Apps you open from now on can play and record H.264 and HEVC, with hardware \
                 acceleration. The desktop itself loads the new video driver when it starts: sign \
                 out and back in to use hardware H.264/HEVC for screen sharing and recording, and \
                 reopen apps that were already open."
            }
            MesaState::Waiting | MesaState::Off | MesaState::Unsupported => {
                "New codecs work right away: calls, screen sharing and videos pick them up the \
                 next time they start. No restart is needed."
            }
        })
    }

    /// The actions the controls offer, primary first.
    #[must_use]
    pub fn offered_actions(&self) -> Vec<CodecAction> {
        match self.state {
            PackState::Installing | PackState::Removing => Vec::new(),
            PackState::Installed if self.mesa == MesaState::Waiting => {
                vec![CodecAction::Refresh, CodecAction::Remove]
            }
            PackState::WaitingForNetwork | PackState::Installed => vec![CodecAction::Remove],
            PackState::Failed if self.desired => vec![CodecAction::Install, CodecAction::Remove],
            PackState::NotInstalled | PackState::Failed => vec![CodecAction::Install],
        }
    }

    /// The button label for `action` in this state.
    #[must_use]
    pub fn action_label(&self, action: CodecAction) -> &'static str {
        match (action, self.state) {
            (CodecAction::Install, PackState::Failed) => "Try again",
            (CodecAction::Install, _) => "Install media codecs",
            (CodecAction::Remove, PackState::WaitingForNetwork) => "Cancel installation",
            (CodecAction::Remove, _) => "Remove media codecs",
            (CodecAction::Refresh, _) => "Check for the hardware driver again",
        }
    }
}

/// Whether this system can install the pack at all.
#[must_use]
pub fn available() -> bool {
    Path::new(HELPER_PATH).is_file()
}

/// Start one fixed unit instance and wait until it finishes.
///
/// The helper reports progress in [`STATUS_PATH`] while this runs; [`watch`]
/// delivers those updates. Stopping the caller does not stop the install: the
/// unit keeps running in the system manager.
///
/// # Errors
/// Returns the unit's failure reason when systemd, polkit or the helper refuse
/// or fail.
pub fn run(action: CodecAction) -> Result<(), String> {
    let unit = action.unit();
    let output = bounded_command::run(
        "systemctl",
        &["start", "--wait", unit],
        None,
        MAX_OUTPUT,
        OPERATION_TIMEOUT,
    )
    .map_err(|error| format!("systemctl command failed: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let status = CodecPackStatus::read();
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

/// Call `on_change` whenever the helper rewrites its status, from a background
/// thread, until it returns `false`. Event-driven (inotify on [`STATE_DIR`]):
/// nothing polls.
///
/// # Errors
/// Returns an error when the state directory cannot be watched (not a Tilo
/// image); callers then re-read the status after their own actions only.
pub fn watch(on_change: impl FnMut() -> bool + Send + 'static) -> Result<(), String> {
    watch_dir(Path::new(STATE_DIR), on_change)
}

/// [`watch`] on an arbitrary directory (tests).
///
/// # Errors
/// As [`watch`].
#[cfg(target_os = "linux")]
pub fn watch_dir(
    dir: &Path,
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
    std::thread::Builder::new()
        .name("codec-pack-watch".into())
        .spawn(move || {
            let mut buffer = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
            let mut reader = inotify::Reader::new(&fd, &mut buffer);
            loop {
                let relevant = match reader.next() {
                    Ok(event) => event
                        .file_name()
                        .is_some_and(|name| name.to_bytes() == b"status.json"),
                    Err(rustix::io::Errno::INTR) => false,
                    Err(_) => return,
                };
                if relevant && !on_change() {
                    return;
                }
            }
        })
        .map(|_| ())
        .map_err(|error| format!("codec-pack watcher: {error}"))
}

/// The codec pack is part of the Tilo OS image; elsewhere there is no status
/// to watch.
///
/// # Errors
/// Always.
#[cfg(not(target_os = "linux"))]
pub fn watch_dir(
    dir: &Path,
    _on_change: impl FnMut() -> bool + Send + 'static,
) -> Result<(), String> {
    Err(format!(
        "watch {}: the codec pack exists only on Tilo OS",
        dir.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn missing_or_garbled_status_is_not_installed() {
        assert_eq!(CodecPackStatus::parse(""), CodecPackStatus::default());
        assert_eq!(
            CodecPackStatus::parse("[1,2]").state,
            PackState::NotInstalled
        );
        assert_eq!(
            CodecPackStatus::parse(r#"{"state":"exploded"}"#).offered_actions(),
            vec![CodecAction::Install]
        );
    }

    #[test]
    fn installed_with_mesa_waiting_offers_a_recheck() {
        let status = CodecPackStatus::parse(
            r#"{"schema":1,"state":"installed","mesa":"waiting","generation":4,"last_error":"","desired":true}"#,
        );
        assert_eq!(status.state, PackState::Installed);
        assert_eq!(status.mesa, MesaState::Waiting);
        assert_eq!(status.generation, 4);
        assert!(
            status
                .summary()
                .contains("waiting for a matching Mesa build from RPM Fusion")
        );
        assert_eq!(
            status.offered_actions(),
            vec![CodecAction::Refresh, CodecAction::Remove]
        );
        assert_eq!(status.refresh_problem(), None);
        assert!(status.when_it_applies().unwrap().contains("No restart"));
    }

    #[test]
    fn every_state_has_its_line_and_actions() {
        let cases = [
            (
                "not-installed",
                false,
                "Not installed",
                vec![CodecAction::Install],
            ),
            (
                "waiting-for-network",
                true,
                "Waiting for an internet connection. Installation starts automatically once this device is online.",
                vec![CodecAction::Remove],
            ),
            (
                "installing",
                true,
                "Downloading and installing\u{2026}",
                vec![],
            ),
            ("removing", false, "Removing\u{2026}", vec![]),
            (
                "failed",
                true,
                "Installation failed: offline",
                vec![CodecAction::Install, CodecAction::Remove],
            ),
            (
                "failed",
                false,
                "Installation failed: offline",
                vec![CodecAction::Install],
            ),
        ];
        for (state, desired, line, actions) in cases {
            let status = CodecPackStatus::parse(&format!(
                r#"{{"state":"{state}","last_error":"offline","desired":{desired}}}"#
            ));
            assert_eq!(status.summary(), line, "{state}");
            assert_eq!(status.offered_actions(), actions, "{state}");
        }
        let enabled = CodecPackStatus::parse(r#"{"state":"installed","mesa":"enabled"}"#);
        assert_eq!(
            enabled.summary(),
            "Installed, including hardware H.264/HEVC"
        );
        assert!(
            enabled
                .when_it_applies()
                .unwrap()
                .contains("sign out and back in")
        );
    }

    #[test]
    fn progress_is_reported_while_installing_only() {
        let installing = CodecPackStatus::parse(
            r#"{"state":"installing","progress":{"step":3,"steps":6,"label":"Downloading codecs"}}"#,
        );
        let progress = installing.progress.clone().unwrap();
        assert_eq!(progress.step, 3);
        assert!((progress.fraction() - 2.0 / 6.0).abs() < f32::EPSILON);
        assert_eq!(
            installing.summary(),
            "Downloading codecs (step 3 of 6)\u{2026}"
        );
        let stale = CodecPackStatus::parse(
            r#"{"state":"installed","progress":{"step":3,"steps":6,"label":"x"}}"#,
        );
        assert_eq!(stale.progress, None);
        let hostile = CodecPackStatus::parse(
            r#"{"state":"installing","progress":{"step":90,"steps":0,"label":"x"}}"#,
        );
        assert_eq!(hostile.progress, None);
    }

    #[test]
    fn opting_in_is_remembered_across_failures_and_offline_periods() {
        assert!(!CodecPackStatus::parse(r#"{"state":"not-installed"}"#).opted_in());
        assert!(CodecPackStatus::parse(r#"{"state":"waiting-for-network"}"#).opted_in());
        assert!(CodecPackStatus::parse(r#"{"state":"failed","desired":true}"#).opted_in());
        assert!(!CodecPackStatus::parse(r#"{"state":"failed","desired":false}"#).opted_in());
    }

    #[test]
    fn failed_refresh_of_a_working_pack_is_reported_separately() {
        let status = CodecPackStatus::parse(
            r#"{"state":"installed","mesa":"enabled","last_error":"dnf5 failed: offline"}"#,
        );
        assert_eq!(status.offered_actions(), vec![CodecAction::Remove]);
        assert_eq!(
            status.refresh_problem().as_deref(),
            Some("The last update of the codec pack failed: dnf5 failed: offline")
        );
    }

    #[test]
    fn error_text_is_bounded_and_printable() {
        let long = "x".repeat(1000);
        let status = CodecPackStatus::parse(&format!(
            r#"{{"state":"failed","last_error":"a\u001b[31m{long}"}}"#
        ));
        assert!(status.last_error.len() <= MAX_TEXT);
        assert!(!status.last_error.contains('\u{1b}'));
    }

    #[test]
    fn only_the_three_polkit_allowed_units() {
        assert_eq!(
            CodecAction::Install.unit(),
            "tilo-codec-pack@install.service"
        );
        assert_eq!(CodecAction::Remove.unit(), "tilo-codec-pack@remove.service");
        assert_eq!(
            CodecAction::Refresh.unit(),
            "tilo-codec-pack@refresh.service"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_watcher_reports_an_atomic_status_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        watch_dir(dir.path(), move || tx.send(()).is_ok()).unwrap();
        // The helper writes a temporary file and renames it over the status.
        let temporary = dir.path().join(".status.tmp");
        std::fs::write(&temporary, r#"{"state":"installing"}"#).unwrap();
        std::fs::write(dir.path().join("unrelated"), "x").unwrap();
        std::fs::rename(&temporary, dir.path().join("status.json")).unwrap();
        rx.recv_timeout(tilo_test_env::hang_bound())
            .expect("rename over status.json is reported");
        assert_eq!(
            CodecPackStatus::read_from(&dir.path().join("status.json")).state,
            PackState::Installing
        );
    }
}
