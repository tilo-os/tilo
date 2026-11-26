//! A nested Tilo session the tool drives: where it is, the environment its own tools act on it
//! in, and the device's daemon that runs beside its desktop.
//!
//! The session is `dist/dev/nested-session.sh` over the build in
//! `target/nested-session`. A build goes to the daemon's package cache as the device's own
//! development build (`packages#install-dev`), and the daemon starts it as it starts any app a
//! window shows.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::package::StagedDev;

pub(crate) const SESSION_SCRIPT: &str = "dist/dev/nested-session.sh";
pub(crate) const BUILD_DIR: &str = "target/nested-session";
/// The variable naming a nested-session build elsewhere than the checkout's [`BUILD_DIR`]: a build
/// shared read-only between sessions, as `nested-session.sh` honours it.
pub(crate) const BUILD_ENV: &str = "TILO_NESTED_SESSION_BUILD";
pub(crate) const RUNTIME_BINARY: &str = "thin-app/tilolang-process";
/// The file nested-session.sh writes into a running session's runtime directory: its control
/// socket, runtime directory and state directory, as JSON.
pub(crate) const SESSION_FILE: &str = "session.json";

/// The Tilo checkout the tool works in: `explicit`, `$TILO_REPO`, or the first directory above
/// `dir` or the current directory that has the nested session; `None` when nothing names one and
/// none is above, as for a tool installed on its own.
///
/// # Errors
/// `explicit` or `$TILO_REPO` names a directory that is no checkout.
pub(crate) fn checkout(explicit: Option<&Path>, dir: &Path) -> Result<Option<PathBuf>, String> {
    if let Some(repo) = explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("TILO_REPO").map(PathBuf::from))
    {
        return if repo.join(SESSION_SCRIPT).is_file() {
            Ok(Some(repo))
        } else {
            Err(format!("{}: no {SESSION_SCRIPT} there", repo.display()))
        };
    }
    let starts = [
        std::fs::canonicalize(dir).ok(),
        std::env::current_dir().ok(),
    ];
    Ok(starts.into_iter().flatten().find_map(|start| {
        start
            .ancestors()
            .find(|dir| dir.join(SESSION_SCRIPT).is_file())
            .map(Path::to_path_buf)
    }))
}

/// The checkout a nested session is started from or attached in ([`checkout`]), or what to do
/// without one: every command that runs an app (`tilolang run`, `tilolang dev`) needs it.
pub(crate) fn repo_root(
    explicit: Option<&Path>,
    dir: &Path,
    command: &str,
) -> Result<PathBuf, String> {
    checkout(explicit, dir)?.ok_or_else(|| {
        format!(
            "`{command}` runs the app in a nested Tilo session, which is built and started from a \
             Tilo checkout ({SESSION_SCRIPT}), and there is none here: run it in a checkout of \
             https://code.tilo.so/tilo/tilo, or name one with --repo DIR or $TILO_REPO"
        )
    })
}

/// The nested-session build the session's tools come from: `$TILO_NESTED_SESSION_BUILD` when set,
/// as `nested-session.sh` reads it, else the checkout `repo`'s [`BUILD_DIR`].
pub(crate) fn build_dir(repo: &Path) -> PathBuf {
    build_dir_from(repo, std::env::var_os(BUILD_ENV))
}

fn build_dir_from(repo: &Path, named: Option<std::ffi::OsString>) -> PathBuf {
    named
        .filter(|named| !named.is_empty())
        .map_or_else(|| repo.join(BUILD_DIR), PathBuf::from)
}

/// The directory of the session's tools (`tilo-install`, `tilo-control`, ...) in the build
/// [`build_dir`] names, or why it holds none: which build it looked in and how it chose it.
pub(crate) fn tools(repo: &Path) -> Result<PathBuf, String> {
    let bin = build_dir(repo).join("bin");
    if bin.join("tilo-install").is_file() {
        return Ok(bin);
    }
    let chosen = if std::env::var_os(BUILD_ENV).is_some_and(|named| !named.is_empty()) {
        format!("${BUILD_ENV}")
    } else {
        format!("the checkout's {BUILD_DIR}; ${BUILD_ENV} names another build")
    };
    Err(format!(
        "{} has no session tools (no tilo-install): it is the build {chosen}. Build one with \
         dist/dev/nested-session-build.sh",
        bin.display()
    ))
}

/// The control socket nested-session.sh gives the session whose state is `state`.
pub(crate) fn control_socket(state: &Path) -> Result<PathBuf, String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or("XDG_RUNTIME_DIR is unset")?;
    let digest = sha256_hex(state.as_os_str().as_encoded_bytes());
    let short: String = digest.chars().take(12).collect();
    Ok(runtime
        .join(format!("tilo-nested-{short}"))
        .join("control.sock"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// The id `app`'s development build runs under on the device `command` asks: `tilo-install
/// dev-id`, which asks the daemon (`devices#dev-id`). The device's key never leaves the daemon,
/// so this is where a build learns what to compile the app as.
pub(crate) fn dev_id_from(mut command: Command, app: &str) -> Result<String, String> {
    let output = command
        .args(["dev-id", app])
        .output()
        .map_err(|error| format!("asking the device for {app}'s development build: {error}"))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "asking the device for {app}'s development build: {}",
            said.trim()
        ));
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if id.is_empty() {
        return Err(format!(
            "the device named no id {app}'s development build runs under"
        ));
    }
    Ok(id)
}

/// The `tilo-install` that asks this computer's daemon when no session is named: the tool's own
/// sibling, the checkout's session build beside `dir`, or one on `PATH`.
pub(crate) fn install_tool(dir: &Path) -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let sibling = parent.join("tilo-install");
        if sibling.is_file() {
            return sibling;
        }
    }
    if let Ok(Some(repo)) = checkout(None, dir) {
        let tool = build_dir(&repo).join("bin").join("tilo-install");
        if tool.is_file() {
            return tool;
        }
    }
    PathBuf::from("tilo-install")
}

/// The interfaces a nested session serves: the checkout's own declaration, which nested-session.sh
/// hands its shell as `TILO_SYSTEM_INTERFACES`, not the host image's.
const SYSTEM_INTERFACES: &str = "os/usr/share/tilo/system-interfaces.toml";

pub(crate) struct Peer {
    pub(crate) state: PathBuf,
    pub(crate) control: PathBuf,
    interfaces: PathBuf,
}

#[derive(serde::Deserialize)]
struct SessionFile {
    control: PathBuf,
    runtime: PathBuf,
    state: PathBuf,
}

impl SessionFile {
    /// The running session `named` names: its ready file, its runtime directory, its control
    /// socket or its state directory.
    fn named(named: &Path) -> Result<Self, String> {
        let file = if named.join(SESSION_FILE).is_file() {
            named.join(SESSION_FILE)
        } else if named.is_dir() {
            control_socket(named)?.with_file_name(SESSION_FILE)
        } else if named
            .extension()
            .is_some_and(|extension| extension == "sock")
        {
            named.with_file_name(SESSION_FILE)
        } else {
            named.to_path_buf()
        };
        let text = std::fs::read_to_string(&file).map_err(|error| {
            format!(
                "{}: {error} (name a running nested session: its --ready file, its runtime \
                 directory, its control socket or its state directory)",
                file.display()
            )
        })?;
        let session: Self = serde_json::from_str(&text)
            .map_err(|error| format!("{}: not a session file: {error}", file.display()))?;
        if !session.control.exists() {
            return Err(format!(
                "{}: the session is not running (no {})",
                file.display(),
                session.control.display()
            ));
        }
        Ok(session)
    }
}

/// A running nested session a person's programs reach Tilo's API in: where it answers, and the
/// state directory a person started it with, which names it.
#[derive(Debug)]
pub(crate) struct Running {
    pub(crate) state: PathBuf,
    pub(crate) socket: PathBuf,
    /// Its runtime directory, where its daemon listens.
    pub(crate) runtime: PathBuf,
}

impl Running {
    /// The running session `named` names, as [`SessionFile::named`] reads it.
    ///
    /// # Errors
    /// It names no running session, or one that does not answer.
    pub(crate) fn named(named: &Path) -> Result<Self, String> {
        let session = SessionFile::named(named)?;
        let socket = session
            .runtime
            .join(tilo_paths::login::SESSION_DIR)
            .join(tilo_paths::login::API_SOCKET_FILE);
        if tilo_files::native::socket::connect(&socket).is_err() {
            return Err(format!(
                "the session {} does not answer yet; try again once it shows its desktop",
                session.state.display()
            ));
        }
        Ok(Self {
            state: session.state,
            socket,
            runtime: session.runtime,
        })
    }

    /// The environment a program runs in to act in this session: its login, so it reaches the
    /// session's own daemon as `tilo-install` does ([`Session::command`]).
    pub(crate) fn environment(&self) -> Vec<(&'static str, PathBuf)> {
        let mut environment = login_environment(&self.state);
        environment.push(("XDG_RUNTIME_DIR", self.runtime.clone()));
        environment
    }

    /// Every nested session running for this login, by state directory.
    #[must_use]
    pub(crate) fn all() -> Vec<Self> {
        let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(runtime) else {
            return Vec::new();
        };
        let mut all: Vec<Self> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("tilo-nested-")
            })
            .filter_map(|entry| Self::named(&entry.path()).ok())
            .collect();
        all.sort_by(|one, other| one.state.cmp(&other.state));
        all
    }
}

/// The newest session this login runs, if one does: what `tilolang dev` attaches to instead of
/// starting a session of its own.
#[must_use]
pub(crate) fn newest_running() -> Option<Running> {
    Running::all().into_iter().max_by_key(|running| {
        std::fs::metadata(&running.socket)
            .and_then(|meta| meta.modified())
            .ok()
    })
}

/// Start a session on `state` that outlives this tool, and wait until it answers: the session
/// `tilolang dev` leaves running for the next `dev` to attach to.
///
/// # Errors
/// The session script cannot start, or the session does not answer within [`START_WAIT`].
pub(crate) fn start_detached(
    repo: &Path,
    state: &Path,
    ready: &Path,
    headless: bool,
    software: bool,
) -> Result<(), String> {
    std::fs::create_dir_all(state).map_err(|error| format!("{}: {error}", state.display()))?;
    let _ = std::fs::remove_file(ready);
    let log = std::fs::File::create(state.join("session.out"))
        .map_err(|error| format!("{}: {error}", state.display()))?;
    let mut command = Command::new(repo.join(SESSION_SCRIPT));
    command
        .args(["--no-build", "--apps", ""])
        .arg("--state")
        .arg(state)
        .arg("--ready")
        .arg(ready)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log);
    if headless {
        command.arg("--headless");
    }
    if software {
        command.arg("--software");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // A session of its own, so the tool ending does not end it.
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(|error| format!("starting the session: {error}"))?;
    let started = std::time::Instant::now();
    while !ready.is_file() {
        if started.elapsed() > START_WAIT {
            return Err(format!(
                "the session did not answer {} s after it started (see {})",
                START_WAIT.as_secs(),
                state.join("session.out").display()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
}

/// How long a session this tool starts may take to answer.
const START_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

impl Peer {
    /// The session whose state is `state`, started by this tool from the checkout `repo`.
    pub(crate) fn new(state: PathBuf, repo: &Path) -> Result<Self, String> {
        Ok(Self {
            control: control_socket(&state)?,
            state,
            interfaces: repo.join(SYSTEM_INTERFACES),
        })
    }

    /// The running session `named` names ([`SessionFile::named`]), run from the checkout `repo`.
    pub(crate) fn attach(named: &Path, repo: &Path) -> Result<Self, String> {
        let session = SessionFile::named(named)?;
        Ok(Self {
            state: session.state,
            control: session.control,
            interfaces: repo.join(SYSTEM_INTERFACES),
        })
    }

    /// The environment `tilo-install` and `tilo-control` act on
    /// this session in: the one nested- session.sh gives its
    /// shell.
    pub(crate) fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        let state = &self.state;
        command
            .envs(login_environment(state))
            .env("TILO_RUNTIME_BUNDLE_DIR", self.runtimes())
            .env(
                "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
                state.join("keys/runtime-publishers.txt"),
            )
            .env("TILO_TEST_CONTROL", &self.control)
            .env("TILO_SYSTEM_INTERFACES", &self.interfaces)
            .env_remove("TILO_APPS_SYSTEM_DIR");
        if let Some(runtime) = self.control.parent() {
            command.env("XDG_RUNTIME_DIR", runtime);
        }
        command
    }

    pub(crate) fn runtimes(&self) -> PathBuf {
        self.state.join("runtimes")
    }

    /// The Tilo runtime and the tilolang runtime the session baked, as packages name them.
    pub(crate) fn runtimes_baked(&self) -> Result<(String, String), String> {
        let tilo = read_trimmed(&self.runtimes().join(".version"))?;
        let tilolang = read_trimmed(&self.runtimes().join(".tilolang-version"))?;
        Ok((
            format!("tilo-runtime/{tilo}"),
            format!("tilolang/{tilolang}"),
        ))
    }

    /// Install `staged` on the session's daemon as this device's development build, and answer the
    /// id it runs under. The session's `tilo-install` asks the daemon (`packages#install-dev`),
    /// which marks the build, packs it and signs it with the device's key: neither the tool nor
    /// the session ever reads that key.
    pub(crate) fn install_dev(&self, bin: &Path, staged: &StagedDev) -> Result<String, String> {
        let id_file = staged.dir.with_extension("id");
        let _ = std::fs::remove_file(&id_file);
        status(
            self.command(&bin.join("tilo-install"))
                .arg("install")
                .arg(&staged.dir)
                .args(["--dev", "--id-file"])
                .arg(&id_file)
                .stdout(Stdio::null()),
            &format!("installing {} as its development build", staged.app),
        )?;
        let id = std::fs::read_to_string(&id_file)
            .map_err(|error| format!("{}: {error}", id_file.display()))?;
        Ok(id.trim().to_owned())
    }

    /// The id the session's daemon installs `app`'s development build under: the session's
    /// `tilo-install` asks it (`devices#dev-id`), which derives it from the device key the daemon
    /// alone keeps. A build compiled for another id runs as no app the daemon launched.
    pub(crate) fn dev_id(&self, bin: &Path, app: &str) -> Result<String, String> {
        dev_id_from(self.command(&bin.join("tilo-install")), app)
    }

    /// Start `app` through the session's daemon, as a person's window starts: the launch the
    /// installed package names, with its sandbox and grants.
    ///
    /// A development loop starts one instance, so it asks for a plain start (`presentation:
    /// false`) at the daemon's native endpoint. The presentation start first brings a running
    /// window forward, which the daemon asks of itself over a second connection that the first
    /// launch of a session has not yet admitted.
    pub(crate) fn start_app(&self, _bin: &Path, app: &str) -> Result<(), String> {
        let socket = self
            .control
            .parent()
            .ok_or_else(|| "the session names no runtime directory".to_owned())?
            .join("tilo")
            .join(tilo_paths::machine::DAEMON_NATIVE);
        let answered = native_call(&socket, &serde_json::json!({"native": "status"}))?;
        let workspace = answered
            .get("workspace")
            .and_then(serde_json::Value::as_str)
            .filter(|workspace| !workspace.is_empty())
            .ok_or_else(|| "the session serves no workspace to start an app in yet".to_owned())?;
        let answer = native_call(
            &socket,
            &serde_json::json!({
                "native": runtime_portal::native::LAUNCH,
                "app_id": app,
                "workspace": workspace,
                "presentation": false,
            }),
        )?;
        match answer.get("error").and_then(serde_json::Value::as_str) {
            Some(error) => Err(format!("starting {app}: {error}")),
            None => Ok(()),
        }
    }
}

/// The login a session on `state` runs in: its home, its directories and its identity.
fn login_environment(state: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("HOME", state.join("home")),
        ("XDG_CONFIG_HOME", state.join("config")),
        ("XDG_DATA_HOME", state.join("data")),
        ("XDG_STATE_HOME", state.join("state")),
        ("XDG_CACHE_HOME", state.join("cache")),
        ("TILO_IDENTITY_ROOT", state.join("identity")),
        ("TILO_APPS_DATA_DIR", state.join("apps")),
    ]
}

/// One request to the daemon's native endpoint, and the line it answers.
///
/// A start answers only once its app is up, which takes what the app takes: the call waits for
/// the answer rather than cutting it off at a time it invented. A session that ends without
/// answering closes the connection, which is what ends the wait with an error.
fn native_call(socket: &Path, request: &serde_json::Value) -> Result<serde_json::Value, String> {
    use std::io::{BufRead as _, BufReader, Write as _};
    let refused = |error: std::io::Error| format!("{}: {error}", socket.display());
    let mut stream = tilo_files::native::socket::connect(socket).map_err(refused)?;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(refused)?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(refused)?;
    if line.trim().is_empty() {
        return Err(format!(
            "{}: the session ended before it answered",
            socket.display()
        ));
    }
    serde_json::from_str(&line).map_err(|error| format!("the daemon answered no line: {error}"))
}

pub(crate) fn status(command: &mut Command, what: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("{what}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed ({status})"))
    }
}

pub(crate) fn read_trimmed(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_session_names_its_apps_and_its_build;

        phase the_build_is_the_one_the_environment_names_else_the_checkouts;
        {
            let repo = Path::new("/src/tilo");
            assert_eq!(
                build_dir_from(repo, None),
                Path::new("/src/tilo/target/nested-session")
            );
            assert_eq!(
                build_dir_from(repo, Some("".into())),
                Path::new("/src/tilo/target/nested-session")
            );
            assert_eq!(
                build_dir_from(repo, Some("/work/shared/current".into())),
                Path::new("/work/shared/current")
            );
        }
    }

    /// A start answers once its app is up, which takes what the app takes: the call waits for
    /// the daemon's answer instead of cutting it off at a time the tool invented.
    #[cfg(unix)]
    #[test]
    fn a_call_waits_for_an_answer_the_session_takes_its_time_for() {
        use std::io::{BufRead as _, BufReader, Write as _};
        let scratch = tempfile::tempdir().unwrap();
        let socket = scratch.path().join("native.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let answering = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert!(line.contains("status"), "{line}");
            // An app that takes its time to come up.
            std::thread::sleep(std::time::Duration::from_millis(1500));
            writeln!(stream, "{{\"workspace\":\"served\"}}").unwrap();
        });
        let answer = native_call(&socket, &serde_json::json!({"native": "status"})).unwrap();
        assert_eq!(answer["workspace"], "served");
        answering.join().unwrap();
    }

    /// The session the tool attaches to when a command names none is the newest one running for
    /// this login, so one `tilolang dev` does not start a session the next would pay for again.
    #[test]
    fn the_newest_running_session_is_the_one_to_attach_to() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = std::fs::canonicalize(scratch.path()).unwrap();
        let mut held = Vec::new();
        let mut states = Vec::new();
        for name in ["tilo-nested-a", "tilo-nested-b"] {
            let dir = runtime.join(name);
            let login = dir.join(tilo_paths::login::SESSION_DIR);
            std::fs::create_dir_all(&login).unwrap();
            held.push(
                std::os::unix::net::UnixListener::bind(
                    login.join(tilo_paths::login::API_SOCKET_FILE),
                )
                .unwrap(),
            );
            held.push(std::os::unix::net::UnixListener::bind(dir.join("control.sock")).unwrap());
            let state = runtime.join(format!("state-{name}"));
            std::fs::create_dir_all(&state).unwrap();
            std::fs::write(
                dir.join(SESSION_FILE),
                serde_json::json!({
                    "control": dir.join("control.sock"),
                    "runtime": dir,
                    "state": state,
                })
                .to_string(),
            )
            .unwrap();
            states.push(state);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("XDG_RUNTIME_DIR", &runtime);
        let newest = newest_running().expect("a running session");
        assert_eq!(newest.state, states[1], "the newest session answers");
        assert!(
            held.iter().all(|socket| socket.local_addr().is_ok()),
            "both sessions' sockets stay bound while the tool looks"
        );
    }
}
