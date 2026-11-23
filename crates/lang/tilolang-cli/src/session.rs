//! A nested Tilo session the tool drives: where it is, the environment its own tools act on it
//! in, and the session's actions the tool uses (open an app, restart it, install a package).
//!
//! The session is `dist/dev/nested-session.sh` over the build in
//! `target/nested-session`. Everything here goes through the session's own tools and actions:
//! `tilo-install` installs, the launcher starts apps, the shell's actions open and close them.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ui::tilo_actions::remote::{self, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget};

pub(crate) const SESSION_SCRIPT: &str = "dist/dev/nested-session.sh";
pub(crate) const BUILD_DIR: &str = "target/nested-session";
/// The variable naming a nested-session build elsewhere than the checkout's [`BUILD_DIR`]: a build
/// shared read-only between sessions, as `nested-session.sh` honours it.
pub(crate) const BUILD_ENV: &str = "TILO_NESTED_SESSION_BUILD";
pub(crate) const RUNTIME_BINARY: &str = "thin-app/tilolang-process";
/// The file nested-session.sh writes into a running session's runtime directory: its control
/// socket, runtime directory and state directory, as JSON.
pub(crate) const SESSION_FILE: &str = "session.json";

/// The Tilo checkout: `explicit`, `$TILO_REPO`, or the first directory above `dir` or the current
/// directory that has the nested session.
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

/// How long a window may take to close when its app is asked to let it go.
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The interfaces a nested session serves: the checkout's own declaration, which nested-session.sh
/// hands its shell as `TILO_SYSTEM_INTERFACES`, not the host image's.
const SYSTEM_INTERFACES: &str = "os/usr/share/tilo/system-interfaces.toml";

/// How long a session may take to offer an app it installed.
const OFFER_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// How long the session may hold an action's answer while it runs, in milliseconds.
const ACTION_WAIT: u32 = 5_000;
/// The prefix of a window's resource address, before its tab.
const WINDOW_PREFIX: &str = "desktop:tab:";

/// The session's own `action` of `resource`.
fn shell(resource: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: remote::SESSION_ACTIONS_PROVIDER.to_owned(),
        address: ui::tilo_actions::ActionAddress::new(resource, action),
    }
}

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

    /// Install the signed package `tapp`, replacing the installed one of the same id. A development
    /// session runs no device daemon's package cache, so the install names the session's store and is made in
    /// `tilo-install`'s own process.
    pub(crate) fn install(&self, bin: &Path, tapp: &Path) -> Result<(), String> {
        status(
            self.command(&bin.join("tilo-install"))
                .arg("--offline-store")
                .arg(self.state.join("apps"))
                .arg("install")
                .arg(tapp)
                .args(["--allow-self-signed", "--force"])
                .stdout(Stdio::null()),
            &format!("installing {}", tapp.display()),
        )
    }

    /// Open `app` through the workspace's own open of it, or bring its window to the front when it
    /// has one.
    pub(crate) fn open(&self, bin: &Path, app: &str) -> Result<(), String> {
        let open = self.wait_offered(bin, app)?;
        self.invoke(bin, open, &format!("opening {app}"))
    }

    /// Wait until the session's catalogue offers to open `app`, and answer that open: a session
    /// just started indexes the apps it installed as it comes up, and an app it has not indexed
    /// yet cannot be opened.
    pub(crate) fn wait_offered(&self, bin: &Path, app: &str) -> Result<RemoteActionTarget, String> {
        let resource = format!("{}{app}", remote::APPLICATION_RESOURCE_PREFIX);
        let started = std::time::Instant::now();
        loop {
            if let Some(open) =
                self.offered(bin, app, &resource, remote::APPLICATION_OPEN_ACTION)?
            {
                return Ok(open);
            }
            if started.elapsed() > OFFER_WAIT {
                return Err(format!("the session does not offer to open {app}"));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// What the session lists, among the rows that mention `needle`, as `resource`'s `action`.
    fn offered(
        &self,
        bin: &Path,
        needle: &str,
        resource: &str,
        action: &str,
    ) -> Result<Option<RemoteActionTarget>, String> {
        let mut query = remote::CatalogueQuery {
            needle: needle.to_owned(),
            max_results: remote::MAX_CATALOGUE_PAGE,
            ..remote::CatalogueQuery::default()
        };
        loop {
            let asked = RemoteActionMessage::CatalogueRequest {
                correlation: 1,
                query: query.clone(),
            };
            let RemoteActionMessage::CatalogueResponse { result, .. } =
                self.ask(bin, asked, "listing the session's actions")?
            else {
                return Err("the session answered a listing with something else".to_owned());
            };
            let page = result.map_err(|error| format!("listing the session's actions: {error}"))?;
            let found = page.rows.into_iter().find_map(|row| {
                let address = row.action?.address;
                (address.resource == resource && address.action == action).then_some(
                    RemoteActionTarget {
                        provider: row.provider,
                        address,
                    },
                )
            });
            if found.is_some() || page.exhausted || page.next_cursor.is_empty() {
                return Ok(found);
            }
            query.revision = Some(page.revision);
            query.cursor = page.next_cursor;
        }
    }

    /// Invoke `target`, and wait until it ran.
    fn invoke(&self, bin: &Path, target: RemoteActionTarget, what: &str) -> Result<(), String> {
        let invocation = ui::tilo_actions::InvocationId::new();
        let asked = RemoteActionMessage::Invoke(remote::InvokeRequest {
            invocation,
            target,
            argument: remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: Some(ACTION_WAIT),
            appearance: None,
            signed: None,
        });
        let RemoteActionMessage::Receipt(receipt) = self.ask(bin, asked, what)? else {
            return Err(format!("{what}: the session answered with something else"));
        };
        match receipt.status {
            remote::ReceiptStatus::Accepted | remote::ReceiptStatus::Completed { .. } => Ok(()),
            remote::ReceiptStatus::Failed { error } => Err(format!("{what}: {}", error.message)),
            remote::ReceiptStatus::Rejected { error } => Err(format!("{what}: {error}")),
            remote::ReceiptStatus::Cancelled { .. } => Err(format!("{what}: cancelled")),
        }
    }

    /// Ask `message` of the session's control socket, as `tilo-control` sends a frame there.
    fn ask(
        &self,
        bin: &Path,
        message: RemoteActionMessage,
        what: &str,
    ) -> Result<RemoteActionMessage, String> {
        let frame = RemoteActionFrame::new(message)
            .encode()
            .map_err(|error| format!("{what}: {error}"))?;
        let answer = output(
            self.command(&bin.join("tilo-control"))
                .arg("raw")
                .arg(String::from_utf8_lossy(&frame).as_ref()),
            what,
        )?;
        RemoteActionFrame::decode(answer.trim().as_bytes())
            .map(|frame| frame.message)
            .map_err(|error| format!("{what}: {error}: {answer}"))
    }

    /// The tabs showing a window of `app`: each as its group and tab id.
    fn tabs_of(&self, bin: &Path, app: &str) -> Result<Vec<(u64, u64)>, String> {
        let listed = output(
            self.command(&bin.join("tilo-control"))
                .args(["windows", "--json"]),
            "listing windows",
        )?;
        let windows: Vec<serde_json::Value> =
            serde_json::from_str(&listed).map_err(|error| format!("listing windows: {error}"))?;
        Ok(windows
            .iter()
            .filter(|window| window["app_id"] == app)
            .filter_map(|window| Some((window["group"].as_u64()?, window["tab"].as_u64()?)))
            .collect())
    }

    /// Whether a window of `app` is open.
    pub(crate) fn is_open(&self, bin: &Path, app: &str) -> bool {
        self.tabs_of(bin, app).is_ok_and(|tabs| !tabs.is_empty())
    }

    /// Start every window of `app` again in its place: each brought to the front, closed, and
    /// reopened where it was, which relaunches it as a session restore does: on the installed
    /// package and the runtime it names, with its data.
    pub(crate) fn restart(&self, bin: &Path, app: &str) -> Result<(), String> {
        for (group, tab) in self.tabs_of(bin, app)? {
            self.invoke(
                bin,
                shell(&format!("{WINDOW_PREFIX}{tab}"), "close-tab"),
                "closing",
            )?;
            // A window closes when its app has let it go: the tab is reopenable once it is gone.
            let asked = std::time::Instant::now();
            while self.tabs_of(bin, app)?.contains(&(group, tab)) {
                if asked.elapsed() > CLOSE_WAIT {
                    return Err(format!("{app} did not close its window"));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            self.invoke(bin, shell("desktop:chrome", "reopen-closed"), "reopening")?;
        }
        Ok(())
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
