//! `--share` for `run --platform web` and `dev --platform web`: the app in a
//! workspace other people join from their own browser.
//!
//! The workspace's host is the SDK's own, `tilo-relay room --persistent`, on
//! this machine: it governs the workspace, so it keys every record's channel
//! and what one person does reaches everyone else. The workspace is listed in
//! the host's directory at its default role, so its door admits each person
//! on their own: every browser that opens the served join page creates a
//! person of its own, asks the page server's `/join`, and the page server asks
//! the host's door for them. The app is an author of the workspace
//! (`--author-app`), so members write through it.
//!
//! It is for development on this machine. Anyone who reaches the page is
//! let in; the page server and the host listen on loopback.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::directory::door_path;
use tilo_relay_protocol::web::API_PREFIX;
use tilo_workspace_directory::{Person, client, join, web_origin};

use super::Project;

/// Every line the host writes for its operator starts with this.
const OPERATOR: &str = "tilo-room-operator ";
/// How long the host has to start serving.
const START_TIMEOUT: Duration = Duration::from_secs(120);

/// A running shared workspace.
pub struct SharedWorkspace {
    /// The host's page-facing origin (plain HTTP on loopback).
    pub origin: String,
    /// The id of the relay room serving it.
    pub relay_room: String,
    /// The app every page opens.
    pub app_id: String,
    pub name: String,
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    /// The workspace's record of each app, once a page opened it, kept in
    /// `records_file` across restarts.
    records: Mutex<Records>,
    records_file: PathBuf,
    /// Everyone admitted, by person key: the name each joined under.
    people: Mutex<std::collections::BTreeMap<String, String>>,
    people_file: PathBuf,
}

#[derive(Default)]
struct Records {
    opened: HashMap<String, String>,
    /// Apps a page is opening the record of now, and since when.
    opening: HashMap<String, Instant>,
}

impl Records {
    fn claim(&mut self, app: &str, open: bool, now: Instant) -> RecordClaim {
        if let Some(record) = self.opened.get(app) {
            return RecordClaim {
                record: Some(record.clone()),
                open: false,
            };
        }
        let busy = self
            .opening
            .get(app)
            .is_some_and(|since| now.saturating_duration_since(*since) < OPENING_TIMEOUT);
        let open = open && !busy;
        if open {
            self.opening.insert(app.to_owned(), now);
        }
        RecordClaim { record: None, open }
    }

    /// The first record named for `app` stays its record.
    fn opened(&mut self, app: &str, record: &str) {
        self.opening.remove(app);
        self.opened
            .entry(app.to_owned())
            .or_insert_with(|| record.to_owned());
    }
}

/// How long the page that is opening an app's record has before another may.
const OPENING_TIMEOUT: Duration = Duration::from_secs(60);

/// Where an app's record in the workspace stands, for a page that opens it.
#[derive(Debug, PartialEq, Eq)]
pub struct RecordClaim {
    pub record: Option<String>,
    /// Whether the asking page is to open it.
    pub open: bool,
}

/// What a page needs to join: the invitation the workspace's door issued for
/// its person and the one-time token that redeems it at once.
pub struct Admission {
    pub invitation: String,
    pub admission_token: String,
}

impl SharedWorkspace {
    /// Start the workspace for `project`, whose pages are served at
    /// `pages_origin`, keeping it under the app's target directory so a
    /// restart reopens the same workspace with what was in it. `fresh` starts
    /// an empty one.
    pub fn start(project: &Project, pages_origin: &str, fresh: bool) -> Result<Self> {
        let relay = relay_binary(project)?;
        // The state a shared session has already written: kept under the
        // name it was written with, so a restart reopens it.
        let state = project
            .target_dir
            .join("tilo-app/web")
            .join(&project.package)
            .join("room");
        if fresh && state.exists() {
            std::fs::remove_dir_all(&state)
                .with_context(|| format!("removing {}", state.display()))?;
        }
        std::fs::create_dir_all(&state).with_context(|| format!("creating {}", state.display()))?;
        let owner_root = state.join("root");
        if !owner_root.exists() {
            write_secret(&owner_root, &hex::encode(random::<32>()?))?;
        }
        let [web, plain] = free_ports()?;
        let app_id = project.manifest.package.id.clone();
        let name = project.display_name();
        let mut child = ProcessCommand::new(&relay)
            .arg("room")
            .arg("--state")
            .arg(state.join("relay"))
            .args(["--persistent", &project.package])
            .args(["--workspace-name", &name])
            // Listed, so its door admits whoever asks, at the app's default
            // role: the page server asks it for each person.
            .args(["--listing-description", &format!("{name} in a browser")])
            .args(["--default-role", "editor"])
            .arg("--root-file")
            .arg(&owner_root)
            .arg("--room-id-file")
            .arg(state.join("room-id"))
            .args(["--author-app", &app_id])
            .args(["--web", &format!("127.0.0.1:{web}")])
            .args(["--web-plain", &format!("127.0.0.1:{plain}")])
            .args(["--allowed-origin", pages_origin])
            .env(
                "TILO_RELAY_LOG",
                std::env::var("TILO_RELAY_LOG").unwrap_or_else(|_| "error".into()),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("starting {}", relay.display()))?;
        let stdin = child.stdin.take().context("the host has no stdin")?;
        let stdout = child.stdout.take().context("the host has no stdout")?;
        let started_rx = follow_operator(stdout)?;
        let started = started_rx.recv_timeout(START_TIMEOUT).map_err(|_| {
            let _ = child.kill();
            anyhow::anyhow!("the host did not start in two minutes")
        })?;
        let relay_room = started
            .get("room")
            .and_then(Value::as_str)
            .context("the host named no room")?
            .to_owned();
        let records_file = state.join("records.json");
        let people_file = state.join("people.json");
        let people = std::fs::read(&people_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let opened = std::fs::read(&records_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Self {
            origin: format!("http://127.0.0.1:{plain}"),
            relay_room,
            app_id,
            name,
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            records: Mutex::new(Records {
                opened,
                opening: HashMap::new(),
            }),
            records_file,
            people: Mutex::new(people),
            people_file,
        })
    }

    /// Everyone the workspace admitted, `[{id, name}]`, `id` the person's key.
    pub fn people(&self) -> Value {
        self.people
            .lock_recover()
            .iter()
            .map(|(id, name)| json!({"id": id, "name": name}))
            .collect()
    }

    /// Where `app`'s record stands. With `open`, the asking page may be the
    /// one to open it: the first to ask while nobody is.
    pub fn claim(&self, app: &str, open: bool) -> RecordClaim {
        self.records.lock_recover().claim(app, open, Instant::now())
    }

    /// The page that opened `app`'s record names it.
    ///
    /// # Errors
    ///
    /// When the record cannot be kept for the next start.
    pub fn opened(&self, app: &str, record: &str) -> Result<()> {
        if app.is_empty() || record.is_empty() {
            bail!("name the app and its record");
        }
        let json = {
            let mut records = self.records.lock_recover();
            records.opened(app, record);
            serde_json::to_vec(&records.opened)?
        };
        std::fs::write(&self.records_file, json)
            .with_context(|| format!("writing {}", self.records_file.display()))
    }

    /// Ask the workspace's door to admit the person `user_public_key` (hex),
    /// named `label`, at the workspace's default role.
    pub fn admit(&self, user_public_key: &str, label: &str) -> Result<Admission> {
        let mut user = [0_u8; 32];
        hex::decode_to_slice(user_public_key, &mut user)
            .map_err(|_| anyhow!("no person to admit"))?;
        let door = web_origin(&format!(
            "{}{API_PREFIX}/{}",
            self.origin,
            door_path(&self.relay_room)
        ))
        .map_err(anyhow::Error::msg)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("starting the door's runtime")?;
        let granted = runtime
            .block_on(join(
                &client().map_err(anyhow::Error::msg)?,
                &door,
                &Person {
                    user,
                    name: label.to_owned(),
                },
            ))
            .map_err(anyhow::Error::msg)?;
        let people = {
            let mut people = self.people.lock_recover();
            people.insert(hex::encode(user), label.to_owned());
            serde_json::to_vec(&*people)
        };
        let _ = people.map(|json| std::fs::write(&self.people_file, json));
        Ok(Admission {
            invitation: granted.invitation,
            admission_token: granted.admission_token,
        })
    }
}

impl Drop for SharedWorkspace {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin.lock_recover(), "{}", json!({"op": "shutdown"}));
        let mut child = self.child.lock_recover();
        for _ in 0..20 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Read the host's operator events from `stdout` on a thread of its own and
/// answer the host's start on the returned channel.
fn follow_operator(stdout: std::process::ChildStdout) -> Result<mpsc::Receiver<Value>> {
    let (started_tx, started_rx) = mpsc::channel::<Value>();
    std::thread::Builder::new()
        .name("tilo-app-workspace".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Some(event) = line
                    .strip_prefix(OPERATOR)
                    .and_then(|json| serde_json::from_str::<Value>(json).ok())
                else {
                    continue;
                };
                if event.get("event").and_then(Value::as_str) == Some("started") {
                    let _ = started_tx.send(event);
                }
            }
        })
        .context("starting the operator")?;
    Ok(started_rx)
}

/// The SDK's host, built from the SDK checkout the app builds against (with
/// its lockfile) the first time and reused after.
fn relay_binary(project: &Project) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("TILO_RELAY") {
        return Ok(PathBuf::from(path));
    }
    let in_workspace = project.workspace_root == project.sdk_root;
    let target = if in_workspace {
        project.target_dir.clone()
    } else {
        project.target_dir.join("tilo-app/relay")
    };
    let binary = target.join("debug").join(exe("tilo-relay"));
    eprintln!(
        "[tilo-app] building the workspace's host (tilo-relay); the first build takes a few minutes"
    );
    let mut command = ProcessCommand::new(super::cargo_program());
    command
        .args([
            "build",
            "--locked",
            "-p",
            "tilo-relay",
            "--bin",
            "tilo-relay",
        ])
        .args(["--features", "web,agents"])
        .arg("--manifest-path")
        .arg(project.sdk_root.join("Cargo.toml"))
        .current_dir(&project.sdk_root);
    if !in_workspace {
        command.arg("--target-dir").arg(&target);
    }
    let status = command.status().context("starting the host build")?;
    if !status.success() {
        bail!("building the host failed with {status}");
    }
    Ok(binary)
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Two free loopback ports, for the host's TLS and plain listeners.
fn free_ports() -> Result<[u16; 2]> {
    let a = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let b = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    Ok([a.local_addr()?.port(), b.local_addr()?.port()])
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("no randomness: {error}"))?;
    Ok(bytes)
}

fn write_secret(path: &Path, text: &str) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_page_opens_an_apps_record_and_everyone_else_waits_for_it() {
        let mut records = Records::default();
        let now = Instant::now();
        let first = records.claim("so.example.app", true, now);
        assert_eq!(
            first,
            RecordClaim {
                record: None,
                open: true
            }
        );
        let second = records.claim("so.example.app", true, now);
        assert_eq!(
            second,
            RecordClaim {
                record: None,
                open: false
            },
            "one opens it"
        );
        records.opened("so.example.app", "derived:default@tab:0000000000000001");
        records.opened("so.example.app", "derived:default@tab:0000000000000002");
        let later = records.claim("so.example.app", true, now);
        assert_eq!(
            later.record.as_deref(),
            Some("derived:default@tab:0000000000000001")
        );
        assert!(!later.open);
    }

    #[test]
    fn a_page_that_never_opens_the_record_lets_another_open_it() {
        let mut records = Records::default();
        let now = Instant::now();
        assert!(records.claim("so.example.app", true, now).open);
        let late = now + OPENING_TIMEOUT + Duration::from_secs(1);
        assert!(records.claim("so.example.app", true, late).open);
        assert!(
            !records.claim("so.example.app", false, late).open,
            "asking is not opening"
        );
    }
}
