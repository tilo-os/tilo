//! `--check` for the desktop and the headless runtime: start it, ask it something, stop it, and
//! prove it reached nothing of the desktop it runs beside.
//!
//! The proof is a set of traps: listening sockets where a host's session bus, system bus and
//! systemd sockets would be, named by the variables a program reads them from and, where this
//! machine has them, mounted over their fixed paths (`/run/dbus/system_bus_socket`,
//! `/run/systemd/private`, `/run/user/UID/bus`) for the session's processes alone. Any connection
//! to a trap fails the check. The desktop's window opens on a throwaway headless Tilo session
//! standing in for the host desktop, so the windowed session is checked without a display.

use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::session::{self, Mode, Options};
use crate::{json, seconds, target_dir};

/// One trap: a listening socket, and who connected to it.
struct Trap {
    name: String,
    hits: Arc<AtomicUsize>,
}

/// The traps and the directory they live in, removed afterwards.
struct Traps {
    dir: PathBuf,
    /// The host runtime directory the session is handed as `XDG_RUNTIME_DIR`.
    runtime: PathBuf,
    /// The session bus and system bus addresses the session is handed.
    session_bus: PathBuf,
    system_bus: PathBuf,
    all: Vec<Trap>,
    /// `bwrap` arguments that mount traps over the fixed paths this machine has.
    mounts: Vec<String>,
}

impl Traps {
    fn open(dir: &Path) -> Result<Self, String> {
        let _ = std::fs::remove_dir_all(dir);
        let runtime = dir.join("r");
        std::fs::create_dir_all(runtime.join("systemd")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(dir.join("s")).map_err(|e| e.to_string())?;
        let mut traps = Traps {
            dir: dir.to_path_buf(),
            runtime: runtime.clone(),
            session_bus: runtime.join("bus"),
            system_bus: dir.join("s/system_bus_socket"),
            all: Vec::new(),
            mounts: Vec::new(),
        };
        traps.trap("the host's session bus", &runtime.join("bus"))?;
        traps.trap(
            "the host's systemd user manager",
            &runtime.join("systemd/private"),
        )?;
        traps.trap("the system bus", &dir.join("s/system_bus_socket"))?;
        traps.trap("systemd", &dir.join("s/systemd-private"))?;
        traps.trap("the host's login bus", &dir.join("s/user-bus"))?;
        // The fixed paths a program reaches without asking any variable: mounted over, where this
        // machine has them, for the session's processes only.
        let uid = std::fs::metadata("/proc/self")
            .map_or(0, |meta| std::os::unix::fs::MetadataExt::uid(&meta));
        for (fixed, trap) in [
            ("/run/dbus/system_bus_socket", "s/system_bus_socket"),
            ("/var/run/dbus/system_bus_socket", "s/system_bus_socket"),
            ("/run/systemd/private", "s/systemd-private"),
            (&format!("/run/user/{uid}/bus") as &str, "s/user-bus"),
        ] {
            if Path::new(fixed).exists() {
                traps.mounts.extend([
                    "--bind".to_owned(),
                    dir.join(trap).display().to_string(),
                    fixed.to_owned(),
                ]);
            }
        }
        Ok(traps)
    }

    fn trap(&mut self, name: &str, path: &Path) -> Result<(), String> {
        let listener =
            UnixListener::bind(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                counted.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        self.all.push(Trap {
            name: name.to_owned(),
            hits,
        });
        Ok(())
    }

    /// What reached a trap, one line each.
    fn sprung(&self) -> Vec<String> {
        self.all
            .iter()
            .filter_map(|trap| {
                let hits = trap.hits.load(Ordering::SeqCst);
                (hits > 0).then(|| format!("{hits} connection(s) to {}", trap.name))
            })
            .collect()
    }
}

impl Drop for Traps {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A short directory for the check's sockets: a socket path holds 108 bytes, and the session's
/// deepest sit 56 bytes below its host's runtime directory (`tilo-nested-HASH/tilo/services/…`).
fn socket_dir() -> PathBuf {
    std::env::temp_dir().join(format!("tl{}", std::process::id()))
}

/// Stop the process group `child` leads, and wait for it.
pub fn stop_group(child: &mut Child) {
    let group = format!("-{}", child.id());
    let _ = Command::new("kill")
        .args(["-TERM", "--", &group])
        .stderr(Stdio::null())
        .status();
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(20) {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = Command::new("kill")
        .args(["-KILL", "--", &group])
        .stderr(Stdio::null())
        .status();
    let _ = child.wait();
}

/// Mesa's CPU Vulkan driver, for the stand-in host desktop.
fn lavapipe() -> Option<PathBuf> {
    let arch = std::env::consts::ARCH;
    [
        "/etc/vulkan/icd.d",
        "/usr/share/vulkan/icd.d",
        "/usr/local/share/vulkan/icd.d",
    ]
    .iter()
    .flat_map(|dir| {
        [
            Path::new(dir).join(format!("lvp_icd.{arch}.json")),
            Path::new(dir).join("lvp_icd.json"),
        ]
    })
    .find(|path| path.is_file())
}

/// A throwaway headless Tilo session standing in for the desktop a developer runs `cargo desktop`
/// on: its Wayland socket is where the nested window opens.
struct HostDesktop {
    child: Child,
    display: PathBuf,
    control: PathBuf,
    runtime: PathBuf,
}

impl HostDesktop {
    fn start(bin: &Path, home: &Path, runtime: &Path) -> Result<Self, String> {
        let _ = std::fs::remove_dir_all(home);
        for dir in ["config", "data", "state", "cache"] {
            std::fs::create_dir_all(home.join(dir)).map_err(|e| e.to_string())?;
        }
        std::fs::create_dir_all(runtime).map_err(|e| e.to_string())?;
        set_private(runtime)?;
        let control = runtime.join("control.sock");
        let mut command = Command::new(bin.join("tilo-session"));
        command
            .args(["--backend", "headless", "--size", "1600x1000"])
            .args(["--session-bus", "none", "--system-bus", "none", "--no-x11"])
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("TILO_IDENTITY_ROOT", home.join("identity"))
            .env("XDG_RUNTIME_DIR", runtime)
            .env("TILO_TEST_CONTROL", &control)
            .env("TILO_DMABUF_DEVICE", "")
            .env("RUST_LOG", "warn")
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
            .env("DBUS_SYSTEM_BUS_ADDRESS", "unix:path=/nonexistent")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdin(Stdio::null())
            .stdout(file(&home.join("host-desktop.log"))?)
            .stderr(file(&home.join("host-desktop.log"))?)
            .process_group(0);
        if let Some(icd) = lavapipe() {
            command
                .env("VK_DRIVER_FILES", &icd)
                .env("VK_ICD_FILENAMES", &icd)
                .env("WGPU_ADAPTER_NAME", "llvmpipe");
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start the stand-in host desktop: {error}"))?;
        let began = Instant::now();
        let display = loop {
            let found = std::fs::read_dir(runtime).ok().and_then(|entries| {
                entries.flatten().map(|entry| entry.path()).find(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("wayland-")
                                && Path::new(name)
                                    .extension()
                                    .is_none_or(|ext| !ext.eq_ignore_ascii_case("lock"))
                        })
                })
            });
            if let Some(display) = found {
                break display;
            }
            if child.try_wait().ok().flatten().is_some()
                || began.elapsed() > Duration::from_secs(90)
            {
                stop_group(&mut child);
                return Err(format!(
                    "the stand-in host desktop opened no Wayland socket (see {})",
                    home.join("host-desktop.log").display()
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        Ok(Self {
            child,
            display,
            control,
            runtime: runtime.to_path_buf(),
        })
    }
}

fn set_private(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

fn file(path: &Path) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// `tilo-control ARGS` against the session's control socket; its output.
fn control(bin: &Path, socket: &str, runtime: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(Path::new(bin).join("tilo-control"))
        .args(args)
        .env("TILO_TEST_CONTROL", socket)
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .map_err(|error| format!("tilo-control: {error}"))?;
    if !out.status.success() {
        return Err(format!(
            "tilo-control {} failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The smoke test for `cargo desktop --check` and `cargo headless --check`.
pub fn session(repo: &Path, asked: &Options) -> Result<ExitCode, String> {
    let began = Instant::now();
    let label = match asked.mode {
        Mode::Desktop => "desktop",
        Mode::Headless => "headless",
    };
    let mut timings = Vec::new();
    if asked.build {
        build(repo, asked)?;
        timings.push(format!("build {}", seconds(began.elapsed())));
    }
    let bin = session::build_dir(repo).join("bin");
    if !bin.join("tilo-session").is_file() {
        return Err(format!(
            "no build in {} (run without --no-build)",
            bin.display()
        ));
    }
    // The check's own state, in the target directory rather than the developer's.
    let mut options = Options {
        mode: asked.mode,
        state: target_dir(repo).join(format!("tilo-local-check/{label}")),
        build: false,
        check: true,
        ready: None,
        apps: asked.apps.clone(),
        script: asked.script.clone(),
        session: asked.session.clone(),
    };
    if !options.script.iter().any(|arg| arg == "--software") {
        options.script.push("--software".into());
    }
    let sockets = socket_dir();
    let traps = Traps::open(&sockets)?;
    let ready = sockets.join("ready.json");
    let mut host = Guard(match options.mode {
        Mode::Desktop => Some(HostDesktop::start(
            &bin,
            &options.state.with_file_name(format!("{label}-host")),
            &sockets.join("h"),
        )?),
        Mode::Headless => None,
    });
    let started = Instant::now();
    eprintln!(
        "==> starting the {label} session (state {})",
        options.state.display()
    );
    let mut child = launch(repo, &options, &traps, &ready, host.0.as_ref())
        .spawn()
        .map_err(|error| format!("could not start nested-session.sh: {error}"))?;
    let outcome = probe(&mut child, &ready, &bin, &options, host.0.as_ref(), started);
    stop_group(&mut child);
    if let Some(host) = host.0.as_mut() {
        stop_group(&mut host.child);
    }
    let sprung = traps.sprung();
    let _ = std::fs::remove_dir_all(&sockets);
    let logs = options.state.join("logs");
    verdict(outcome, &traps, &sprung, &logs, label, timings, started)
}

/// What the check found, said; an error if the session did not answer or reached a trap.
fn verdict(
    outcome: Result<Vec<String>, String>,
    traps: &Traps,
    sprung: &[String],
    logs: &Path,
    label: &str,
    mut timings: Vec<String>,
    started: Instant,
) -> Result<ExitCode, String> {
    let said = outcome.inspect_err(|_| {
        let log = std::fs::read_to_string(logs.join("session.log")).unwrap_or_default();
        let tail: Vec<&str> = log.lines().rev().take(30).collect();
        for line in tail.iter().rev() {
            eprintln!("  session.log: {line}");
        }
    });
    let said = match (said, sprung.is_empty()) {
        (Ok(said), true) => said,
        (Ok(_), false) => {
            return Err(format!(
                "the session reached the desktop it runs beside: {} (logs: {})",
                sprung.join("; "),
                logs.display()
            ));
        }
        (Err(error), _) => {
            return Err(format!(
                "{error} (logs: {}){}",
                logs.display(),
                if sprung.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; and it reached the desktop it runs beside: {}",
                        sprung.join("; ")
                    )
                }
            ));
        }
    };
    for line in said {
        println!("ok: {line}");
    }
    println!(
        "ok: nothing reached the host's session bus, system bus, login bus or systemd ({} traps{})",
        traps.all.len(),
        if traps.mounts.is_empty() {
            ""
        } else {
            ", fixed paths included"
        }
    );
    timings.push(format!(
        "start, answer and stop {}",
        seconds(started.elapsed())
    ));
    println!("ok: cargo {label} --check passed ({})", timings.join(", "));
    Ok(ExitCode::SUCCESS)
}

/// The session's build, of the apps asked for.
fn build(repo: &Path, options: &Options) -> Result<(), String> {
    eprintln!("==> building the session (dist/dev/nested-session-build.sh)");
    let mut build = Command::new("bash");
    build.arg(repo.join("dist/dev/nested-session-build.sh"));
    session::apps_to_build(&mut build, options);
    let status = build.status().map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("the session's build failed (above)".into())
    }
}

/// nested-session.sh as the check runs it: on the traps, inside a mount namespace that puts them
/// over the fixed bus paths, and in a window of `host` when there is one.
fn launch(
    repo: &Path,
    options: &Options,
    traps: &Traps,
    ready: &Path,
    host: Option<&HostDesktop>,
) -> Command {
    let bwrap = Command::new("bwrap")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    let mut command = if traps.mounts.is_empty() || !bwrap {
        eprintln!("==> no bwrap or no fixed bus paths here: the traps are the variables only");
        Command::new("bash")
    } else {
        let mut command = Command::new("bwrap");
        command
            .args(["--dev-bind", "/", "/"])
            .args(&traps.mounts)
            .args(["--die-with-parent", "--", "bash"]);
        command
    };
    let script = session::script(repo, options, ready, false);
    command.args(script.get_args());
    for (key, value) in script.get_envs() {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    command
        .env("XDG_RUNTIME_DIR", &traps.runtime)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", traps.session_bus.display()),
        )
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            format!("unix:path={}", traps.system_bus.display()),
        )
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .process_group(0);
    if let Some(host) = host {
        command
            .env("WAYLAND_DISPLAY", &host.display)
            .env("TILO_NESTED_HOST_IN_CONTAINER", "1");
    } else {
        command.env_remove("WAYLAND_DISPLAY");
    }
    command
}

/// Wait for the session to answer, then ask it something: its window on the host desktop, or a
/// snapshot and its actions.
fn probe(
    child: &mut Child,
    ready: &Path,
    bin: &Path,
    options: &Options,
    host: Option<&HostDesktop>,
    started: Instant,
) -> Result<Vec<String>, String> {
    let text = session::wait_ready(child, ready, Duration::from_secs(600))?;
    let mut said = vec![format!(
        "answered {} after start",
        seconds(started.elapsed())
    )];
    let field =
        |key| json::string_field(&text, key).ok_or_else(|| format!("{key} missing in {text}"));
    let (socket, runtime) = (field("control")?, field("runtime")?);
    if let Some(host) = host {
        // The nested session's window, as the host desktop lists it: the nested backend shows its
        // frames to the host alone and reads none back, so the host is where it is seen.
        let windows = control(
            bin,
            &host.control.display().to_string(),
            &host.runtime.display().to_string(),
            &["windows"],
        )?;
        let window = windows
            .lines()
            .find(|line| line.contains("Tilo"))
            .ok_or_else(|| format!("the host desktop shows no Tilo window: {windows}"))?;
        said.push(format!(
            "its window is open on the host desktop: {}",
            window.trim()
        ));
    } else {
        let shot = options.state.join("check.png");
        control(
            bin,
            &socket,
            &runtime,
            &["snapshot", &shot.display().to_string()],
        )?;
        said.push(format!("snapshot {}", shot.display()));
        let actions = control(bin, &socket, &runtime, &["actions", "list"])?;
        let count = actions
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();
        if count == 0 {
            return Err("tilo-control actions list printed nothing".into());
        }
        said.push(format!(
            "tilo-control actions list: {count} lines, the first:"
        ));
        for line in actions.lines().take(5) {
            said.push(format!("  {line}"));
        }
    }
    // Long enough for the services the session starts after its first frame to reach for a bus.
    std::thread::sleep(Duration::from_secs(5));
    Ok(said)
}

/// The stand-in host desktop, stopped however the check ends.
struct Guard(Option<HostDesktop>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(host) = self.0.as_mut() {
            stop_group(&mut host.child);
        }
    }
}
