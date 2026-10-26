//! A nested Tilo session the tool drives: where it is, the environment its own tools act on it
//! in, and the session's actions the tool uses (open an app, restart it, install a package).
//!
//! The session is `dist/dev/nested-session.sh` (or `nested-pair.sh`) over the build in
//! `target/nested-session`. Everything here goes through the session's own tools and actions:
//! `tilo-install` installs, the launcher starts apps, the shell's actions open and close them.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) const SESSION_SCRIPT: &str = "dist/dev/nested-session.sh";
pub(crate) const PAIR_SCRIPT: &str = "dist/dev/nested-pair.sh";
pub(crate) const BUILD_DIR: &str = "target/nested-session";
/// The thin binary the session bakes into the tilolang runtime bundle.
pub(crate) const RUNTIME_BINARY: &str = "thin-app/tilolang-app";
/// The file nested-session.sh writes into a running session's runtime directory: its control
/// socket, runtime directory and state directory, as JSON.
pub(crate) const SESSION_FILE: &str = "session.json";

/// The Tilo checkout: `explicit`, `$TILO_REPO`, or the first directory above `dir` or the
/// current directory that has the nested session.
pub(crate) fn repo_root(explicit: Option<&Path>, dir: &Path) -> Result<PathBuf, String> {
    if let Some(repo) = explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("TILO_REPO").map(PathBuf::from))
    {
        return if repo.join(SESSION_SCRIPT).is_file() {
            Ok(repo)
        } else {
            Err(format!("{}: no {SESSION_SCRIPT} there", repo.display()))
        };
    }
    let starts = [
        std::fs::canonicalize(dir).ok(),
        std::env::current_dir().ok(),
    ];
    starts
        .into_iter()
        .flatten()
        .find_map(|start| {
            start
                .ancestors()
                .find(|dir| dir.join(SESSION_SCRIPT).is_file())
                .map(Path::to_path_buf)
        })
        .ok_or_else(|| {
            "no Tilo checkout above the package; name one with --repo or $TILO_REPO".to_owned()
        })
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

/// One session the tool drives.
pub(crate) struct Peer {
    pub(crate) state: PathBuf,
    pub(crate) control: PathBuf,
}

#[derive(serde::Deserialize)]
struct SessionFile {
    control: PathBuf,
    state: PathBuf,
}

impl Peer {
    /// The session whose state is `state`, started by this tool.
    pub(crate) fn new(state: PathBuf) -> Result<Self, String> {
        Ok(Self {
            control: control_socket(&state)?,
            state,
        })
    }

    /// The running session `named` names: its ready file, its runtime directory, or its control
    /// socket.
    pub(crate) fn attach(named: &Path) -> Result<Self, String> {
        let file = if named.is_dir() {
            named.join(SESSION_FILE)
        } else if named
            .extension()
            .is_some_and(|extension| extension == "sock")
        {
            named
                .parent()
                .ok_or("a control socket has a directory")?
                .join(SESSION_FILE)
        } else {
            named.to_path_buf()
        };
        let text = std::fs::read_to_string(&file).map_err(|error| {
            format!(
                "{}: {error} (name a running nested session: its --ready file, its runtime \
                 directory or its control socket)",
                file.display()
            )
        })?;
        let session: SessionFile = serde_json::from_str(&text)
            .map_err(|error| format!("{}: not a session file: {error}", file.display()))?;
        if !session.control.exists() {
            return Err(format!(
                "{}: the session is not running (no {})",
                file.display(),
                session.control.display()
            ));
        }
        Ok(Self {
            state: session.state,
            control: session.control,
        })
    }

    /// The environment `tilo-install` and `tilo-control` act on this session in: the one
    /// nested-session.sh gives its shell.
    pub(crate) fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        let state = &self.state;
        command
            .env("HOME", state.join("home"))
            .env("XDG_CONFIG_HOME", state.join("config"))
            .env("XDG_DATA_HOME", state.join("data"))
            .env("XDG_STATE_HOME", state.join("state"))
            .env("XDG_CACHE_HOME", state.join("cache"))
            .env("TILO_IDENTITY_ROOT", state.join("identity"))
            .env("TILO_APPS_DATA_DIR", state.join("apps"))
            .env("TILO_RUNTIME_BUNDLE_DIR", self.runtimes())
            .env(
                "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
                state.join("keys/runtime-publishers.txt"),
            )
            .env("TILO_TEST_CONTROL", &self.control)
            .env_remove("TILO_APPS_SYSTEM_DIR");
        if let Some(runtime) = self.control.parent() {
            command.env("XDG_RUNTIME_DIR", runtime);
        }
        command
    }

    /// Where the session's runtime bundles are installed.
    pub(crate) fn runtimes(&self) -> PathBuf {
        self.state.join("runtimes")
    }

    /// The key the session's runtime bundles are signed with: the one its launcher trusts.
    pub(crate) fn runtime_publisher(&self) -> PathBuf {
        self.state.join("keys/publisher.json")
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

    /// Install the signed package `tapp`, replacing the installed one of the same id.
    pub(crate) fn install(&self, bin: &Path, tapp: &Path) -> Result<(), String> {
        status(
            self.command(&bin.join("tilo-install"))
                .arg("install")
                .arg(tapp)
                .args(["--allow-self-signed", "--force"])
                .stdout(Stdio::null()),
            &format!("installing {}", tapp.display()),
        )
    }

    /// Open `app` through the session's own action, or bring its window to the front when it
    /// has one.
    pub(crate) fn open(&self, bin: &Path, app: &str) -> Result<(), String> {
        self.invoke(bin, "applications", app)
    }

    fn invoke(&self, bin: &Path, provider: &str, action: &str) -> Result<(), String> {
        output(
            self.command(&bin.join("tilo-control"))
                .args(["actions", "invoke", provider, action, "--wait"]),
            &format!("{provider} {action}"),
        )
        .map(drop)
    }

    /// Whether a window of `app` is open.
    pub(crate) fn is_open(&self, bin: &Path, app: &str) -> bool {
        output(
            self.command(&bin.join("tilo-control")).arg("windows"),
            "listing windows",
        )
        .is_ok_and(|windows| windows.contains(&format!("\"{app}\"")))
    }

    /// Start `app` again in its place: its window to the front, closed, and reopened where it
    /// was, which relaunches it as a session restore does: on the installed package and the
    /// runtime it names, with its data.
    pub(crate) fn restart(&self, bin: &Path, app: &str) -> Result<(), String> {
        self.open(bin, app)?;
        self.invoke(bin, "actions", "tab.close")?;
        self.invoke(bin, "actions", "tab.reopen")
    }
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

pub(crate) fn output(command: &mut Command, what: &str) -> Result<String, String> {
    let output = command
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("{what}: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!("{what} failed ({})", output.status))
    }
}

pub(crate) fn read_trimmed(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|text| text.trim().to_owned())
        .map_err(|error| format!("{}: {error}", path.display()))
}
