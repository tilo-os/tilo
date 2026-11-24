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
        })
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

    /// Start `app` through the session's daemon, as a person's window starts: the launch the
    /// installed package names, with its sandbox and grants.
    pub(crate) fn start_app(&self, bin: &Path, app: &str) -> Result<(), String> {
        status(
            self.command(&bin.join("tilo-runtime"))
                .args(["--run-app", app])
                .stdout(Stdio::null()),
            &format!("starting {app}"),
        )
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
}
