//! A private `PipeWire` graph for one Windows program's launch.
//!
//! A Windows program's sandbox must never receive the session's `PipeWire` or Pulse socket:
//! either one would make every source visible as well as the default sink. Each launch gets a
//! tiny private `PipeWire` daemon and its Pulse server instead, whose only graph objects are Pulse
//! tunnels Tilo creates:
//!
//! * an output tunnel is always present, to the session's Pulse server;
//! * an input tunnel is added only after `CapabilityPlan` has granted the canonical
//!   `Device(DeviceCap::Microphone)` capability.
//!
//! The compatibility service prepares the graph ([`AudioGraph::prepare`]: its directories and
//! configuration, which decide the microphone), and the launch starts it right before the sandbox
//! ([`AudioGraph::start`]), as children of the sandbox's own process. So the pair is in the
//! launch's group, stays while the desktop restarts in place as the program does, and ends when
//! the program ends, is quit, or its group is ended, whoever started it: each daemon dies with
//! the process that started it. Killing the two daemons is revocation: existing client
//! descriptors lose their peer, and a later launch gets fresh sockets and a fresh capability
//! decision.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

const PIPEWIRE_SHARE: &str = "/usr/share/pipewire";
const START_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_UNIX_SOCKET_PATH: usize = 100;

/// The two daemons a graph starts.
#[derive(Clone, Debug)]
pub struct Daemons {
    pub pipewire: PathBuf,
    pub pulse: PathBuf,
}

impl Daemons {
    /// The system's `pipewire` and `pipewire-pulse`.
    #[must_use]
    pub fn installed() -> Self {
        Self {
            pipewire: PathBuf::from("/usr/bin/pipewire"),
            pulse: PathBuf::from("/usr/bin/pipewire-pulse"),
        }
    }
}

/// A graph prepared in its own directory: the daemons' sockets and configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioGraph {
    root: PathBuf,
}

impl AudioGraph {
    /// Prepare a graph in `root`, which must not exist yet (its parent is made when missing), for
    /// the session of `uid`, with a microphone source only when `microphone`. `uid` is the
    /// session's effective uid, never one taken from a catalogue record or an application.
    ///
    /// # Errors
    /// `root` exists already ([`AudioGraphError::Taken`]), or could not be made private.
    pub fn prepare(root: PathBuf, uid: u32, microphone: bool) -> Result<Self, AudioGraphError> {
        if let Some(parent) = root.parent() {
            fs::create_dir_all(parent).map_err(AudioGraphError::Io)?;
        }
        match fs::create_dir(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(AudioGraphError::Taken(root));
            }
            Err(error) => return Err(AudioGraphError::Io(error)),
        }
        let graph = Self { root };
        let prepared = (|| {
            ensure_private_directory(&graph.root)?;
            ensure_private_directory(&graph.root.join("audio"))?;
            ensure_private_directory(&graph.root.join("audio/pulse"))?;
            prepare_config(&graph.root.join("pipewire-config"), uid, microphone)?;
            check_socket_path(&graph.pipewire_socket())?;
            check_socket_path(&graph.pulse_socket())
        })();
        match prepared {
            Ok(()) => Ok(graph),
            Err(error) => {
                graph.remove();
                Err(error)
            }
        }
    }

    /// The graph prepared in `root` before.
    #[must_use]
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn pipewire_socket(&self) -> PathBuf {
        self.root.join("audio/pipewire-0")
    }

    #[must_use]
    pub fn pulse_socket(&self) -> PathBuf {
        self.root.join("audio/pulse/native")
    }

    /// Whether the graph was started and has ended since: its `PipeWire` socket is there and
    /// nobody answers it.
    #[must_use]
    pub fn ended(&self) -> bool {
        let socket = self.pipewire_socket();
        fs::symlink_metadata(&socket).is_ok_and(|metadata| metadata.file_type().is_socket())
            && tilo_files::native::socket::connect(&socket)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
    }

    /// Remove the graph's directory.
    pub fn remove(self) {
        let _ = fs::remove_dir_all(&self.root);
    }

    /// Start the graph's daemons as this thread's children and wait for their sockets. Each dies
    /// when this thread's process ends, even when it becomes another program first (the sandbox
    /// the graph is for), and inherits none of its descriptors.
    ///
    /// # Errors
    /// A daemon could not start or made no socket in time; whatever started is ended.
    pub fn start(&self, daemons: &Daemons) -> Result<Started, AudioGraphError> {
        let audio = self.root.join("audio");
        let config = self.root.join("pipewire-config");
        let mut pipewire = spawn(&daemons.pipewire, &audio, &config, true)
            .map_err(AudioGraphError::SpawnPipeWire)?;
        if let Err(error) = wait_for_socket(&self.pipewire_socket(), &mut pipewire) {
            stop_child(&mut pipewire);
            return Err(error);
        }
        let mut pulse = match spawn(&daemons.pulse, &audio, &config, false) {
            Ok(process) => process,
            Err(error) => {
                stop_child(&mut pipewire);
                return Err(AudioGraphError::SpawnPulse(error));
            }
        };
        if let Err(error) = wait_for_socket(&self.pulse_socket(), &mut pulse) {
            stop_child(&mut pulse);
            stop_child(&mut pipewire);
            return Err(error);
        }
        Ok(Started { pipewire, pulse })
    }
}

/// A graph's two daemons, ended when this is dropped.
#[derive(Debug)]
pub struct Started {
    pipewire: Child,
    pulse: Child,
}

impl Started {
    /// The daemons' pids: `PipeWire`'s, then its Pulse server's.
    #[must_use]
    pub fn pids(&self) -> [u32; 2] {
        [self.pipewire.id(), self.pulse.id()]
    }

    /// Leave the daemons to end with this process, which goes on as the program they serve.
    pub fn with_this_process(self) {
        std::mem::forget(self);
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        stop_child(&mut self.pulse);
        stop_child(&mut self.pipewire);
    }
}

/// Start the graph in `root` with `daemons`, then become `command`, whose process the daemons
/// end with: a Windows program's sandbox, which binds the graph's sockets.
///
/// # Errors
/// The graph did not start, or `command` could not be executed.
pub fn exec_with(
    root: PathBuf,
    daemons: &Daemons,
    command: &[std::ffi::OsString],
) -> Result<std::convert::Infallible, AudioGraphError> {
    let Some((program, args)) = command.split_first() else {
        return Err(AudioGraphError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no command",
        )));
    };
    AudioGraph::at(root).start(daemons)?.with_this_process();
    // The descriptors the sandbox is handed stay open across the exec, as they did into this.
    Err(AudioGraphError::Io(Command::new(program).args(args).exec()))
}

fn spawn(program: &Path, runtime: &Path, config: &Path, core: bool) -> std::io::Result<Child> {
    let mut command = Command::new(program);
    // Do not inherit a caller-selected PipeWire/Pulse configuration or socket. The session's
    // endpoint is named only in the generated, mode-0600 drop-in.
    command.env_clear();
    for key in ["LANG", "LANGUAGE", "LC_ALL", "TZ"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("HOME", "/var/empty")
        .env("XDG_RUNTIME_DIR", runtime)
        .env("PIPEWIRE_RUNTIME_DIR", runtime)
        .env("PULSE_RUNTIME_PATH", runtime.join("pulse"))
        .env("PIPEWIRE_CONFIG_DIR", config)
        .env("PATH", "/usr/bin:/bin");
    if core {
        command
            .env("PIPEWIRE_DAEMON", "true")
            .env("PIPEWIRE_CORE", "pipewire-0");
    } else {
        command.env("PIPEWIRE_REMOTE", "pipewire-0");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: prctl and close_range are async-signal-safe and the closure captures nothing.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The sandbox's own descriptors (its filter, its prefix) stay with the sandbox.
            if libc::syscall(
                libc::SYS_close_range,
                3,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

fn prepare_config(config_root: &Path, uid: u32, microphone: bool) -> Result<(), AudioGraphError> {
    ensure_private_directory(config_root)?;
    copy_base_config(config_root, "pipewire.conf")?;
    copy_base_config(config_root, "pipewire-pulse.conf")?;
    let pipewire_dropin = config_root.join("pipewire.conf.d/90-tilo-windows-proxy.conf");
    let pulse_dropin = config_root.join("pipewire-pulse.conf.d/90-tilo-windows-proxy.conf");
    ensure_private_directory(&config_root.join("pipewire.conf.d"))?;
    ensure_private_directory(&config_root.join("pipewire-pulse.conf.d"))?;

    let session_pulse = format!("unix:/run/user/{uid}/pulse/native");
    write_private_file(
        &pipewire_dropin,
        &pipewire_dropin_contents(&session_pulse, microphone),
    )?;
    write_private_file(&pulse_dropin, pulse_dropin_contents())?;
    Ok(())
}

fn pulse_dropin_contents() -> &'static str {
    "# Generated by Tilo. The capsule cannot ask this broker to load host modules.\n\
     pulse.properties = {\n\
       pulse.allow-module-loading = false\n\
     }\n"
}

fn copy_base_config(config_root: &Path, file: &str) -> Result<(), AudioGraphError> {
    let source = Path::new(PIPEWIRE_SHARE).join(file);
    let metadata = fs::symlink_metadata(&source).map_err(AudioGraphError::BaseConfig)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AudioGraphError::UnsafeBaseConfig(source));
    }
    fs::copy(&source, config_root.join(file)).map_err(AudioGraphError::BaseConfig)?;
    fs::set_permissions(config_root.join(file), fs::Permissions::from_mode(0o600))
        .map_err(AudioGraphError::BaseConfig)
}

fn pipewire_dropin_contents(session_pulse: &str, microphone: bool) -> String {
    let mut contents = format!(
        "# Generated by Tilo. This private graph has no device discovery.\n\
         context.properties = {{\n  core.daemon = true\n  core.name = pipewire-0\n}}\n\
         context.modules = [\n\
         {{ name = libpipewire-module-pulse-tunnel\n  flags = [ ifexists nofail ]\n  args = {{\n\
         \ttunnel.mode = sink\n\
         \tpulse.server.address = \"{session_pulse}\"\n\
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
             \tpulse.server.address = \"{session_pulse}\"\n\
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

fn ensure_private_directory(path: &Path) -> Result<(), AudioGraphError> {
    fs::create_dir_all(path).map_err(AudioGraphError::Io)?;
    let metadata = fs::symlink_metadata(path).map_err(AudioGraphError::Io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AudioGraphError::UnsafePath(path.to_path_buf()));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(AudioGraphError::Io)
}

fn write_private_file(path: &Path, contents: &str) -> Result<(), AudioGraphError> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(AudioGraphError::Io)?;
    file.write_all(contents.as_bytes())
        .map_err(AudioGraphError::Io)?;
    file.sync_all().map_err(AudioGraphError::Io)
}

fn check_socket_path(path: &Path) -> Result<(), AudioGraphError> {
    if path.as_os_str().as_encoded_bytes().len() > MAX_UNIX_SOCKET_PATH {
        return Err(AudioGraphError::SocketPathTooLong(path.to_path_buf()));
    }
    Ok(())
}

fn wait_for_socket(path: &Path, child: &mut Child) -> Result<(), AudioGraphError> {
    let deadline = Instant::now()
        .checked_add(START_TIMEOUT)
        .ok_or_else(|| AudioGraphError::StartTimedOut(path.to_path_buf()))?;
    loop {
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()) {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(AudioGraphError::Io)? {
            return Err(AudioGraphError::Exited(status.code()));
        }
        if Instant::now() >= deadline {
            return Err(AudioGraphError::StartTimedOut(path.to_path_buf()));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[derive(Debug, Error)]
pub enum AudioGraphError {
    #[error("could not prepare private PipeWire directory: {0}")]
    Io(std::io::Error),
    #[error("a private audio graph is in {0} already")]
    Taken(PathBuf),
    #[error("Tilo PipeWire base configuration is unavailable: {0}")]
    BaseConfig(std::io::Error),
    #[error("Tilo PipeWire base configuration is not a regular file: {0}")]
    UnsafeBaseConfig(PathBuf),
    #[error("private audio graph path is unsafe: {0}")]
    UnsafePath(PathBuf),
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
        fn installed_pipewire_accepts_the_generated_private_graph() {
            let daemons = Daemons::installed();
            if !daemons.pipewire.is_file()
                || !daemons.pulse.is_file()
                || !Path::new(PIPEWIRE_SHARE).is_dir()
            {
                return;
            }
            let directory = tempfile::tempdir().unwrap();
            let uid = rustix::process::geteuid().as_raw();
            let graph = AudioGraph::prepare(directory.path().join("lease"), uid, false).unwrap();
            let started = graph.start(&daemons).unwrap();
            assert!(
                fs::symlink_metadata(graph.pipewire_socket())
                    .is_ok_and(|metadata| metadata.file_type().is_socket())
            );
            assert!(
                fs::symlink_metadata(graph.pulse_socket())
                    .is_ok_and(|metadata| metadata.file_type().is_socket())
            );
            drop(started);
            graph.remove();
        }
        {
            let config = pipewire_dropin_contents("unix:/run/user/1000/pulse/native", false);
            assert!(config.contains("tunnel.mode = sink"));
            assert!(!config.contains("tunnel.mode = source"));
            assert!(config.contains("no WirePlumber or device-discovery"));
        }
        {
            let config = pipewire_dropin_contents("unix:/run/user/1000/pulse/native", true);
            assert!(config.contains("tunnel.mode = sink"));
            assert!(config.contains("tunnel.mode = source"));
            assert!(config.contains("tilo.windows.compat.microphone"));
        }
        {
            assert!(pulse_dropin_contents().contains("pulse.allow-module-loading = false"));
        }
        installed_pipewire_accepts_the_generated_private_graph();
    }

    #[test]
    fn a_graph_directory_is_never_taken_over() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("lease");
        fs::create_dir(&root).unwrap();
        assert!(matches!(
            AudioGraph::prepare(root.clone(), 1000, false),
            Err(AudioGraphError::Taken(taken)) if taken == root
        ));
    }
}
