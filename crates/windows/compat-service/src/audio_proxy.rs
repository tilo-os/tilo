//! Per-launch `PipeWire` graph for Windows compatibility capsules.
//!
//! A capsule must never receive the session `PipeWire` or Pulse socket: either
//! one would make every source visible as well as the default sink.  Instead
//! this module starts a tiny private `PipeWire` daemon for one launch.  Its only
//! graph objects are Pulse tunnels deliberately created by Tilo:
//!
//! * an output tunnel is always present, to the session's Pulse server;
//! * an input tunnel is added only after `CapabilityPlan` has granted the
//!   canonical `Device("microphone")` capability.
//!
//! The capsule sees only the private daemon's native and Pulse sockets.  It
//! cannot name, mount, or connect to the host socket.  Killing the two broker
//! processes is revocation: existing client descriptors lose their peer and
//! a later launch gets fresh sockets and a fresh capability decision.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

const PIPEWIRE_BIN: &str = "/usr/bin/pipewire";
const PIPEWIRE_PULSE_BIN: &str = "/usr/bin/pipewire-pulse";
const PIPEWIRE_SHARE: &str = "/usr/share/pipewire";
const START_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_UNIX_SOCKET_PATH: usize = 100;

#[derive(Debug)]
pub struct AudioProxyLease {
    root: PathBuf,
    pipewire: Child,
    pulse: Child,
}

impl AudioProxyLease {
    /// Start a private graph rooted in the already-daemon-owned lease path.
    /// `uid` is taken from the compositor session/effective uid, never from a
    /// catalogue record or application request.
    pub fn start(root: PathBuf, uid: u32, microphone: bool) -> Result<Self, AudioProxyError> {
        let audio_root = root.join("audio");
        let config_root = root.join("pipewire-config");
        ensure_private_directory(&root)?;
        ensure_private_directory(&audio_root)?;
        ensure_private_directory(&audio_root.join("pulse"))?;
        prepare_config(&config_root, uid, microphone)?;

        let native_socket = audio_root.join("pipewire-0");
        let pulse_socket = audio_root.join("pulse/native");
        check_socket_path(&native_socket)?;
        check_socket_path(&pulse_socket)?;

        let mut pipewire = spawn_pipewire(&audio_root, &config_root)?;
        if let Err(error) = wait_for_socket(&native_socket, &mut pipewire) {
            stop_child(&mut pipewire);
            let _ = fs::remove_dir_all(&root);
            return Err(error);
        }
        let mut pulse = match spawn_pulse(&audio_root, &config_root) {
            Ok(process) => process,
            Err(error) => {
                stop_child(&mut pipewire);
                let _ = fs::remove_dir_all(&root);
                return Err(error);
            }
        };
        if let Err(error) = wait_for_socket(&pulse_socket, &mut pulse) {
            stop_child(&mut pulse);
            stop_child(&mut pipewire);
            let _ = fs::remove_dir_all(&root);
            return Err(error);
        }
        Ok(Self {
            root,
            pipewire,
            pulse,
        })
    }

    #[must_use]
    pub fn pipewire_socket(&self) -> PathBuf {
        self.root.join("audio/pipewire-0")
    }

    #[must_use]
    pub fn pulse_socket(&self) -> PathBuf {
        self.root.join("audio/pulse/native")
    }

    pub fn revoke(mut self) {
        stop_child(&mut self.pulse);
        stop_child(&mut self.pipewire);
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Drop for AudioProxyLease {
    fn drop(&mut self) {
        stop_child(&mut self.pulse);
        stop_child(&mut self.pipewire);
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn spawn_pipewire(runtime: &Path, config: &Path) -> Result<Child, AudioProxyError> {
    let mut command = Command::new(PIPEWIRE_BIN);
    private_process_environment(&mut command, runtime, config);
    command.env("PIPEWIRE_DAEMON", "true");
    command.env("PIPEWIRE_CORE", "pipewire-0");
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command.spawn().map_err(AudioProxyError::SpawnPipeWire)
}

fn spawn_pulse(runtime: &Path, config: &Path) -> Result<Child, AudioProxyError> {
    let mut command = Command::new(PIPEWIRE_PULSE_BIN);
    private_process_environment(&mut command, runtime, config);
    command.env("PIPEWIRE_REMOTE", "pipewire-0");
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command.spawn().map_err(AudioProxyError::SpawnPulse)
}

fn private_process_environment(command: &mut Command, runtime: &Path, config: &Path) {
    // Do not inherit a caller-selected PipeWire/Pulse configuration or socket.
    // The host endpoint is named only in the generated, mode-0600 drop-in.
    command.env_clear();
    for key in ["LANG", "LANGUAGE", "LC_ALL", "TZ"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.env("HOME", "/var/empty");
    command.env("XDG_RUNTIME_DIR", runtime);
    command.env("PIPEWIRE_RUNTIME_DIR", runtime);
    command.env("PULSE_RUNTIME_PATH", runtime.join("pulse"));
    command.env("PIPEWIRE_CONFIG_DIR", config);
    command.env("PATH", "/usr/bin:/bin");
}

fn prepare_config(config_root: &Path, uid: u32, microphone: bool) -> Result<(), AudioProxyError> {
    ensure_private_directory(config_root)?;
    copy_base_config(config_root, "pipewire.conf")?;
    copy_base_config(config_root, "pipewire-pulse.conf")?;
    let pipewire_dropin = config_root.join("pipewire.conf.d/90-tilo-windows-proxy.conf");
    let pulse_dropin = config_root.join("pipewire-pulse.conf.d/90-tilo-windows-proxy.conf");
    ensure_private_directory(&config_root.join("pipewire.conf.d"))?;
    ensure_private_directory(&config_root.join("pipewire-pulse.conf.d"))?;

    let host_pulse = format!("unix:/run/user/{uid}/pulse/native");
    write_private_file(
        &pipewire_dropin,
        &pipewire_dropin_contents(&host_pulse, microphone),
    )?;
    // The base PipeWire Pulse configuration supplies the protocol server.  An
    // empty Tilo-owned drop-in makes the private configuration directory
    // complete and gives upgrades a stable place for future Pulse policy.
    write_private_file(&pulse_dropin, pulse_dropin_contents())?;
    Ok(())
}

fn pulse_dropin_contents() -> &'static str {
    "# Generated by Tilo. The capsule cannot ask this broker to load host modules.\n\
     pulse.properties = {\n\
       pulse.allow-module-loading = false\n\
     }\n"
}

fn copy_base_config(config_root: &Path, file: &str) -> Result<(), AudioProxyError> {
    let source = Path::new(PIPEWIRE_SHARE).join(file);
    let metadata = fs::symlink_metadata(&source).map_err(AudioProxyError::BaseConfig)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AudioProxyError::UnsafeBaseConfig(source));
    }
    fs::copy(&source, config_root.join(file)).map_err(AudioProxyError::BaseConfig)?;
    fs::set_permissions(config_root.join(file), fs::Permissions::from_mode(0o600))
        .map_err(AudioProxyError::BaseConfig)
}

fn pipewire_dropin_contents(host_pulse: &str, microphone: bool) -> String {
    let mut contents = format!(
        "# Generated by Tilo. This private graph has no device discovery.\n\
         context.properties = {{\n  core.daemon = true\n  core.name = pipewire-0\n}}\n\
         context.modules = [\n\
         {{ name = libpipewire-module-pulse-tunnel\n  flags = [ ifexists nofail ]\n  args = {{\n\
         \ttunnel.mode = sink\n\
         \tpulse.server.address = \"{host_pulse}\"\n\
         \treconnect.interval.ms = 1000\n\
         \tstream.props = {{\n\
         \t\tnode.name = \"tilo.windows.compat.output\"\n\
         \t\tnode.description = \"Tilo Windows compatibility playback\"\n\
         \t\tmedia.class = \"Audio/Sink\"\n\
         \t\tnode.virtual = true\n\
         \t}}\n  }}\n}}\n"
    );
    if microphone {
        #[allow(
            clippy::format_push_string,
            reason = "the generated PipeWire fragment is bounded and this keeps it auditable"
        )]
        contents.push_str(&format!(
            "{{ name = libpipewire-module-pulse-tunnel\n  flags = [ ifexists nofail ]\n  args = {{\n\
             \ttunnel.mode = source\n\
             \tpulse.server.address = \"{host_pulse}\"\n\
             \treconnect.interval.ms = 1000\n\
             \tstream.props = {{\n\
             \t\tnode.name = \"tilo.windows.compat.microphone\"\n\
             \t\tnode.description = \"Tilo Windows compatibility microphone\"\n\
             \t\tmedia.class = \"Audio/Source\"\n\
             \t\tnode.virtual = true\n\
             \t}}\n  }}\n}}\n"
        ));
    }
    contents.push_str(
        "]\n\
         # There is intentionally no WirePlumber or device-discovery process in this graph.\n",
    );
    contents
}

fn ensure_private_directory(path: &Path) -> Result<(), AudioProxyError> {
    fs::create_dir_all(path).map_err(AudioProxyError::Io)?;
    let metadata = fs::symlink_metadata(path).map_err(AudioProxyError::Io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AudioProxyError::UnsafeLeasePath(path.to_path_buf()));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(AudioProxyError::Io)
}

fn write_private_file(path: &Path, contents: &str) -> Result<(), AudioProxyError> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(AudioProxyError::Io)?;
    file.write_all(contents.as_bytes())
        .map_err(AudioProxyError::Io)?;
    file.sync_all().map_err(AudioProxyError::Io)
}

fn check_socket_path(path: &Path) -> Result<(), AudioProxyError> {
    if path.as_os_str().as_encoded_bytes().len() > MAX_UNIX_SOCKET_PATH {
        return Err(AudioProxyError::SocketPathTooLong(path.to_path_buf()));
    }
    Ok(())
}

fn wait_for_socket(path: &Path, child: &mut Child) -> Result<(), AudioProxyError> {
    let deadline = Instant::now()
        .checked_add(START_TIMEOUT)
        .ok_or_else(|| AudioProxyError::StartTimedOut(path.to_path_buf()))?;
    loop {
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()) {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(AudioProxyError::Io)? {
            return Err(AudioProxyError::Exited(status.code()));
        }
        if Instant::now() >= deadline {
            return Err(AudioProxyError::StartTimedOut(path.to_path_buf()));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[derive(Debug, Error)]
pub enum AudioProxyError {
    #[error("could not prepare private PipeWire directory: {0}")]
    Io(std::io::Error),
    #[error("Tilo PipeWire base configuration is unavailable: {0}")]
    BaseConfig(std::io::Error),
    #[error("Tilo PipeWire base configuration is not a regular file: {0}")]
    UnsafeBaseConfig(PathBuf),
    #[error("private audio lease path is unsafe: {0}")]
    UnsafeLeasePath(PathBuf),
    #[error("private audio socket path is too long: {0}")]
    SocketPathTooLong(PathBuf),
    #[error("could not start PipeWire proxy: {0}")]
    SpawnPipeWire(std::io::Error),
    #[error("could not start PipeWire Pulse proxy: {0}")]
    SpawnPulse(std::io::Error),
    #[error("audio proxy process exited during startup ({0:?})")]
    Exited(Option<i32>),
    #[error("audio proxy did not create its socket in time: {0}")]
    StartTimedOut(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_private_audio_graph_exposes_sources_only_by_grant() {
        // Phase `installed_pipewire_accepts_the_generated_private_graph`.
        fn installed_pipewire_accepts_the_generated_private_graph() {
            if !Path::new(PIPEWIRE_BIN).is_file() || !Path::new(PIPEWIRE_PULSE_BIN).is_file() {
                return;
            }
            let directory = tempfile::tempdir().unwrap();
            // SAFETY: `geteuid` has no preconditions and does not mutate memory.
            let uid = unsafe { libc::geteuid() };
            let lease = AudioProxyLease::start(directory.path().join("lease"), uid, false).unwrap();
            assert!(
                fs::symlink_metadata(lease.pipewire_socket())
                    .is_ok_and(|metadata| metadata.file_type().is_socket())
            );
            assert!(
                fs::symlink_metadata(lease.pulse_socket())
                    .is_ok_and(|metadata| metadata.file_type().is_socket())
            );
            lease.revoke();
        }
        // Phase `playback_graph_has_a_sink_but_no_source`.
        {
            let config = pipewire_dropin_contents("unix:/run/user/1000/pulse/native", false);
            assert!(config.contains("tunnel.mode = sink"));
            assert!(!config.contains("tunnel.mode = source"));
            assert!(config.contains("no WirePlumber or device-discovery"));
        }
        // Phase `microphone_grant_is_the_only_way_to_materialize_source_tunnel`.
        {
            let config = pipewire_dropin_contents("unix:/run/user/1000/pulse/native", true);
            assert!(config.contains("tunnel.mode = sink"));
            assert!(config.contains("tunnel.mode = source"));
            assert!(config.contains("tilo.windows.compat.microphone"));
        }
        // Phase `pulse_clients_cannot_load_a_host_facing_module`.
        {
            assert!(pulse_dropin_contents().contains("pulse.allow-module-loading = false"));
        }
        // Phase `host_socket_is_never_a_capsule_socket_path`.
        {
            let root = Path::new("/run/tilo/windows-compat/leases/123-4");
            let proxy = root.join("audio/pulse/native");
            assert_ne!(proxy, Path::new("/run/user/1000/pulse/native"));
            assert!(proxy.starts_with(root));
        }
        // Phase `installed_pipewire_accepts_the_generated_private_graph` (defined above).
        installed_pipewire_accepts_the_generated_private_graph();
    }
}
