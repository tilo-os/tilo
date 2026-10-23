//! Members' legacy apps on this KVM worker, through `tilo-legacy`.
//!
//! Each member has their own legacy VM, kept in their area on the worker
//! exactly as a device keeps a user's: `tilo-legacy` creates it from the
//! guest image, wakes it on a launch and puts it to sleep when idle. A
//! launch runs `tilo-legacy relay-stream`, which shows the app's windows on
//! the member's headless Tilo shell and writes them as encoded
//! [`LegacyFrame`]s on its stdout; [`WindowInput`]s for them go to its
//! stdin. The process's exit ends the launch, and closing its stdin (the
//! member's stream went away) ends the app.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use tilo_relay_protocol::config::LegacyAppsConfig;
use tilo_relay_protocol::environment::{
    LEGACY_PIPE_FRAME_BYTES, LaunchId, LegacyFrame, LegacyGuestOs, LegacyLaunch, Refusal,
    WindowInput,
};
use tokio::sync::mpsc;
use wire_frame::Framing;

use crate::environment_host::{LaunchError, MemberArea, MemberContext};
use crate::legacy_host::{LegacyApp, LegacyVmHost};

const FRAMING: Framing = Framing::little_endian(LEGACY_PIPE_FRAME_BYTES);
const FRAME_QUEUE: usize = 256;
/// The worker's graphics and logging setup, which the headless shell's
/// renderer needs; nothing else of the worker's environment is passed on.
const PASSED_THROUGH: &[&str] = &[
    "WGPU_BACKEND",
    "WGPU_ADAPTER_NAME",
    "VK_DRIVER_FILES",
    "VK_ICD_FILENAMES",
    "RUST_LOG",
];
const INPUT_QUEUE: usize = 256;

/// [`LegacyVmHost`] over `tilo-legacy`.
#[derive(Clone, Debug)]
pub struct TiloLegacyHost {
    config: LegacyAppsConfig,
}

impl TiloLegacyHost {
    /// A host running members' apps as `config` says.
    #[must_use]
    pub fn new(config: LegacyAppsConfig) -> Self {
        Self { config }
    }

    /// Where a member's legacy VM and headless shell keep their files.
    #[must_use]
    pub fn member_home(area: &MemberArea) -> PathBuf {
        area.root().join("legacy")
    }

    /// Where a member's VM and headless shell keep their sockets. Unix socket
    /// paths are short (108 bytes), and the deepest ones sit several levels
    /// below this, so it is a short private directory beside the system's
    /// temporary files rather than inside the member's area.
    #[must_use]
    pub fn member_runtime(area: &MemberArea) -> PathBuf {
        let digest = blake3::hash(area.root().as_os_str().as_encoded_bytes());
        std::env::temp_dir().join(format!(
            "tilo-legacy-{}",
            hex::encode(digest.as_bytes().get(..8).unwrap_or_default())
        ))
    }

    /// `tilo-legacy` with the member's own directories as its whole
    /// environment.
    #[must_use]
    pub fn tilo_legacy(&self, area: &MemberArea) -> Command {
        let home = Self::member_home(area);
        let mut command = Command::new(&self.config.tilo_legacy_binary);
        if let Some(image) = &self.config.guest_image {
            command.arg("--image").arg(image);
        }
        if let Some(idle) = self.config.idle_suspend_secs {
            command.arg("--idle-suspend").arg(idle.to_string());
        }
        command
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin")),
            )
            .env("HOME", &home)
            .env("XDG_DATA_HOME", home.join("data"))
            // libvirt's session daemon keeps its QEMU monitor sockets below
            // the config directory: reach the durable one by a short link.
            .env("XDG_CONFIG_HOME", Self::member_runtime(area).join("config"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("XDG_RUNTIME_DIR", Self::member_runtime(area))
            .envs(
                PASSED_THROUGH
                    .iter()
                    .filter_map(|name| Some((*name, std::env::var_os(name)?))),
            );
        command
    }

    /// The `relay-stream` command for one launch.
    #[must_use]
    pub fn command(&self, area: &MemberArea, request: &LegacyLaunch) -> Command {
        let mut command = self.tilo_legacy(area);
        command
            .arg("relay-stream")
            .arg("--shell")
            .arg(&self.config.shell_binary)
            .arg("--")
            .arg(&request.app)
            .args(&request.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command
    }
}

impl LegacyVmHost for TiloLegacyHost {
    fn launch(
        &self,
        member: &MemberContext,
        _launch: LaunchId,
        request: &LegacyLaunch,
    ) -> Result<LegacyApp, LaunchError> {
        if request.guest != LegacyGuestOs::Linux {
            return Err(LaunchError::Failed(
                "this worker runs Linux legacy apps only".into(),
            ));
        }
        let home = Self::member_home(&member.area);
        let runtime = Self::member_runtime(&member.area);
        let config = home.join("config");
        for dir in [&home, &runtime, &config] {
            create_private_dir(dir)
                .map_err(|error| LaunchError::Failed(format!("{}: {error}", dir.display())))?;
        }
        link_config(&runtime.join("config"), &config)
            .map_err(|error| LaunchError::Failed(format!("the config link: {error}")))?;
        let mut child = self
            .command(&member.area, request)
            .spawn()
            .map_err(|error| LaunchError::Failed(format!("start tilo-legacy: {error}")))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(LaunchError::Failed("tilo-legacy has no pipes".into()));
        };
        let (frames_tx, frames) = mpsc::channel(FRAME_QUEUE);
        let (input, mut input_rx) = mpsc::channel::<WindowInput>(INPUT_QUEUE);
        std::thread::spawn(move || read_frames(child, stdout, &frames_tx));
        std::thread::spawn(move || {
            let mut stdin = stdin;
            while let Some(event) = input_rx.blocking_recv() {
                if FRAMING.write_value(&mut stdin, &event).is_err()
                    || std::io::Write::flush(&mut stdin).is_err()
                {
                    return;
                }
            }
        });
        Ok(LegacyApp { frames, input })
    }
}

fn read_frames(
    mut child: Child,
    mut stdout: std::process::ChildStdout,
    frames: &mpsc::Sender<LegacyFrame>,
) {
    let mut ended = false;
    while let Ok(Some(frame)) = FRAMING.read_value::<_, LegacyFrame>(&mut stdout) {
        ended = matches!(
            frame,
            LegacyFrame::Exited { .. } | LegacyFrame::Refused { .. }
        );
        if frames.blocking_send(frame).is_err() {
            // Nobody watches any more: the app goes with the stream.
            let _ = child.kill();
            break;
        }
    }
    let status = child.wait().ok();
    if !ended {
        let _ = frames.blocking_send(LegacyFrame::Refused {
            reason: Refusal::LaunchFailed,
            detail: format!(
                "tilo-legacy ended without the app's exit ({})",
                status.map_or_else(|| "unknown".into(), |status| status.to_string())
            ),
        });
    }
}

/// `link` → `target`, replacing whatever `link` was.
fn link_config(link: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    if std::fs::read_link(link).is_ok_and(|current| current == target) {
        return Ok(());
    }
    let _ = std::fs::remove_file(link);
    std::os::unix::fs::symlink(target, link)
}

/// `dir`, private to this process's user; one someone else made first (in a
/// shared temporary directory) is refused.
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    std::fs::create_dir_all(dir)?;
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.is_dir() || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "not a directory of this worker's",
        ));
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_runs_in_the_members_own_directories() {
        let host = TiloLegacyHost::new(LegacyAppsConfig {
            guest_image: Some("/srv/guest.qcow2".into()),
            idle_suspend_secs: Some(60),
            ..LegacyAppsConfig::default()
        });
        let area = MemberArea::of(std::path::Path::new("/srv/relay"), &[7; 32]);
        let command = host.command(
            &area,
            &LegacyLaunch {
                guest: LegacyGuestOs::Linux,
                app: "foot".into(),
                args: vec!["-e".into(), "sh".into()],
                cwd: "/home/you".into(),
            },
        );
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--image",
                "/srv/guest.qcow2",
                "--idle-suspend",
                "60",
                "relay-stream",
                "--shell",
                "/usr/bin/shell",
                "--",
                "foot",
                "-e",
                "sh"
            ]
        );
        let env: std::collections::BTreeMap<String, String> = command
            .get_envs()
            .filter_map(|(key, value)| {
                Some((
                    key.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        let home = TiloLegacyHost::member_home(&area);
        assert_eq!(env.get("HOME"), Some(&home.display().to_string()));
        let runtime = TiloLegacyHost::member_runtime(&area);
        assert_eq!(
            env.get("XDG_RUNTIME_DIR"),
            Some(&runtime.display().to_string())
        );
        let deepest = runtime.join("tilo-legacy/linux/vsock.sock_65535");
        assert!(deepest.as_os_str().len() < 100, "{}", deepest.display());
        assert!(
            !env.contains_key("SSH_AUTH_SOCK"),
            "nothing of the worker's leaks in"
        );
    }
}
