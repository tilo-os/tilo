//! Several people on several devices each, as real sessions on this machine:
//! a site one person's home peer governs their workspace from, the person
//! whose identity every device is certified under, and a device that runs a
//! headless `tilo-session` of its own.
//!
//! A person is not a device. A person is the root their devices are certified
//! under ([`Person`]); each of their devices is a key and a certificate the
//! root signed ([`Person::device`]). Every device runs a session process of its
//! own, with its own home, its own identity root and its own state, as a
//! device on someone's desk does.
//!
//! A workspace lives at a site ([`Site`]): a relay of this machine that
//! governs it, hosts it for the people in it, admits people at the door it
//! serves the way a site's door admits them, and keeps what its people record
//! while they are away. The site is one person's own, as a home peer is: its
//! device is a device of theirs, the workspace's owner, and the invitations
//! it issues for someone are theirs to issue. A device joins by redeeming an
//! invitation issued for its person, over the collaboration endpoint, as the
//! person's own join does ([`Device::join`]): nothing here bypasses the
//! admission a workspace decides.
//!
//! What a session shows and does is read and driven through the control socket
//! it opens ([`Device::ask`]): [`Device::state`] is the desk as it is now,
//! [`Device::held`] the workspaces it keeps, [`Device::open`] an app the
//! device has installed, and [`Device::act`] any action the session
//! publishes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a device lives until its session ends"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use test_http::{Reply, Responder};
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_relay_protocol::directory::{DoorJoin, Listing, door_path};
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{DeviceCert, IdentityPaths};

/// The invitation text a site issues for a person: a [`tilo_workspace_join`]
/// invitation, as a link carries it.
pub type Invitation = String;

/// A site: a relay of this machine that governs one workspace, hosts it for
/// the people in it and admits people to it at its door.
pub struct Site {
    runtime: tokio::runtime::Runtime,
    relay: Option<tilo_relay::daemon::Relay>,
    /// The workspaces this site governs.
    pub workspaces: tilo_relay::workspaces::Workspaces,
    /// The person this site is: the workspace's owner.
    pub owner: [u8; 32],
    /// The site's device, the one it governs and signs with — a device of
    /// [`Self::owner`], as a home peer is.
    pub host: DeviceCert,
    /// The workspace the site serves here, by id.
    pub workspace: String,
    /// The workspace's name.
    pub name: String,
    /// Where the site is reached: its entry, as a link names it.
    pub entry: String,
    /// The door the site serves, kept up until the site is dropped.
    _door: Responder,
    _state: tempfile::TempDir,
}

impl Drop for Site {
    fn drop(&mut self) {
        if let Some(relay) = self.relay.take() {
            self.runtime.block_on(relay.shutdown());
        }
    }
}

impl Site {
    /// `owner`'s site: a relay of this machine governing the workspace `name`,
    /// listening on loopback with its directory and door served over HTTP.
    pub fn start(owner: &Person, name: &str) -> Self {
        let state = tempfile::tempdir().expect("a directory for the site's host");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the site's host");
        let paths = IdentityPaths::under(state.path().join("identity"));
        std::fs::create_dir_all(state.path().join("identity"))
            .expect("a directory for the site's identity");
        let host = owner.certify_as_host(&paths, "The site");
        let host_key = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
            .expect("the site's device key");
        let (relay, workspaces, workspace) = runtime.block_on(async {
            let relay = tilo_relay::daemon::Relay::start(
                tilo_relay::RelayConfig {
                    state_root: state.path().join("host"),
                    // Devices of this machine find the site on loopback, as
                    // they do on a person's own network.
                    iroh_listen: "127.0.0.1:0".parse().expect("a loopback address"),
                    ..tilo_relay::RelayConfig::default()
                },
                tilo_relay::daemon::RelayIdentity {
                    user_public_key: owner.public,
                    device_signing_key: host_key.clone(),
                    device_cert: host.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                },
            )
            .await
            .expect("the site's host");
            let workspaces = relay
                .workspaces()
                .expect("the site governs workspaces")
                .clone();
            let workspace = workspaces
                .create(name)
                .await
                .expect("the site's workspace")
                .workspace;
            // The site's own device is in the workspace it governs, as a
            // site's is: it hosts what its people record there.
            workspaces
                .enter(&workspace, &host)
                .await
                .expect("the site's device is in its workspace");
            (relay, workspaces, workspace)
        });
        let door = door_responder(&runtime, &workspaces, &workspace, name);
        let entry = format!("{}/", door.url());
        Self {
            runtime,
            relay: Some(relay),
            workspaces,
            owner: owner.public,
            host,
            workspace,
            name: name.to_owned(),
            entry,
            _door: door,
            _state: state,
        }
    }

    /// Make the workspace public with `role` for everyone who joins at the
    /// door, as its owner does: from now on the door admits people.
    pub fn publish(&self, role: WorkspaceRole) {
        self.runtime
            .block_on(self.workspaces.publish(
                &self.workspace,
                &self.host,
                Listing {
                    description: format!("{}'s workspace", self.name),
                    default_role: role,
                },
            ))
            .expect("the workspace is published");
    }

    /// Admit `person` at the site's door, as the person themselves asks it:
    /// the HTTP POST their own join makes, answered by the site's door with an
    /// invitation only they can redeem.
    ///
    /// # Panics
    ///
    /// The door refuses them: a workspace that is not public admits nobody, as
    /// does one that banned them.
    pub fn admit(&self, person: [u8; 32]) -> Invitation {
        self.try_admit(person)
            .unwrap_or_else(|refusal| panic!("the site's door refuses them: {refusal}"))
    }

    /// [`Self::admit`], with the door's refusal given back rather than
    /// asserted on.
    ///
    /// # Errors
    ///
    /// What the door said, when it says no.
    pub fn try_admit(&self, person: [u8; 32]) -> Result<Invitation, String> {
        let client = tilo_net::http::blocking_client()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("a client for the site");
        let answer = client
            .post(format!("{}{}join", self.entry, door_path(&self.workspace)))
            .json(&json!({
                "userPublicKey": hex::encode(person),
                "displayName": "Someone",
            }))
            .send()
            .expect("the site's door answers");
        let status = answer.status();
        let bytes = answer.bytes().expect("the door's answer");
        if !status.is_success() {
            let refusal = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|refusal| refusal["error"].as_str().map(str::to_owned))
                .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
            return Err(refusal);
        }
        let answer: tilo_relay_protocol::directory::DoorAnswer =
            serde_json::from_slice(&bytes).expect("a door's answer");
        Ok(answer.invitation)
    }

    /// Make `person` one of the workspace's `role`, as its owner does: a
    /// moderator invites and removes, an editor writes.
    pub fn set_role(&self, person: [u8; 32], role: WorkspaceRole) {
        self.runtime
            .block_on(self.workspaces.set_member_role(
                &self.workspace,
                &self.host,
                tilo_relay::workspaces::SetMemberRoleRequest {
                    request_id: fresh_id(),
                    user_public_key: person,
                    role: member_role(role),
                },
            ))
            .expect("the workspace's owner sets a member's role");
    }

    /// The invitation the site's owner issues for `person`, as a person
    /// invites someone into their workspace: it binds the person's root, so
    /// only their devices redeem it.
    pub fn invite(&self, person: [u8; 32], role: WorkspaceRole) -> Invitation {
        let issued = self
            .runtime
            .block_on(self.workspaces.issue(
                &self.workspace,
                &self.host,
                tilo_workspace_join::invitations::IssueInvitationRequest {
                    request_id: fresh_id(),
                    role: member_role(role),
                    recipient_user_public_key: Some(person),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                    inviter_name: None,
                },
            ))
            .expect("the workspace's owner issues an invitation");
        tilo_workspace_join::encode_invitation_text(&issued.signed_invitation)
    }

    /// The workspace's people as its host records them: each person's root in
    /// hex, and their role and whether they are removed.
    pub fn members(
        &self,
    ) -> std::collections::BTreeMap<String, tilo_relay::workspaces::MemberMark> {
        self.runtime
            .block_on(self.workspaces.membership(&self.workspace))
            .expect("the site's members")
            .1
    }

    /// Wait until the site's own governance records `person` in the workspace.
    pub fn wait_for_member(&self, person: [u8; 32]) {
        let person = hex::encode(person);
        tilo_test_env::wait_until(format!("the site admits {person}"), || {
            self.members()
                .get(&person)
                .is_some_and(|mark| !mark.removed)
        });
    }

    /// What the site keeps of the record `app`/`record` in the workspace: the
    /// bytes of a document its people wrote, as a device that joins later is
    /// replayed them.
    pub fn record(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        self.runtime
            .block_on(
                self.workspaces
                    .record_document(&self.workspace, app, record),
            )
            .expect("the site's records")
    }

    /// The workspace's shared desktop as the site keeps it now: its strips and
    /// windows, or nothing while the workspace holds no desk of its own.
    pub fn desktop(&self) -> Option<tilo_desktop_model::DesktopTree> {
        let bytes = self.record(
            tilo_desktop_model::ownership::DESKTOP_APP,
            tilo_desktop_model::ownership::DESKTOP_RECORD,
        )?;
        let document = automerge::AutoCommit::load(&bytes).ok()?;
        tilo_desktop_model::shared_record::read(&document, None).map(|desk| desk.tree)
    }

    /// The workspace's shared desktop as the site keeps it, described for a
    /// step that waited for it: how many bytes and changes it holds, and how
    /// many windows read out of it.
    pub fn desk_report(&self) -> String {
        let Some(bytes) = self.record(
            tilo_desktop_model::ownership::DESKTOP_APP,
            tilo_desktop_model::ownership::DESKTOP_RECORD,
        ) else {
            return "the site keeps no desk record".to_owned();
        };
        let Ok(document) = automerge::AutoCommit::load(&bytes) else {
            return format!("{} bytes that do not load", bytes.len());
        };
        let changes = document.get_changes(&[]).len();
        let read = tilo_desktop_model::shared_record::read(&document, None).map_or_else(
            || "no desk reads out of it".to_owned(),
            |desk| {
                let windows = desk
                    .tree
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .map(|strip| strip.window_groups.len())
                    .sum::<usize>();
                format!("{windows} windows in it")
            },
        );
        format!("{} bytes, {changes} changes, {read}", bytes.len())
    }
}

/// The door a site serves: its entry, the directory of what it lists, and the
/// door of the workspace it governs, which admits a person the workspace is
/// public to — the same handlers the production door answers with
/// (`tilo_relay::web`).
fn door_responder(
    runtime: &tokio::runtime::Runtime,
    workspaces: &tilo_relay::workspaces::Workspaces,
    workspace: &str,
    name: &str,
) -> Responder {
    let origin = Arc::new(Mutex::new(String::new()));
    let served_origin = Arc::clone(&origin);
    let workspaces = workspaces.clone();
    let workspace_answered = workspace.to_owned();
    let listed = workspace.to_owned();
    let page_name = name.to_owned();
    let door = workspace.to_owned();
    let behind = format!("/directory/{workspace}/join");
    let answer = runtime.handle().clone();
    let responder = Responder::handle(move |request| {
        let json = |body: String| {
            Some(
                Reply::status(200)
                    .header("content-type", "application/json")
                    .body(body),
            )
        };
        match (request.method.as_str(), request.target.as_str()) {
            ("GET", "/entry.json") => json(
                json!({
                    "directory": "directory",
                    "home": {
                        "name": page_name,
                        "host": { "origin": served_origin.lock().expect("the site's origin").clone() },
                        "admission": "door/",
                    },
                })
                .to_string(),
            ),
            ("POST", "/directory") => {
                let listed = listed.clone();
                json(
                    json!({ "workspaces": [{
                        "workspace": listed,
                        "name": page_name,
                        "description": "",
                        "defaultRole": "editor",
                        "door": door_path(&workspace_answered),
                        "here": [],
                        "hereCount": 0,
                        "members": 1,
                        "lastActiveMs": null,
                        "community": false,
                    }] })
                    .to_string(),
                )
            }
            ("POST", asked) if asked == behind || asked == "/door/join" => {
                let Ok(join) = serde_json::from_slice::<DoorJoin>(&request.body) else {
                    return Some(Reply::status(400));
                };
                let admitted = answer.block_on(
                    workspaces.admit_listed(&door, &join),
                );
                match admitted {
                    Ok((issued, token)) => json(
                        json!({
                            "invitation": tilo_workspace_join::encode_invitation_text(
                                &issued.signed_invitation
                            ),
                            "admissionToken": token,
                        })
                        .to_string(),
                    ),
                    Err(error) => Some(
                        Reply::status(403)
                            .header("content-type", "application/json")
                            .body(json!({ "error": error.to_string() }).to_string()),
                    ),
                }
            }
            _ => Some(Reply::status(404)),
        }
    });
    *origin.lock().expect("the site's origin") = format!("{}/", responder.url());
    responder
}

/// A person: the root their devices are certified under, and the identity this
/// machine provisions them with before any of their devices run.
pub struct Person {
    /// What they go by.
    pub name: String,
    /// Their root, as a workspace names them.
    pub public: [u8; 32],
    root: runtime_identity::SigningIdentity,
    /// The identity the first use of this machine provisioned for them: the
    /// record each of their devices carries, and the root secret devices
    /// certify their own keys with.
    identity: tempfile::TempDir,
}

impl Person {
    /// The person `name`, provisioned as a computer's first use provisions
    /// them.
    pub fn new(name: &str) -> Self {
        let identity = tempfile::tempdir().expect("a directory for the person's identity");
        let config = identity.path().join("config");
        let system = identity.path().join("system");
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("HOME", identity.path().join("home"))
                .set("XDG_CONFIG_HOME", &config)
                .set("XDG_DATA_HOME", identity.path().join("data"))
                .set("XDG_STATE_HOME", identity.path().join("state"))
                .set("XDG_CACHE_HOME", identity.path().join("cache"))
                .set("TILO_IDENTITY_ROOT", &system)
                .set(FILE_SECRETS_ENV, "1");
            tilo_identity_host::provisioning::ensure_collaboration_identity(
                &config.join("tilo/identity"),
                &IdentityPaths::under(&system),
                IdentityStorage::Files,
            )
            .expect("this person's identity");
        }
        let identity_dir = config.join("tilo/identity");
        let record = runtime_identity::resolve_active_identity(&identity_dir)
            .expect("the identity this machine provisioned for the person");
        let root = runtime_identity::load_from_path(record).expect("the person's root key");
        Self {
            name: name.to_owned(),
            public: root.group_public_key(),
            root,
            identity,
        }
    }

    /// Certify the device key `paths` holds under this person's root, as the
    /// person admits a device of theirs: `label` is what it is called, and
    /// the capabilities are the ones the identity layer grants a joined
    /// device.
    pub fn certify(&self, paths: &IdentityPaths, label: &str) -> DeviceCert {
        self.certify_with(paths, label, None)
    }

    /// Certify the device key `paths` holds as this person's host: the device
    /// that governs and hosts workspaces, as a home peer is.
    pub fn certify_as_host(&self, paths: &IdentityPaths, label: &str) -> DeviceCert {
        self.certify_with(
            paths,
            label,
            Some(tilo_user_identity::DeviceCapabilities::host()),
        )
    }

    /// [`Self::certify`], with what the person lets the device do said here
    /// rather than taken from the identity layer.
    fn certify_with(
        &self,
        paths: &IdentityPaths,
        label: &str,
        capabilities: Option<tilo_user_identity::DeviceCapabilities>,
    ) -> DeviceCert {
        let person_root = &self.root;
        let sign = |digest: &[u8]| {
            let digest: [u8; 32] = digest.try_into().map_err(|_| "not a digest")?;
            person_root.sign(&digest).map_err(|error| error.to_string())
        };
        let mut cert = tilo_user_identity::persist::provision_joined_device(
            paths,
            self.public,
            sign,
            label,
            tilo_clock::unix_now_ns(),
        )
        .expect("the device is certified under its person's root");
        if let Some(capabilities) = capabilities {
            // What a person lets this device do is theirs to say; the
            // certificate is signed again over what it now says.
            cert.capabilities = capabilities;
            let digest = tilo_user_identity::device_cert_signing_digest(&cert)
                .expect("a device certificate digest");
            cert.signature = sign(&digest).expect("the person's root signs the device");
            for path in [
                paths.device_cert_for_user(&cert.user_public_key),
                paths.device_cert.clone(),
            ] {
                tilo_user_identity::persist::write_device_cert(&path, &cert)
                    .expect("the device's certificate is written");
            }
        }
        cert
    }

    /// Another of this person's devices, certified under their root as a
    /// device is enrolled: `label` is what it is called.
    pub fn device(&self, label: &str) -> Device {
        let home = tempfile::tempdir().expect("a directory for the device");
        let root = home.path();
        for directory in ["config", "data", "state", "cache", "runtime", "home"] {
            std::fs::create_dir_all(root.join(directory)).expect("a device directory");
        }
        // The record of who the person is, which every device of theirs holds:
        // never the root secret, which stays with the person.
        copy_dir(
            &self.identity.path().join("config/tilo/identity"),
            &root.join("config/tilo/identity"),
        );
        let paths = IdentityPaths::under(root.join("identity"));
        let cert = self.certify(&paths, label);
        Device {
            person: self.name.clone(),
            label: label.to_owned(),
            cert,
            control: root.join("control.sock"),
            home,
            child: None,
        }
    }
}

/// One device of one person, running a session of its own on this machine.
pub struct Device {
    /// The person whose device this is.
    pub person: String,
    /// What the device is called.
    pub label: String,
    /// The certificate its person's root signed for it, as a workspace admits
    /// it.
    pub cert: DeviceCert,
    home: tempfile::TempDir,
    child: Option<Child>,
    control: PathBuf,
}

impl Device {
    /// Start this device's session without joining anything: a session on
    /// someone's desk before they join a workspace.
    pub fn start(&mut self) {
        self.spawn(None);
    }

    /// Start this device's session joining the workspace `invitation` grants
    /// it, as a device the host started for a workspace does: the real join,
    /// redeemed over the collaboration endpoint.
    pub fn join(&mut self, invitation: &str) {
        self.spawn(Some(invitation));
    }

    /// Stop this device's session, as the person closing their laptop does.
    pub fn stop(&mut self) {
        let Some(child) = self.child.take() else {
            return;
        };
        // The session leads its own process group: an ending reaches everything
        // it started.
        let pid = i32::try_from(child.id()).unwrap_or(0);
        if pid > 0 {
            // SAFETY: a signal to a process group this test started.
            unsafe {
                libc::killpg(pid, libc::SIGTERM);
            }
        }
        let mut child = child;
        let deadline = Instant::now().checked_add(Duration::from_secs(20));
        loop {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                // SAFETY: as above.
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
                let _ = child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The device's own directory: its home, its config, its state and the
    /// identity root its session runs on.
    pub fn directory(&self) -> &Path {
        self.home.path()
    }

    /// The identity this device holds: the person's root's public key, the
    /// device's own key and the certificate the root signed for it.
    pub fn identity(&self) -> IdentityPaths {
        IdentityPaths::under(self.home.path().join("identity"))
    }

    /// The device's own session file: what it logged, when something went
    /// wrong.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.home.path().join("session.log")).unwrap_or_default()
    }

    /// Read this device's session log, so a failed step says why.
    fn read_line_of_log(&self) -> String {
        let log = self.log();
        log.lines().rev().take(20).collect::<Vec<_>>().join("\n")
    }

    /// Run this device's session: its own home, its own identity root, its own
    /// state and its own control socket, as a device's session runs.
    fn spawn(&mut self, invitation: Option<&str>) {
        let root = self.home.path().to_path_buf();
        let control = self.control.clone();
        let log = std::fs::File::create(root.join("session.log")).expect("a log for the session");
        let mut command = Command::new(env!("CARGO_BIN_EXE_tilo-session"));
        command
            .args([
                "--backend",
                "headless",
                // The person's desk, with the shell that shows it: what a
                // device's session mounts and what [`Device::open`] opens
                // into.
                "--shell",
                "screen",
                "--no-x11",
                "--session-bus",
                "none",
                "--system-bus",
                "none",
                "--test-control",
                control.to_str().expect("a socket path"),
            ])
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env("TILO_IDENTITY_ROOT", root.join("identity"))
            .env(FILE_SECRETS_ENV, "1")
            // The session says at info why a join did not land: what a failed
            // step reads.
            .env("RUST_LOG", "info")
            .env_remove("TILO_TEST_CONTROL")
            .env_remove("TILO_AGENT_DOOR")
            .env_remove("TILO_ADMIN_CONTROL")
            .env_remove("TILO_SHELL_JOIN_INVITATION_FILE")
            .env_remove("TILO_SHELL_JOIN_ADMINISTRATOR")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            // A stop of the whole group reaches it, as a session is stopped.
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log.try_clone().expect("the log's own handle")));
        if let Some(invitation) = invitation {
            let path = root.join("invitation.txt");
            std::fs::write(&path, invitation).expect("the invitation is written");
            command.env("TILO_SHELL_JOIN_INVITATION_FILE", path);
        }
        let mut child = command.spawn_locked().expect("the session starts");
        let mut out = BufReader::new(child.stdout.take().expect("the session's output"));
        // What the session prints once it listens, the last thing it prints.
        let listening = format!("TILO_TEST_CONTROL={}", control.display());
        let (tell, told) = mpsc::channel();
        let mut logged = log.try_clone().expect("the log's own handle");
        std::thread::spawn(move || {
            let mut line = String::new();
            let mut announced = false;
            loop {
                line.clear();
                match out.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if !announced && line.trim_end() == listening {
                    announced = true;
                    let _ = tell.send(true);
                }
                let _ = writeln!(logged, "{}", line.trim_end());
            }
            let _ = tell.send(false);
        });
        match told.recv_timeout(tilo_test_env::hang_bound()) {
            Ok(true) => {}
            Ok(false) | Err(_) => panic!(
                "{}'s session never opened its control socket:\n{}",
                self.label,
                String::from_utf8_lossy(
                    &std::fs::read(root.join("session.log")).unwrap_or_default()
                )
            ),
        }
        self.child = Some(child);
    }

    /// Ask the device's session `request`, as `tilo-control` does, and return
    /// its answer.
    pub fn ask(&self, request: &Value) -> Value {
        let mut stream = UnixStream::connect(&self.control)
            .unwrap_or_else(|error| panic!("{}'s control socket: {error}", self.label));
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("a read timeout");
        stream
            .write_all(format!("{request}\n").as_bytes())
            .expect("the socket reads");
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .expect("an answer");
        serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!(
                "{}'s answer {line:?}: {error}\n{}",
                self.label,
                self.read_line_of_log()
            )
        })
    }

    /// The desk as this device shows it now: the tree, the workspaces its
    /// engine holds, its agents and its chrome.
    pub fn state(&self) -> Value {
        self.ask(&json!({"op": "state_dump"}))
    }

    /// The workspaces this device keeps, by id: a workspace it joined is one
    /// of them.
    pub fn held(&self) -> Vec<String> {
        self.state()["state"]["held_workspaces"]
            .as_array()
            .map(|held| {
                held.iter()
                    .filter_map(|workspace| workspace["room"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Wait until this device keeps the workspace `workspace`, failing with
    /// what its session logged if it never does.
    pub fn wait_holding(&self, workspace: &str) {
        let deadline = tilo_test_env::hang_deadline();
        let mut pause = Duration::from_millis(10);
        loop {
            let held = self.held();
            if held.iter().any(|held| held == workspace) {
                return;
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "{} never kept {workspace}; it holds {held:?}:\n{}",
                self.label,
                self.log(),
            );
            std::thread::sleep(pause.min(deadline.saturating_duration_since(now)));
            pause = pause.saturating_mul(2).min(Duration::from_millis(500));
        }
    }

    /// The actions this device publishes now.
    pub fn actions(&self) -> Value {
        self.ask(&json!({"op": "actions_enumerate"}))
    }

    /// Map a window of `app` called `title` on this device's desk, as an app
    /// starting its own window does: a window of the workspace it shows, kept
    /// up while the returned value lives.
    pub fn window(&self, app: &str, title: &str) -> super::window::Window {
        super::window::Window::open(self.directory(), app, title)
    }

    /// Invoke `id` of `provider` on this device, as a program of the person's
    /// does, and return the receipt.
    pub fn act(&self, provider: &str, id: &str, args: &Value) -> Value {
        self.ask(&json!({
            "op": "actions_invoke",
            "provider": provider,
            "id": id,
            "args": args,
        }))
    }

    /// Press the control named `named`, as an assistive technology activates
    /// it.
    pub fn press(&self, named: &str) {
        let answer = self.ask(&json!({"op": "click_named", "name": named}));
        assert!(
            answer.get("err").is_none(),
            "{} presses {named}: {answer}",
            self.label
        );
    }

    /// Type `text` on this device, as the person at its keyboard does.
    pub fn type_text(&self, text: &str) {
        let answer = self.ask(&json!({"op": "type", "text": text}));
        assert!(
            answer.get("err").is_none(),
            "{} types: {answer}",
            self.label
        );
    }

    /// Wait until this device's desk has `window`, a window's title, on it.
    pub fn wait_for_window(&self, window: &str) {
        tilo_test_env::wait_until(format!("{} shows {window}", self.label), || {
            let windows = self.ask(&json!({"op": "windows"}));
            windows.to_string().contains(window)
        });
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The role a member holds in a workspace, as [`WorkspaceRole`] names the
/// workspace's listing of it.
fn member_role(role: WorkspaceRole) -> tilo_workspace_join::MemberRole {
    match role {
        WorkspaceRole::Viewer => tilo_workspace_join::MemberRole::Viewer,
        WorkspaceRole::Commenter => tilo_workspace_join::MemberRole::Commenter,
        WorkspaceRole::Editor => tilo_workspace_join::MemberRole::Editor,
        WorkspaceRole::Moderator => tilo_workspace_join::MemberRole::Moderator,
    }
}

/// An id of this test's own, distinct from every other it makes: what a request
/// is remembered by.
fn fresh_id() -> [u8; 32] {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let digest = blake3::hash(format!("id/{}/{n}", std::process::id()).as_bytes());
    let mut id = [0; 32];
    id.copy_from_slice(digest.as_bytes());
    id
}

/// Copy the tree `from` to `to`, keeping the modes files are read with.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a copied directory");
    for entry in std::fs::read_dir(from).expect("a readable directory") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        let kind = entry.file_type().expect("an entry's kind");
        if kind.is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("a copied file");
            let mode = std::fs::metadata(&target).expect("a copied file's mode");
            let mut permissions = mode.permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o600);
            std::fs::set_permissions(&target, permissions).expect("a mode");
        }
    }
}
