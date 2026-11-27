//! Several people on several devices each, as real sessions on this machine:
//! a site one person's home peer hosts their workspace from, the person
//! whose identity every device is certified under, and a device that runs a
//! headless `tilo-session` of its own.
//!
//! A person is not a device. A person is the root their devices are certified
//! under ([`Person`]), which their first device makes, founding their own
//! workspace as it does; each later device of theirs is a key and a certificate
//! the root signed, let in with a ticket from that first device
//! ([`Person::device`]). Every device runs as a device does: its
//! own daemon (`tilo-runtime`, the person's own service) with its own home, its
//! own identity root and its own state, holding the device's endpoint on
//! loopback, and the session that daemon starts as the person's shell, on the
//! link it made for it.
//!
//! A workspace lives at a site ([`Site`]): a server of this machine that
//! hosts it for the people in it, admits people at the entry it
//! serves the way a site's entry admits them, and keeps what its people record
//! while they are away. The site is one person's own, as a home peer is: its
//! device is a device of theirs, the workspace's owner, and the invitations
//! it issues for someone are theirs to issue. A device joins by redeeming an
//! invitation issued for its person, or through the workspace's open entry by
//! its link, as the person's own join does ([`Device::join`]): its daemon signs
//! the admission with the device's key, and nothing here bypasses the admission
//! a workspace decides.
//!
//! What a session shows and does is read and driven through the control socket
//! it opens ([`Device::ask`]): [`Device::state`] is the shell as it is now,
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

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use test_http::{Reply, Responder};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    CatalogueQuery, CatalogueRow, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_device_identity::IdentityPaths;
use tilo_device_identity::login::daemon::{DeviceActions, DeviceRefusal};
use tilo_device_identity::login::{ALREADY_A_PERSONS, FOUND};
use tilo_identity::provisioning::{FILE_SECRETS_ENV, TEST_PLATFORM_ENV, TILO_OS_PLATFORM};
use tilo_server_protocol::directory::{EntryJoin, Listing, entry_path};
use tilo_server_protocol::workspaces::Role;
use tilo_test_env::DiesWithTheTest as _;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::{EntryRefusal, Listed};

/// The invitation text a site issues for a person: a [`tilo_workspace_join`] invitation, as a link
/// carries it.
pub type Invitation = String;

/// Whether the sessions of this process's devices run the development session's tilolang apps,
/// seeded with each one's first scenario, as `tilolang run` starts them.
static SEED_APPS: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Run the development session's tilolang apps in every session this test starts from here on
/// (`--seed-apps`): Notes, Tasks, Chat, Mail and Tube, installed in each device's store
/// ([`crate::common::dev_apps::install`]) and seeded with their first scenarios, so their
/// resources and actions are each device's as they are in a development session. What a test asks
/// for before it makes its devices.
pub fn seed_the_apps() {
    let _ = SEED_APPS.set(());
}

/// The log filter the sessions of this process's devices start under, when a test asked for more
/// than the fixture's own `info`. Each test runs in a process of its own, so the filter is that
/// test's.
static SESSION_LOG: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Start every session this test starts from here on under the log filter `filter`, a `RUST_LOG`:
/// what a test asks for before it makes its devices, when a step that fails has to say what a
/// session only says below `info`.
pub fn log_sessions_at(filter: &str) {
    let _ = SESSION_LOG.set(filter.to_owned());
}

/// How often a device's window shares ask the device sharing a window again for what it shows,
/// and how long an ask keeps that device's streams up: the production periods unless a test says
/// otherwise, as the session reads them ([`tilo_session::VIEW_AGAIN_ENV`]).
static SHARE_VIEW: std::sync::OnceLock<(Duration, Duration)> = std::sync::OnceLock::new();

/// Have every session this test starts from here on renew the windows of a workspace at `again`
/// and keep a viewer's streams up for `expiry` instead of the production periods: what a test
/// that proves a share outlives the waiting for the viewer to ask again watches in a fraction of
/// a second, rather than spending the production seconds on it. Called before a test makes its
/// devices.
pub fn share_view_periods(again: Duration, expiry: Duration) {
    let _ = SHARE_VIEW.set((again, expiry));
}

/// How long a device is given to hold the workspace it joined and to show it in front: the join is
/// the product's own, which gives it [`tilo_session::sync::JOIN_PATIENCE`] before the person is told
/// it did not happen, and [`JOIN_OUTCOME_READ`] after that to read the outcome it settled on.
///
/// The whole hang bound after the patience would outlast the test: nextest's ci profile stops a
/// test after eight minutes, and a device's setup before its join can take minutes of that on a
/// busy machine. A join that never took would then be killed before its wait could say why, with
/// the device's log, and every test that joins would fail without a message.
pub fn joined_within() -> Duration {
    tilo_session::sync::JOIN_PATIENCE
        .saturating_add(tilo_test_env::hang_bound().min(JOIN_OUTCOME_READ))
}

/// How long after the join's own patience a test reads what the device settled on: by then the
/// device has held the workspace or told the person the join did not happen, so this only covers
/// its session answering on a loaded machine.
const JOIN_OUTCOME_READ: Duration = Duration::from_secs(30);

/// A site: a server of this machine that hosts one workspace for the people in it and
/// admits people to it at its entry.
pub struct Site {
    runtime: tokio::runtime::Runtime,
    server: Option<tilo_runtime::device::keep::server::KeptServer>,
    /// The workspaces this site hosts.
    pub workspaces: tilo_workspace_service::workspaces::Workspaces,
    /// The person this site is: the workspace's owner.
    pub owner: [u8; 32],
    /// The site's device, the one it signs with — a device of [`Self::owner`], as a
    /// home peer is.
    pub host: DeviceCert,
    /// The workspace the site serves here, by id.
    pub workspace: String,
    /// The workspace's name.
    pub name: String,
    /// Where the site is reached: its entry, as a link names it.
    pub entry: String,
    /// The role the entry admits people at, as [`Self::publish`] set it: what the site's owner
    /// invites a device at when the entry itself takes the person in.
    published: Mutex<Option<Role>>,
    /// The entry the site serves, kept up until the site is dropped.
    _entry: Responder,
    state: tempfile::TempDir,
}

impl Drop for Site {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Site {
    /// Stop the site's host, as a site that went away: what its people's devices hold of the
    /// workspace, they read of their own.
    pub fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            self.runtime.block_on(server.shutdown());
        }
    }

    /// `owner`'s site: a server of this machine hosting the workspace `name`,
    /// listening on loopback with its directory and entry served over HTTP.
    pub fn start(owner: &Person, name: &str) -> Self {
        // The site runs in this process: its log is this test's stderr.
        install_log();
        // The site keeps the keys of the channels it seals in the platform secret store, which this
        // process keeps in memory: a site without one can host no sealed record, and its people's
        // devices reach no app wire.
        tilo_secrets::keep_in_memory_for_process();
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
        let host_key = tilo_device_identity::persist::read_device_key(&paths.device_privkey)
            .expect("the site's device key");
        let (server, workspaces, workspace) = runtime.block_on(async {
            let server = tilo_runtime::device::keep::server::KeptServer::with_relay_mode(
                tilo_server_protocol::config::ServerConfig {
                    state_root: state.path().join("host"),
                    ..tilo_server_protocol::config::ServerConfig::default()
                }
                .on_loopback(),
                tilo_runtime::device::keep::server::ServerIdentity {
                    user_public_key: owner.public,
                    device_signing_key: host_key.clone(),
                    device_cert: host.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                },
                // The site relays through this machine's loopback relay, as its people's devices
                // do: neither side reaches number 0.
                test_server::relay_mode(),
            )
            .await
            .expect("the site's host");
            let workspaces = server
                .workspaces()
                .expect("the site hosts workspaces")
                .clone();
            let workspace = workspaces
                .create(name)
                .await
                .expect("the site's workspace")
                .workspace;
            // The site's own device is in the workspace it hosts, as a site's is: it hosts what
            // its people record there.
            workspaces
                .enter(&workspace, (&host, &host_key))
                .await
                .expect("the site's device is in its workspace");
            (server, workspaces, workspace)
        });
        // The directory publishes who the site's host is and where it is dialled, as a site's
        // does: a device entering through the entry reaches the host from that.
        let published_host = tilo_server_protocol::web::PageHost {
            server_cert: host.clone(),
            address: Some(tilo_server_protocol::web::PageHostAddress {
                endpoint_addr: server.node_addr(),
                relay_url: None,
                web: None,
            }),
            origin: None,
        };
        let responder = entry_responder(&workspace, name, &published_host);
        let entry = format!("{}/", responder.url());
        Self {
            runtime,
            server: Some(server),
            workspaces,
            owner: owner.public,
            host,
            workspace,
            name: name.to_owned(),
            entry,
            published: Mutex::new(None),
            _entry: responder,
            state,
        }
    }

    /// Delete the workspace, as its owner does: final, for everyone in it.
    pub fn delete(&self) {
        self.runtime
            .block_on(self.workspaces.delete(&self.workspace))
            .expect("the site deletes its workspace");
    }

    /// Where the site's server is reached, as a link to something it hands out names it.
    pub fn node_addr(&self) -> iroh::EndpointAddr {
        self.server
            .as_ref()
            .expect("the site's server runs")
            .node_addr()
    }

    /// The workspace `workspace` as the site's host carries it, by its id here: what a machine an
    /// environment runs in is handed, and what a page action names.
    pub fn workspace_id(&self, workspace: &str) -> Option<tilo_desktop_model::WorkspaceId> {
        self.runtime
            .block_on(self.workspaces.summary(workspace))
            .ok()
            .map(|summary| summary.workspace_id)
    }

    /// Make the workspace public with `role` for everyone who joins at the entry, as its owner does:
    /// from now on the entry admits people.
    pub fn publish(&self, role: Role) {
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
        *self.published.lock().expect("the site's published role") = Some(role);
    }

    /// Admit `person` at the site's entry, as their own entry through it asks
    /// it, and answer what their session joins by: the workspace's link, where
    /// the entry takes anyone who asks, or the invitation only they can redeem.
    ///
    /// A device that asks an open entry signs its own admission under the entry
    /// the workspace records, in its daemon, the process that holds its key: the
    /// session hands the link to the daemon's join, which enters through the
    /// entry at the host the site's directory publishes.
    ///
    /// # Panics
    ///
    /// The entry refuses them: a workspace that is not public admits nobody, as
    /// does one that banned them.
    pub fn admit(&self, person: [u8; 32]) -> Invitation {
        self.try_admit(person)
            .unwrap_or_else(|refusal| panic!("the site's entry refuses them: {refusal}"))
    }

    /// [`Self::admit`], with the entry's refusal given back rather than
    /// asserted on.
    ///
    /// # Errors
    ///
    /// What the entry said, when it says no.
    pub fn try_admit(&self, person: [u8; 32]) -> Result<Invitation, String> {
        let listed = self.runtime.block_on(self.workspaces.admit_listed(
            &self.workspace,
            &EntryJoin {
                user_public_key: hex::encode(person),
                display_name: "Someone".to_owned(),
            },
        ));
        let role = || {
            self.published
                .lock()
                .expect("the site's published role")
                .expect("an entry admits people once it is published")
        };
        match listed {
            Ok(Listed::Invited(issued)) => Ok(tilo_workspace_join::encode_invitation_text(
                &issued.signed_invitation,
            )),
            Ok(Listed::Open) => Ok(self.link()),
            // The entry tells a person it already has in to add the device from a device of
            // theirs that is in: the site's owner invites it, and the entry still refuses it.
            Err(EntryRefusal::Refused(said))
                if said == tilo_peer_state::workspace::membership::ENTRY_NAMES_ANOTHER_DEVICE =>
            {
                Ok(self.invite(person, role()))
            }
            Err(refusal) => Err(refusal.to_string()),
        }
    }

    /// The workspace's link, carrying no invitation, under the site's entry: what someone hands a
    /// person to a public workspace, joined through its open entry.
    pub fn link(&self) -> String {
        use tilo_reference::link::{
            HostOrigin, Link, LinkSecrets, LinkTarget, Mode, Scope, WebEntry,
        };
        let entry = WebEntry::new(&self.entry).expect("the site's entry is a web entry");
        let host = HostOrigin::new(self.entry.trim_end_matches('/')).expect("the site's origin");
        let id = uuid::Uuid::parse_str(&self.workspace).expect("the workspace's id is a UUID");
        Link {
            mode: Mode::Desktop,
            target: LinkTarget::new(Scope::Workspace {
                host,
                id,
                slug: None,
            }),
            secrets: LinkSecrets::default(),
        }
        .to_url(&entry)
        .expect("the workspace's link")
    }

    /// Make `person` one of the workspace's `role` , as its owner does: a moderator invites and
    /// removes, an editor writes.
    pub fn set_role(&self, person: [u8; 32], role: Role) {
        self.runtime
            .block_on(self.workspaces.set_member_role(
                &self.workspace,
                &self.host,
                tilo_workspace_service::workspaces::SetMemberRoleRequest {
                    request_id: fresh_id(),
                    user_public_key: person,
                    role,
                },
            ))
            .expect("the workspace's owner sets a member's role");
    }

    /// The invitation the site's owner issues for `person` , as a person invites someone into their
    /// workspace: it binds the person's root, so only their devices redeem it.
    pub fn invite(&self, person: [u8; 32], role: Role) -> Invitation {
        let issued = self
            .runtime
            .block_on(self.workspaces.issue(
                &self.workspace,
                &self.host,
                tilo_workspace_join::invitations::IssueInvitationRequest {
                    request_id: fresh_id(),
                    role,
                    recipient_user_public_key: Some(person),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                    inviter_name: None,
                },
            ))
            .expect("the workspace's owner issues an invitation");
        tilo_workspace_join::encode_invitation_text(&issued.signed_invitation)
    }

    /// The workspace's people as its host records them: each person's root in hex, and their role
    /// and whether they are removed.
    pub fn members(
        &self,
    ) -> std::collections::BTreeMap<String, tilo_workspace_service::workspaces::MemberMark> {
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

    /// Remove `person` and every device of theirs from the workspace, as its owner does: from now
    /// on the site hosts nothing of the workspace for them.
    pub fn remove(&self, person: [u8; 32]) {
        self.runtime
            .block_on(self.workspaces.remove_member(
                &self.workspace,
                &self.host,
                tilo_workspace_service::workspaces::RemoveMemberRequest {
                    request_id: fresh_id(),
                    user_public_key: person,
                },
            ))
            .expect("the workspace's owner removes a member");
    }

    /// Wait until the workspace's governance records `person` as removed.
    pub fn wait_for_removal(&self, person: [u8; 32]) {
        let person = hex::encode(person);
        tilo_test_env::wait_until(format!("the site removes {person}"), || {
            self.members().get(&person).is_some_and(|mark| mark.removed)
        });
    }

    /// The workspace as the site's governance summarises it now: its people, their devices and
    /// the generation its governance is at.
    pub fn runtime_summary(
        &self,
    ) -> Result<tilo_server_protocol::workspaces::WorkspaceSummary, String> {
        self.runtime
            .block_on(self.workspaces.summary(&self.workspace))
            .map_err(|error| error.to_string())
    }

    /// The records of the workspace this site holds, by the app and record they belong to: what its
    /// people wrote there, as their host keeps it.
    pub fn records(&self) -> Vec<(String, String)> {
        self.runtime
            .block_on(self.workspaces.document_routes(&self.workspace))
            .expect("the site's records")
    }

    /// What the site's copy of record `record` of `app` holds, saved, read through the keys it
    /// holds as a member of the record's channel: `None` while it holds no such record, or cannot
    /// open it yet.
    pub fn record(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        self.runtime
            .block_on(
                self.workspaces
                    .resource_document(&self.workspace, app, record),
            )
            .ok()
            .flatten()
    }

    /// Wait until the site holds a record of `app` : what a window of `device` , which wrote in it,
    /// had reach the workspace's host. What that device's session and app said comes with the
    /// failure: a write that never left its device says why there.
    pub fn wait_for_record_of(&self, app: &str, device: &Device) {
        let held = Held {
            site: self,
            app,
            device: Some(device),
        };
        tilo_test_env::wait_until(held, || {
            self.records()
                .iter()
                .any(|(recorded, _)| recorded.as_str() == app)
        });
    }

    /// The workspace's shared desktop as the site keeps it now: its strips and windows, or nothing
    /// while the workspace holds no shell of its own.
    pub fn desktop(&self) -> Option<tilo_desktop_model::Layout> {
        let bytes = self.record(
            tilo_desktop_model::ownership::DESKTOP_APP,
            tilo_desktop_model::ownership::DESKTOP_DOCUMENT,
        )?;
        let document = automerge::AutoCommit::load(&bytes).ok()?;
        tilo_desktop_model::shared_resource::read(&document, None).map(|shell| shell.tree)
    }

    /// The verdicts the workspace holds on writes, as the site keeps them
    /// now, described for a step that waited for one: what the site refused,
    /// of whose write, and why.
    pub fn refusals_report(&self) -> String {
        let held = self.runtime.block_on(async {
            self.workspaces
                .state(&self.workspace)
                .await
                .map(|state| state.workspace().write_refusals())
        });
        match held {
            Some(refusals) if refusals.is_empty() => "no refusal of any write".to_owned(),
            Some(refusals) => format!("{refusals:#?}"),
            None => "the site holds no copy of the workspace".to_owned(),
        }
    }
}

/// What the site holds, named on the wait that found it short: the routes it kept are the
/// difference between a write that never left its device and one the host has but nobody reads.
/// With the device that wrote, its session's and its app's own words follow.
struct Held<'a> {
    site: &'a Site,
    app: &'a str,
    device: Option<&'a Device>,
}

impl std::fmt::Display for Held<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "the site to hold a record of {} (it holds {:?})",
            self.app,
            self.site.records()
        )?;
        if let Some(device) = self.device {
            write!(formatter, "\n{}", device.diagnostic())?;
        }
        Ok(())
    }
}

/// How many windows the workspace's shared shell holds, as the site keeps it.
pub fn windows(site: &Site) -> usize {
    titles_at(site).len()
}

/// Whether `device` holds the workspace `site` serves.
fn holds_workspace(device: &Device, site: &Site) -> bool {
    device.held().contains(&site.workspace)
}

/// Bring the workspace `site` serves to the front on `device`'s shell, so an app it opens is a
/// window of that workspace's shared shell.
pub fn show(device: &Device, site: &Site) {
    tilo_test_env::wait_until(format!("{} holds the workspace", device.label), || {
        holds_workspace(device, site)
    });
    let shown = device.act(
        &at("so.tilo.session", "desktop:chrome", "switch-workspace"),
        &json!({"workspace": site.workspace}),
    );
    assert!(
        shown.get("err").is_none(),
        "{} shows {}: {shown}\n{}",
        device.label,
        site.name,
        device.log()
    );
}

/// The window titles `device` shows on the workspace `site` serves: the shared shell it reads out of
/// the workspace.
pub fn titles_on(device: &Device, site: &Site) -> Vec<String> {
    let state = device.state();
    let tree: tilo_desktop_model::Layout =
        serde_json::from_value(state["state"]["workspace_tree"].clone()).unwrap_or_else(|error| {
            panic!("{}'s shell does not read: {error}\n{state}", device.label)
        });
    let mut titles: Vec<String> = tree
        .workspaces
        .iter()
        .filter(|workspace| workspace.id().to_string() == site.workspace)
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.title.clone())
        .collect();
    titles.sort_unstable();
    titles
}

/// The window titles the site keeps on the workspace's shared shell.
pub fn titles_at(site: &Site) -> Vec<String> {
    site.desktop().map_or_else(Vec::new, |shell| {
        let mut titles: Vec<String> = shell
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.title.clone())
            .collect();
        titles.sort_unstable();
        titles
    })
}

/// Wait until the site keeps a window titled `title` on the workspace's shell and every device in
/// `devices` shows it: a record a device wrote, read where it has to arrive.
pub fn wait_shown(site: &Site, devices: &[&Device], title: &str) {
    waited(
        &format!("every device to show {title:?}"),
        site,
        devices,
        || {
            titles_at(site).iter().any(|kept| kept == title)
                && devices
                    .iter()
                    .all(|device| titles_on(device, site).iter().any(|shown| shown == title))
        },
    );
}

/// Wait until `condition` holds, and if it never does, fail with what the site keeps and what the
/// devices showed and logged: a write that never left its device says why there.
pub fn waited(what: &str, site: &Site, devices: &[&Device], condition: impl FnMut() -> bool) {
    let watched = Watched {
        what,
        site,
        devices,
    };
    tilo_test_env::wait_until(watched, condition);
}

/// The step [`waited`] waits for, named on the failure that found it short: what the site keeps,
/// and what each device shows and logged.
struct Watched<'a> {
    what: &'a str,
    site: &'a Site,
    devices: &'a [&'a Device],
}

impl std::fmt::Display for Watched<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}; the site keeps {:?} and refuses: {}\nthe devices:\n",
            self.what,
            titles_at(self.site),
            self.site.refusals_report()
        )?;
        for device in self.devices {
            writeln!(
                formatter,
                "{} shows {:?}, with {:?} in front of {:?}\n{}",
                device.label,
                titles_on(device, self.site),
                device.showing(),
                device.on_shell(),
                device.diagnostic()
            )?;
        }
        Ok(())
    }
}

/// Two people in one workspace Ada's site hosts, both editors, each at a device of their own: the
/// site lets its shell author the workspace's records, as a workspace's host does before its people
/// open windows there.
pub fn two_people_at_a_site() -> (Site, Person, Person, Device, Device) {
    two_people_at_a_site_carrying(&[], |_| {})
}

/// The app whose record a workspace's tree is.
pub const FILES_APP: &str = "so.tilo.file-explorer";

/// The example app whose one checklist is the workspace's record: a hand-written Rust app.
pub const SHARED_CHECKLIST: &str = "so.tilo.example.shared-checklist";

/// Poker, a default Rust app: its one table is the workspace's record.
pub const POKER: &str = "so.tilo.poker";

/// The two people's laptops in the site's workspace, each carrying what `install` puts in its
/// store before its session starts, and the workspace letting `authored` author its records.
pub fn two_people_at_a_site_carrying(
    authored: &[&str],
    install: impl Fn(&Device),
) -> (Site, Person, Person, Device, Device) {
    let (site, ada, bob, mut ada_laptop) = site_carrying(authored, &install);
    let mut bob_laptop = bob.device("Bob's laptop");
    install(&bob_laptop);
    // Both people join a workspace its host's own entry admits them to: neither join needs the
    // other's device, so both sessions start before either is waited for.
    let hers = site.invite(ada.public, Role::Editor);
    let theirs = site.invite(bob.public, Role::Editor);
    join_together(
        &mut [&mut ada_laptop, &mut bob_laptop],
        &[hers.as_str(), theirs.as_str()],
    );
    for device in [&ada_laptop, &bob_laptop] {
        device.wait_holding(&site.workspace);
    }
    site.wait_for_member(bob.public);
    (site, ada, bob, ada_laptop, bob_laptop)
}

/// [`two_people_at_a_site`] with the Files app, before the second person joins: Ada's laptop holds
/// the workspace, and what brings Bob in is the invitation her session hands out for it.
pub fn a_site_with_files_for_one_laptop() -> (Site, Person, Person, Device) {
    let (site, ada, bob, mut ada_laptop) = site_carrying(&[FILES_APP], &Device::install_files);
    let hers = site.invite(ada.public, Role::Editor);
    ada_laptop.join(&hers);
    ada_laptop.wait_holding(&site.workspace);
    (site, ada, bob, ada_laptop)
}

/// The site the two people share and the first person's laptop made, carrying what `install` puts
/// in its store, before its session starts: the caller starts it, beside whatever it starts with
/// it.
fn site_carrying(_apps: &[&str], install: &impl Fn(&Device)) -> (Site, Person, Person, Device) {
    let ada = Person::new("Ada");
    let bob = Person::new("Bob");
    let site = Site::start(&ada, "Studio");
    let ada_laptop = ada.device("Ada's laptop");
    install(&ada_laptop);
    (site, ada, bob, ada_laptop)
}

/// The invitation `device`'s session hands out for the workspace it holds, made by the shell's own
/// action at `role` for `person`: what an app shares, and what only `person`'s devices redeem.
pub fn shared_invitation(device: &Device, role: &str, person: [u8; 32]) -> Invitation {
    let answer = shell_answer(
        device,
        &json!({"role": role, "recipient": hex::encode(person)}),
    );
    let update = &answer["update"];
    assert_eq!(
        update["state"],
        json!("succeeded"),
        "{} shares an invitation: {answer}\n{}",
        device.label,
        device.diagnostic()
    );
    update["output"]["value"]["link"]
        .as_str()
        .unwrap_or_else(|| panic!("{}'s invitation carries its text: {answer}", device.label))
        .to_owned()
}

/// The answer `device`'s shell gives when asked to hand out an invitation into the workspace it
/// shows, as the app sharing what is on screen asks it.
pub fn shell_answer(device: &Device, args: &serde_json::Value) -> serde_json::Value {
    device.act_wait(
        &at(
            tilo_actions::remote::session_actions::PROVIDER,
            tilo_actions::remote::session_actions::INVITE_RESOURCE,
            tilo_actions::remote::session_actions::ISSUE_INVITATION,
        ),
        args,
    )
}

/// One person with two devices of their own in one workspace their site hosts, both editors: the
/// two sessions a person's laptop and their phone run, sharing what they record there.
pub fn two_devices_of_one_person() -> (Site, Person, Device, Device) {
    two_devices_of_one_person_carrying(|_| {})
}

/// [`two_devices_of_one_person`] for two devices that carry the development session's tilolang
/// apps, as `tilolang run` leaves them; [`seed_the_apps`] runs them.
pub fn two_devices_of_one_person_with_apps() -> (Site, Person, Device, Device) {
    let (site, ada, laptop, phone) = two_devices_of_one_person_carrying(|device| {
        crate::common::dev_apps::install(&device.store(), &device.store().join("scratch"));
    });
    // Seeded apps start in each session's own store and move into the workspace once the device
    // holds its keys: what a step does to them before then stays behind.
    if SEED_APPS.get().is_some() {
        for device in [&laptop, &phone] {
            device.wait_apps_kept();
        }
    }
    (site, ada, laptop, phone)
}

/// The person's laptop and phone, each carrying what `install` puts in its store: both are made
/// first, then told of the person's fleet, then started together — a session reads the apps and
/// the devices of the person it has when it starts.
fn two_devices_of_one_person_carrying(
    install: impl Fn(&mut Device),
) -> (Site, Person, Device, Device) {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    let mut laptop = ada.device("Ada's laptop");
    let mut phone = ada.device("Ada's phone");
    for device in [&mut laptop, &mut phone] {
        install(device);
    }
    let hers = site.invite(ada.public, Role::Editor);
    let hers_phone = site.invite(ada.public, Role::Editor);
    join_together(
        &mut [&mut laptop, &mut phone],
        &[hers.as_str(), hers_phone.as_str()],
    );
    for device in [&laptop, &phone] {
        device.wait_holding(&site.workspace);
    }
    (site, ada, laptop, phone)
}

/// Start `devices`' sessions together: every session is started before any is waited for, so the
/// devices boot at once rather than one boot after another. Wait for what each needs of its
/// workspace after this returns ([`Device::wait_holding`], [`Device::wait_showing`]).
pub fn start_together(devices: &mut [&mut Device]) {
    for device in &mut *devices {
        device.begin(None);
    }
    for device in &mut *devices {
        device.ready();
    }
}

/// [`start_together`] for devices that each join by an invitation of their own, the `i`-th of
/// `devices` redeeming `invitations[i]`: each session reads its invitation at boot
/// ([`Device::join`]), so the whole batch is started before any is waited for.
pub fn join_together(devices: &mut [&mut Device], invitations: &[&str]) {
    assert_eq!(
        devices.len(),
        invitations.len(),
        "every device joins by one invitation"
    );
    let mut invitations = invitations.iter();
    for device in &mut *devices {
        device.begin(invitations.next().copied());
    }
    for device in &mut *devices {
        device.ready();
    }
}

/// What a site serves: its entry and the directory of what it lists. The entry itself is the
/// workspace's host's, which a device enters through on its session with the host
/// ([`Site::admit`] asks it as that entry does).
fn entry_responder(
    workspace: &str,
    name: &str,
    host: &tilo_server_protocol::web::PageHost,
) -> Responder {
    let host = serde_json::to_value(host).expect("the site's host as its directory publishes it");
    let origin = Arc::new(Mutex::new(String::new()));
    let served_origin = Arc::clone(&origin);
    let workspace_answered = workspace.to_owned();
    let listed = workspace.to_owned();
    let page_name = name.to_owned();
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
                        "admission": format!("entry/directory/{workspace_answered}/"),
                    },
                })
                .to_string(),
            ),
            ("POST", "/directory") => {
                let listed = listed.clone();
                json(
                    json!({ "host": host, "workspaces": [{
                        "workspace": listed,
                        "name": page_name,
                        "description": "",
                        "defaultRole": "editor",
                        "entry": entry_path(&workspace_answered),
                        "here": [],
                        "hereCount": 0,
                        "members": 1,
                        "lastActiveMs": null,
                        "community": false,
                    }] })
                    .to_string(),
                )
            }
            _ => Some(Reply::status(404)),
        }
    });
    *origin.lock().expect("the site's origin") = format!("{}/", responder.url());
    responder
}

/// The log of what runs in this test's own process — the site's server above all — as `RUST_LOG`
/// asks for it, warnings and worse by default: what the host did with a change a device published
/// is only said there.
pub fn install_log() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::io::stderr)
            .try_init();
    });
}

/// A person: the identity their first device made, the root their devices are certified under.
///
/// Their first device makes them through its own daemon, as `tilo login --new-identity` asks it
/// (`devices#found`), and founds their own workspace in that step; it is the one device of theirs
/// that holds their root. Every later device of theirs enters with a ticket that device issues
/// (`devices#issue`, `devices#logIn`), as `tilo device add` and `tilo login` do, and so takes
/// their workspace from it.
pub struct Person {
    /// What they go by.
    pub name: String,
    /// Their root, as a workspace names them.
    pub public: [u8; 32],
    /// Their root as their first device made it, read from where that device keeps it: what
    /// certifies the site's own device and a tab a page signed in, which stand where a device
    /// holding the root signs them in.
    root: tilo_identity::store::SigningIdentity,
    /// The home of the device that made their root, kept for as long as the person is: the device
    /// every later one of theirs takes its ticket from.
    founder: Arc<tempfile::TempDir>,
    /// Their first device, until [`Self::device`] hands it out.
    first: Mutex<Option<Device>>,
}

impl Person {
    /// The person `name`, made by their first device's daemon (`devices#found`), which founds their
    /// own workspace as it makes their root. The device is stopped once it has; the first
    /// [`Self::device`] is that device.
    pub fn new(name: &str) -> Self {
        let home = Arc::new(device_home());
        let mut first = Device::at(
            name,
            "First device",
            Arc::clone(&home),
            unsigned_cert([0; 32]),
        );
        first.restart_daemon();
        // Followed past its end, a founding is read where the person's part answers, which says
        // the device is theirs already: the founding is done once that part answers.
        let made = first.device_action(FOUND, json!({ "name": name }));
        if let Err(refused) = made
            && !refused.message.contains(ALREADY_A_PERSONS)
        {
            panic!(
                "{name}'s first device made them: {refused}\n{}",
                first.warnings()
            );
        }
        first.call_until_answered("info", &json!({}));
        first.stop();
        first.cert = tilo_device_identity::persist::read_owner_cert(&first.identity())
            .expect("the certificate the founding filed")
            .expect("the person's certificate");
        let root = founded_root(home.path());
        assert_eq!(
            root.group_public_key(),
            first.cert.user_public_key,
            "the device keeps the root that certified it"
        );
        Self {
            name: name.to_owned(),
            public: first.cert.user_public_key,
            root,
            founder: home,
            first: Mutex::new(Some(first)),
        }
    }

    /// The configuration home of the device that made the person.
    pub fn config_home(&self) -> PathBuf {
        self.founder.path().join("config")
    }

    /// Certify the device key `paths` holds as this person's site: the device a site runs, which
    /// serves the workspaces it hosts and takes part in their channels, so the records it keeps
    /// are sealed ones.
    pub fn certify_as_host(&self, paths: &IdentityPaths, label: &str) -> DeviceCert {
        self.certify_with(
            paths,
            label,
            Some(tilo_user_identity::DeviceCapabilities::member()),
        )
    }

    /// Certify the device key `paths` holds under this person's root, as their root signs in a
    /// tab a page approved: `label` is what it is called, and the capabilities are the ones the
    /// identity layer grants a joined device.
    pub fn certify(&self, paths: &IdentityPaths, label: &str) -> DeviceCert {
        self.certify_with(paths, label, None)
    }

    /// [`Self::certify`], with what the person lets the device do said here rather than taken from
    /// the identity layer.
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
        let (device, _) = tilo_device_identity::persist::load_or_create_device_key(paths)
            .expect("this computer's device key");
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: self.public,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: label.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        let digest = tilo_user_identity::device_cert_signing_digest(&cert)
            .expect("a device certificate digest");
        cert.signature = sign(&digest).expect("the person's root signs the device");
        tilo_device_identity::persist::write_public_key(&paths.user_pubkey, &self.public)
            .expect("this computer's person");
        if let Some(capabilities) = capabilities {
            // What a person lets this device do is theirs to say; the certificate is signed again
            // over what it now says.
            cert.capabilities = capabilities;
            let digest = tilo_user_identity::device_cert_signing_digest(&cert)
                .expect("a device certificate digest");
            cert.signature = sign(&digest).expect("the person's root signs the device");
        }
        tilo_device_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&cert.user_public_key),
            &cert,
        )
        .expect("the device's certificate is written");
        cert
    }

    /// One of this person's devices, called `label`, left stopped. The first is the one that made
    /// them; every later one is a computer of its own that enters with a ticket their first device
    /// issues ([`Device::redeem`]). Their first device names each as the person names a device
    /// (`devices#rename`), which only a device their record of their devices lists takes: that
    /// device's daemon as it is, running for a test that started it, else started until then.
    pub fn device(&self, label: &str) -> Device {
        let mut founder = Device::at(
            &self.name,
            "their first device",
            Arc::clone(&self.founder),
            unsigned_cert(self.public),
        );
        let founder_was_off = UnixStream::connect(founder.native_socket()).is_err();
        if founder_was_off {
            founder.restart_daemon();
        }
        let first = self.first.lock().expect("the person's first device").take();
        let device = first.map_or_else(
            || self.let_in(&mut founder, label),
            |mut first| {
                label.clone_into(&mut first.label);
                first
            },
        );
        founder.call_until_answered(
            tilo_device_identity::login::RENAME,
            &json!({ "device": hex::encode(device.cert.device_public_key), "name": label }),
        );
        if founder_was_off {
            founder.stop();
        }
        device
    }

    /// A computer of the person's own called `label`, let in with a ticket `founder`, their first
    /// device, issues, as `tilo device add` asks it there.
    fn let_in(&self, founder: &mut Device, label: &str) -> Device {
        let mut device = Device::at(
            &self.name,
            label,
            Arc::new(device_home()),
            unsigned_cert(self.public),
        );
        device.redeem(founder);
        // The device names the person as they go by, from the record its login took up: a device
        // that took up an unnamed person says so on everyone's bar until their record arrives.
        let identity_dir = tilo_identity::store::account_identity_dir(
            &device.directory().join("config/tilo/identity"),
        );
        let took_up = tilo_identity::provisioning::take_up_device_person(
            &identity_dir,
            &device.identity(),
            tilo_clock::unix_now_ns(),
            &self.name,
        )
        .expect("the device takes up its person");
        assert!(took_up, "the device's certificate names its person");
        device
    }
}

/// Make `root`'s runtime directory, as a machine's boot has its manager make it: a disk copied from
/// another machine never carries one, and a daemon started without it binds its sockets elsewhere
/// than where every client of this device looks.
fn make_runtime(root: &Path) {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root.join("runtime"))
        .expect("the device's runtime directory");
}

/// A directory for a device of its own: its home, configuration, data, state, cache and runtime.
fn device_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("a directory for the device");
    for directory in ["config", "data", "state", "cache", "runtime", "home"] {
        std::fs::create_dir_all(home.path().join(directory)).expect("a device directory");
    }
    home
}

/// A certificate nobody signed, naming `person` and no key: what a device is until a root
/// certifies its key, which its founding or its login files in its place.
fn unsigned_cert(person: [u8; 32]) -> DeviceCert {
    DeviceCert {
        user_public_key: person,
        device_public_key: [0; 32],
        device_label: String::new(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 0,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    }
}

/// The person's root as the stopped device at `home` keeps it: sealed to that device's key, so
/// this process takes the key, as the device's daemon does, while it reads the root. The
/// identity root the seal reads the key from is named in the environment for that time alone.
fn founded_root(home: &Path) -> tilo_identity::store::SigningIdentity {
    let mut sealing = tilo_test_env::ScopedEnv::new();
    sealing.set("TILO_IDENTITY_ROOT", home.join("identity"));
    let _taken = tilo_device_identity::persist::take_device_key(&IdentityPaths::under(
        home.join("identity"),
    ))
    .expect("the stopped device's key is taken as its daemon takes it");
    let dir = tilo_identity::store::account_identity_dir(&home.join("config/tilo/identity"));
    let record = tilo_identity::store::held_root(&dir).expect("the device kept the root it made");
    tilo_identity::store::load_from_path(record).expect("the person's root key")
}

/// Provisioning a person writes their profile and changes nothing this process reads: another
/// person's lookups, on any thread of this process, keep answering the profile the environment
/// names.
#[test]
fn provisioning_a_person_leaves_the_process_environment_alone() {
    let watched = [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "TILO_IDENTITY_ROOT",
        FILE_SECRETS_ENV,
    ];
    let before: Vec<_> = watched.into_iter().map(std::env::var_os).collect();
    let person = Person::new("Ada");
    let after: Vec<_> = watched.into_iter().map(std::env::var_os).collect();
    assert_eq!(
        before, after,
        "provisioning a person leaves the environment alone"
    );
    assert!(
        person.config_home().join("tilo/identity").is_dir(),
        "the person's identity is at their own roots"
    );
}

impl Person {
    /// A machine copied from `source`, one of this person's devices, as `tilo-vm vm branch`
    /// copies a disk, and entered as a device of the person's own: [`Self::copy_of`], then
    /// [`Device::redeem`] with a ticket `source` issues.
    pub fn branch(&self, source: &mut Device) -> (Device, tilo_device_identity::branch::Branched) {
        let (mut copy, branched) = self.copy_of(source);
        copy.redeem(source);
        (copy, branched)
    }

    /// `source`'s whole directory, copied while its session is stopped after `source` booted
    /// once on a build that records its machine. The copy boots on a machine of its own, so the
    /// boot's own re-key ([`branch::settle`]) gives it a device key of its own, which nobody
    /// certified yet: until it redeems a ticket ([`Device::redeem`]) it is no device of the
    /// person's, and opens nothing of theirs.
    ///
    /// What `source` keeps of the person's root is sealed to `source`'s key, and does not come
    /// along. A copy alone opens nothing, and the copy never holds the source's sealed root beside
    /// the one it is let into.
    pub fn copy_of(&self, source: &Device) -> (Device, tilo_device_identity::branch::Branched) {
        use tilo_device_identity::branch::{self, Branch, Machine, Settled};
        assert!(
            source.child.is_none() && source.daemon.is_none(),
            "a disk is copied while its machine is off"
        );
        let source_machine = Machine::parse("4c4c4544-0042-3510-8048-b4c04f4d3232").unwrap();
        let branch_machine = Machine::parse("8a1f02d6-91c4-4f0e-9b9a-2d1f5c3e7a10").unwrap();
        let boot = |paths: &IdentityPaths, machine: &Machine| {
            branch::settle(
                paths,
                Some(machine),
                &Branch {
                    // Nothing the copy holds sealed to its source's key is opened: it enters as
                    // a new device does, and keeps what it is let into sealed to its own key.
                    stores: vec![],
                    machine: None,
                    now_ns: tilo_clock::unix_now_ns(),
                    after: &|_| Ok(()),
                },
            )
            .expect("the boot settles which device this is")
        };
        // The source's first boot on this build records its machine, and it is that machine
        // from then on: a disk may be copied more than once.
        assert!(matches!(
            boot(&source.identity(), &source_machine),
            Settled::Recorded | Settled::Same
        ));

        let home = tempfile::tempdir().expect("a directory for the copy");
        copy_machine(source.directory(), home.path());
        drop_carried_root(home.path());

        let paths = IdentityPaths::under(home.path().join("identity"));
        let Settled::Branched(branched) = boot(&paths, &branch_machine) else {
            panic!("the copy did not re-key");
        };
        // The copy holds no root: its certificate waits for the person.
        assert!(matches!(
            branched.certification,
            branch::Certification::Pending(_)
        ));
        // Signed by nobody: it names the key the copy is to be certified for, and admits
        // nothing. Redeeming a ticket replaces it with the one the person's root signed.
        let cert = DeviceCert {
            device_public_key: branched.device,
            device_label: branched.label.clone(),
            capabilities: source.cert.capabilities.clone(),
            ..unsigned_cert(self.public)
        };
        let copy = Device::at(&self.name, &branched.label, Arc::new(home), cert);
        copy.seeds.set(source.seeds.get());
        (copy, *branched)
    }
}

/// Leave out of the machine copied to `root` what its source keeps of the person's root: the
/// record of the root it made and the share of it sealed to the source's key. The person's public
/// record, which names whose device this is, stays.
fn drop_carried_root(root: &Path) {
    let gone = |path: &Path| match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            panic!("{} is left out of the copy: {error}", path.display())
        }
        _ => {}
    };
    let dir = tilo_identity::store::account_identity_dir(&root.join("config/tilo/identity"));
    let record = tilo_identity::store::held_root(&dir).expect("the source kept its person's root");
    gone(&record);
    // The account's sealed shares, the root's alone, and the secret they are sealed with.
    match std::fs::remove_dir_all(dir.join("secrets")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            panic!("the source's sealed shares are left out of the copy: {error}")
        }
        _ => {}
    }
    gone(&dir.join("seal.key"));
}

impl Device {
    /// `person`'s device called `label`, over `home`, its certificate `cert`: nothing runs yet.
    fn at(person: &str, label: &str, home: Arc<tempfile::TempDir>, cert: DeviceCert) -> Self {
        Self {
            person: person.to_owned(),
            label: label.to_owned(),
            cert,
            control: home.path().join("control.sock"),
            home,
            child: None,
            daemon: None,
            shell: None,
            announced: None,
            session: None,
            seeds: std::cell::Cell::new(false),
            extra_env: Vec::new(),
        }
    }

    /// This device's daemon's native endpoint: where a program of the person's on it calls.
    fn native_socket(&self) -> PathBuf {
        self.home
            .path()
            .join("runtime/tilo")
            .join(tilo_machine::DAEMON_NATIVE)
    }

    /// Call the device action `action` of `devices` on this device's daemon, followed until it
    /// settles, as `tilo login` does on a computer no person's certificate names yet: its answer.
    fn device_action(&self, action: &str, args: Value) -> Result<Value, DeviceRefusal> {
        DeviceActions::at(self.native_socket()).ask_followed(action, args)
    }

    /// The workspace this device's daemon names as the person's own: the one it founds, from its
    /// own status.
    pub fn daemon_workspace(&self) -> String {
        let stream = UnixStream::connect(self.native_socket()).expect("the daemon answers");
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .expect("a read bound");
        writeln!(&stream, "{}", json!({"native": "status"})).expect("the status is asked");
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .expect("the daemon answers");
        let status: Value = serde_json::from_str(&line).expect("the daemon's status");
        status["workspace"].as_str().unwrap_or_default().to_owned()
    }

    /// The files of the workspaces this device's workspace service keeps on disk: what a step
    /// that could not reach one reads, to say whether it was never written or never opened.
    pub fn workspace_files(&self) -> String {
        let dir = self
            .home
            .path()
            .join("state/tilo/device/own/service/workspaces");
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| {
                let size = entry.metadata().map(|meta| meta.len()).unwrap_or_default();
                format!("{} ({size} bytes)", entry.file_name().to_string_lossy())
            })
            .collect();
        files.sort();
        format!("{}: {files:?}", dir.display())
    }

    /// The person's device action `action` with `args`, asked on their own connection to this
    /// device's daemon as `tilo device` asks it, in the workspace it holds of theirs: its answer,
    /// asked again until the person's services, which start after the daemon answers, take it.
    fn call_until_answered(&self, action: &str, args: &Value) -> Value {
        let socket = self.native_socket();
        let until = tilo_test_env::hang_deadline();
        loop {
            let answered = tilo_rt::app().handle().block_on(async {
                let calls = tilo_api::calls::Calls::connect(Some(socket.as_path()), None).await?;
                calls.founded_workspace().await?;
                calls
                    .start(
                        None,
                        tilo_device_identity::login::RESOURCE,
                        action,
                        args,
                        "A device action",
                    )
                    .await
            });
            match answered {
                Ok(answered) => return answered,
                Err(error) => assert!(
                    Instant::now() < until,
                    "{} did not answer {action}: {error:#}\n{}",
                    self.label,
                    self.warnings()
                ),
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Enter as a device of the person's through the one ticket issue: `issuer`, a device holding
    /// their root, issues a ticket (`devices#issue`, as `tilo device add` asks), and this device's
    /// daemon redeems it (`devices#logIn`, as `tilo login` does), which has the root certify this
    /// device's own key. What this device is let into from then on is sealed to that key, as on
    /// any device newly added. Each daemon is left as it was: stopped, or running.
    pub fn redeem(&mut self, issuer: &mut Device) {
        let issuer_was_off = UnixStream::connect(issuer.native_socket()).is_err();
        if issuer_was_off {
            issuer.restart_daemon();
        }
        let answer =
            issuer.call_until_answered(tilo_device_identity::login::ISSUE, &json!({"wait": false}));
        let ticket = answer["result"]["ticket"]
            .as_str()
            .filter(|ticket| !ticket.is_empty())
            .unwrap_or_else(|| panic!("{} issued no ticket: {answer}", issuer.label));
        // The issuer certifies the computer that redeems its ticket: it runs until then.
        let was_off = self.daemon.is_none();
        if was_off {
            self.restart_daemon();
        }
        if let Err(refused) = self.device_action(
            tilo_device_identity::login::LOG_IN,
            json!({"ticket": ticket, "agent": "tilo", "label": self.label}),
        ) {
            panic!(
                "{} logged in with {}'s ticket: {refused}\n{}",
                self.label,
                issuer.label,
                self.warnings()
            );
        }
        if was_off {
            self.stop();
        }
        if issuer_was_off {
            issuer.stop();
        }
        self.cert = tilo_device_identity::persist::read_owner_cert(&self.identity())
            .expect("the certificate the ticket's redemption filed")
            .expect("the person's certificate");
    }

    /// The people whose certificates name this device's key, as its daemon says what it is
    /// (`devices#info`): none on a computer no person let in yet.
    pub fn people(&self) -> Vec<String> {
        let info = self
            .device_action("info", json!({}))
            .unwrap_or_else(|refused| panic!("{} says what it is: {refused}", self.label));
        serde_json::from_value(info["people"].clone()).unwrap_or_default()
    }
}

/// Copy a stopped machine's directory as a disk copy carries it: every file
/// with its mode and every link as a link. What lives only while the machine
/// runs (its runtime directory, sockets) and this harness's own logs are not
/// on the disk.
fn copy_machine(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a copied directory");
    for entry in std::fs::read_dir(from).expect("a readable directory") {
        let entry = entry.expect("a directory entry");
        let name = entry.file_name();
        if name == "runtime" || name == "app-logs" {
            continue;
        }
        let target = to.join(&name);
        let kind = entry.file_type().expect("an entry's kind");
        if kind.is_dir() {
            copy_machine(&entry.path(), &target);
            let mode = std::fs::metadata(entry.path()).expect("a directory's mode");
            std::fs::set_permissions(&target, mode.permissions()).expect("a mode");
        } else if kind.is_symlink() {
            let link = std::fs::read_link(entry.path()).expect("a link");
            std::os::unix::fs::symlink(link, &target).expect("a copied link");
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).expect("a copied file");
        }
        // A socket or a pipe lives only while the machine runs.
    }
}

/// One device of one person on this machine: its daemon, and the session that daemon started.
pub struct Device {
    /// The person whose device this is.
    pub person: String,
    /// What the device is called.
    pub label: String,
    /// The certificate its person's root signed for it, as a workspace admits it.
    pub cert: DeviceCert,
    /// Its directory: shared with its person for the device that made them, which issues their
    /// later devices' tickets.
    home: Arc<tempfile::TempDir>,
    /// The session this test started, which stands in for the shell the daemon started for it
    /// until that shell ends.
    child: Option<Child>,
    /// The device's daemon, while it runs: the device's endpoint and state, and what started its
    /// shell.
    daemon: Option<Child>,
    /// The shell the daemon started, once it listens: the session's own process.
    shell: Option<u32>,
    control: PathBuf,
    /// What the session [`Self::begin`] started last said of its control socket, for
    /// [`Self::ready`]: whether it announced listening, sent once when the stream it prints on
    /// ends.
    announced: Option<mpsc::Receiver<bool>>,
    /// The session program this device runs, when it is not this package's own test build
    /// ([`Self::run_session`]).
    session: Option<PathBuf>,
    /// Whether its session serves the development session's tilolang apps it installed
    /// ([`Self::install_dev_apps`]).
    seeds: std::cell::Cell<bool>,
    /// Variables this device's session starts with beside the ones every test device has, set by
    /// [`Self::env`]: what a session needs to find the runtimes and the publishers its installed
    /// apps name.
    extra_env: Vec<(String, std::ffi::OsString)>,
}

impl Device {
    /// Start this device's session without joining anything: a session on someone's shell before
    /// they join a workspace.
    pub fn start(&mut self) {
        self.spawn(None);
    }

    /// Start this device's session joining the workspace `invitation` grants
    /// it, as a device the host started for a workspace does: the real join,
    /// redeemed over the collaboration endpoint.
    pub fn join(&mut self, invitation: &str) {
        self.spawn(Some(invitation));
    }

    /// Run the session program at `program` rather than this package's own build, from the next
    /// start on: a build of the session made another way, which finds the launcher beside itself
    /// as an installed session does.
    pub fn run_session(&mut self, program: PathBuf) {
        self.session = Some(program);
    }

    /// Start this device's session with `key` in its environment, beside the variables every test
    /// device's session carries: what a session reads to find runtime bundles and the publishers
    /// it trusts, which it forwards to the apps and the launcher it starts.
    pub fn env(&mut self, key: &str, value: impl Into<std::ffi::OsString>) {
        self.extra_env.push((key.to_owned(), value.into()));
    }

    /// The process of the session this device runs, while it runs: the shell its daemon started.
    pub fn session_pid(&self) -> Option<u32> {
        self.shell
    }

    /// Stop this device's daemon alone, as a service restarting does: the session it started goes
    /// on, and asks it for a new link once it answers again.
    pub fn stop_daemon(&mut self) {
        let Some(mut daemon) = self.daemon.take() else {
            return;
        };
        let pid = i32::try_from(daemon.id()).unwrap_or(0);
        if pid > 0 {
            // SAFETY: a signal to the one process this test started, not its group: the shell it
            // started is meant to outlive it.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
        let deadline = Instant::now().checked_add(Duration::from_secs(20));
        loop {
            if let Ok(Some(_)) = daemon.try_wait() {
                return;
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let _ = daemon.kill();
                let _ = daemon.wait();
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Start this device's daemon again over the state it kept, as a service restarting does. The
    /// shell it started before is still running and asks it for a new link by itself.
    pub fn restart_daemon(&mut self) {
        self.start_daemon("this device's daemon restarts", None);
    }

    /// Start this device's daemon over its own home, identity root, state and loopback endpoints,
    /// as the person's own service starts it, with what it logs headed `heading`, and wait until
    /// its native endpoint answers: the device's log, which the session the daemon starts writes
    /// to as well. `listening` is the line the daemon's output says once the session listens, and
    /// what to tell then.
    fn start_daemon(
        &mut self,
        heading: &str,
        listening: Option<(String, mpsc::Sender<bool>)>,
    ) -> std::fs::File {
        let root = self.home.path().to_path_buf();
        // The machine's boot makes its runtime directory, as a login's manager does: a copied disk
        // never carries one, and a daemon started without it binds its sockets elsewhere.
        make_runtime(&root);
        let logs = self.logs_dir();
        std::fs::create_dir_all(&logs).expect("a directory for what the device logs");
        // A device started again keeps what it said when it stopped: what it answered last, a
        // crash included, is what a step that fails after the restart reads. The daemon and the
        // session the daemon starts write here, as a device's journal holds both.
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs.join("session.log"))
            .expect("a log for the session");
        let _ = writeln!(log, "\n--- {heading} ---");
        let mut daemon = Command::new(tilo_test_env::sibling_binary("tilo-daemon", "tilo-runtime"));
        self.device_env(&mut daemon, &logs);
        daemon
            // The daemon starts this test's build of the session as the person's shell.
            .env(tilo_runtime::daemon::TEST_SHELL_ENV, self.program())
            .stdin(Stdio::null())
            .stdout(if listening.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::from(log.try_clone().expect("the log's own handle")));
        let mut daemon = spawn_group(daemon, "tilo-runtime");
        if let Some((listening, tell)) = listening {
            let out = daemon.stdout.take().expect("the daemon's output");
            told_at_end(out, Some(listening), tell, &log);
        }
        self.daemon = Some(daemon);
        // The daemon holds the device once its native endpoint answers, before the person's
        // services start: from then on the session finds it there and asks it for its shell.
        let native = root.join("runtime/tilo").join(tilo_machine::DAEMON_NATIVE);
        let label = self.label.clone();
        let logged = logs.join("session.log");
        // A daemon that never answers is read where it waits: what it logged, and each of its
        // threads' stacks. This wait's bound ends just before the one around it.
        let deadline = tilo_test_env::hang_deadline();
        let answered = tilo_test_env::wait_for(
            format!("{label}'s daemon to answer at {}", native.display()),
            || {
                if let Some(daemon) = self.daemon.as_mut()
                    && let Ok(Some(ended)) = daemon.try_wait()
                {
                    panic!(
                        "{label}'s daemon ended as it started ({ended}):\n{}",
                        std::fs::read_to_string(&logged).unwrap_or_default()
                    );
                }
                if UnixStream::connect(&native).is_ok() {
                    return Some(true);
                }
                (Instant::now() >= deadline).then_some(false)
            },
        );
        if !answered {
            let pid = self.daemon.as_ref().map(Child::id);
            panic!(
                "{label}'s daemon did not answer at {} within {:?}; it looks hung.\n\
                 its threads:\n{}\nwhat it logged:\n{}",
                native.display(),
                tilo_test_env::hang_bound(),
                pid.map(stacks).unwrap_or_default(),
                std::fs::read_to_string(&logged).unwrap_or_default()
            );
        }
        log
    }

    /// The session program this device runs ([`Self::run_session`]).
    fn program(&self) -> PathBuf {
        self.session
            .clone()
            .unwrap_or_else(|| tilo_test_env::package_binary!("tilo-session"))
    }

    /// Stop this device, as the person closing their laptop does: its session, then its daemon.
    pub fn stop(&mut self) {
        // The session stands in for the shell the daemon started, and hands the stop on to it.
        if let Some(session) = self.child.take() {
            end_group(session);
        }
        if let Some(daemon) = self.daemon.take() {
            end_group(daemon);
        }
        self.shell = None;
    }

    /// The device's own directory: its home, its config, its state and the identity root its
    /// session runs on.
    pub fn directory(&self) -> &Path {
        self.home.path()
    }

    /// The identity this device holds: the person's root's public key, the device's own key and the
    /// certificate the root signed for it.
    pub fn identity(&self) -> IdentityPaths {
        IdentityPaths::under(self.home.path().join("identity"))
    }

    /// Where this device's session and its apps log.
    fn logs_dir(&self) -> PathBuf {
        match std::env::var_os("TILO_APP_LOG_DIR") {
            Some(dir) => std::path::PathBuf::from(dir).join(format!(
                "{}-{}",
                self.person.to_lowercase().replace(' ', "-"),
                self.label.to_lowercase().replace(' ', "-")
            )),
            None => self.home.path().join("app-logs"),
        }
    }

    /// The device's own session file: what it logged, when something went wrong.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.logs_dir().join("session.log")).unwrap_or_default()
    }

    /// The process this device's session runs as, while it runs: what a sample of everything the
    /// session started ([`tilo_test_env::idle::idle_wakeups`]) is rooted at.
    pub fn process(&self) -> Option<u32> {
        self.shell
    }

    /// What this device's session logged as a warning or an error, oldest first: what a failed step
    /// says when it never happened.
    pub fn warnings(&self) -> String {
        let log = self.log();
        let warnings: Vec<&str> = log
            .lines()
            .filter(|line| line.contains(" WARN ") || line.contains(" ERROR "))
            .collect();
        warnings
            .iter()
            .rev()
            .take(300)
            .rev()
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Read this device's session log, so a failed step says why.
    fn read_line_of_log(&self) -> String {
        let log = self.log();
        log.lines().rev().take(400).collect::<Vec<_>>().join("\n")
    }

    /// What this device shows and logged, for a step that never happened.
    pub fn diagnostic(&self) -> String {
        let mut apps = String::new();
        if let Ok(entries) = std::fs::read_dir(self.logs_dir()) {
            for entry in entries.flatten() {
                let text =
                    std::fs::read_to_string(entry.path()).unwrap_or_else(|error| error.to_string());
                let interesting: Vec<&str> = text
                    .lines()
                    .filter(|line| {
                        [
                            "refused", "seal", "channel", "identity", "join", "WARN", "ERROR",
                            "panicked", "exited", "released", "held",
                        ]
                        .iter()
                        .any(|word| line.contains(word))
                    })
                    .collect();
                let shown = interesting
                    .iter()
                    .rev()
                    .take(200)
                    .rev()
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = writeln!(apps, "{}:\n{shown}", entry.path().display());
            }
        }
        let log: Vec<String> = self.read_line_of_log().lines().map(str::to_owned).collect();
        let tail = log
            .iter()
            .rev()
            .take(40)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "windows: {}\nwhat it told: {}\nwhat its session warned:\n{}\n\
             last lines of its log:\n{tail}\nwhat its apps said:\n{apps}",
            self.ask(&json!({"op": "windows"})),
            self.state()["state"]["notifications"],
            self.warnings(),
        )
    }

    /// Run this device's session and wait until it listens: [`Self::begin`] and [`Self::ready`].
    fn spawn(&mut self, invitation: Option<&str>) {
        self.begin(invitation);
        self.ready();
    }

    /// Start this device as a device starts: its daemon first, over its own home, identity root,
    /// state and loopback endpoints, as the person's own service starts it, and then its session,
    /// which asks that daemon to start it as the person's shell (`shells#start`) and so runs on the
    /// link the daemon made for it.
    ///
    /// What the daemon and the session do before the session listens — the person's services
    /// starting, the session's boot and the join it was handed — runs in their own processes, so a
    /// test that starts several devices together ([`start_together`], [`join_together`]) spends
    /// that once rather than once per device.
    ///
    /// This returns at once, and [`Self::ready`] waits for it: a test that has to watch a device
    /// that is already here while another boots — the arrival its bar says of the newcomer —
    /// starts the newcomer with this and watches from before its session could have said anything.
    pub fn begin(&mut self, invitation: Option<&str>) {
        let root = self.home.path().to_path_buf();
        let control = self.control.clone();
        let logs = self.logs_dir();
        let program = self.program();
        // What the session prints once it listens, the last thing it prints: on the daemon's
        // output, which the shell it starts writes to.
        let listening = format!("TILO_TEST_CONTROL={}", control.display());
        let (tell, told) = mpsc::channel();
        let log = self.start_daemon("this device starts", Some((listening, tell.clone())));
        let mut command = Command::new(&program);
        if self.seeds.get() {
            // It serves the development session's apps, and its launcher starts their windows on
            // the runtimes this run signed ([`super::dev_apps::runtimes`]), as a developer's
            // machine trusts its own builds.
            command.arg("--seed-apps").env(
                "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
                &super::dev_apps::runtimes().trusted,
            );
        }
        command.args([
            "--backend",
            "headless",
            "--no-x11",
            "--session-bus",
            "none",
            "--system-bus",
            "none",
            "--test-control",
            control.to_str().expect("a socket path"),
        ]);
        if SEED_APPS.get().is_some() {
            command.arg("--seed-apps");
        }
        self.device_env(&mut command, &logs);
        if let Some(invitation) = invitation {
            let path = root.join("invitation.txt");
            std::fs::write(&path, invitation).expect("the invitation is written");
            command.env(INVITATION_ENV, path);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log.try_clone().expect("the log's own handle")));
        let mut child = spawn_group(command, "tilo-session");
        // The session stands in for the shell the daemon started until that shell ends: its
        // output ending is the session ending, the shell's start refused included.
        let out = child.stdout.take().expect("the session's output");
        told_at_end(out, None, tell, &log);
        self.child = Some(child);
        self.announced = Some(told);
    }

    /// Give `command`, this device's daemon or its session, the device's environment: its own
    /// home, XDG tree, identity root, secrets and log filter, and the variables a test set
    /// ([`Self::env`], [`share_view_periods`]). The session the daemon starts takes these from the
    /// session that asks for it, as a login's session hands its own.
    fn device_env(&self, command: &mut Command, logs: &Path) {
        // This device's daemon runs the person's shell and their apps' actions, and starts their
        // processes through the run's launcher on the runtimes it built: named to the test process,
        // which the daemon inherits ([`super::furnished`]).
        let _ = super::furnished();
        let root = self.home.path();
        command
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env("TILO_IDENTITY_ROOT", root.join("identity"))
            // This device relays through this machine's loopback relay, as the site does: no
            // session of a test reaches the person's relays or their directory out on the internet.
            .env(
                test_server::loopback_relay().0,
                test_server::loopback_relay().1,
            )
            // Every device of a test runs on this one machine, so each takes its workspaces' work
            // on a port the machine gives it rather than the one port a computer's single runtime
            // takes: the person's other devices reach it at the address its endpoint says.
            .env(tilo_runtime::device::services::PORT_ENV, "0")
            // This device's files daemon serves beside the session in its own runtime directory,
            // where the session finds the daemon's link to the network (`fs::net`).
            .env("TILO_FS_BLOB_SOCKET", root.join("runtime/fs-blobs.sock"))
            // The apps installed on this device: what its shell opens, and what it starts them from.
            .env("TILO_APPS_DATA_DIR", root.join("data/tilo-apps"))
            // This device keeps the channel keys of the workspaces it is in here, as a machine with
            // no keychain does: without them it holds a workspace it cannot read or reach its
            // people through.
            .env(tilo_secrets::SECRETS_DIR_ENV, root.join("secrets"))
            .env(FILE_SECRETS_ENV, "1")
            // Each device runs Tilo OS, so the person's root certifies the ones it lets in as
            // native: what one asks another of theirs to do waits for no yes there.
            .env(TEST_PLATFORM_ENV, TILO_OS_PLATFORM)
            // The session says at info why a join did not land, and its direct links say at debug
            // what they carried: what a failed step reads. A test that needs more of it asked
            // first ([`log_sessions_at`]). The shell the daemon starts keeps the daemon's.
            .env(
                "RUST_LOG",
                SESSION_LOG.get().map_or(
                    "info,tilo_peer_state::direct=debug,tilo_peer_state::join_transport=debug,\
                     tilo_session::layout=debug,tilo_network_actor::native_ingress=debug,\
                     tilo_session_client::ownership=debug",
                    String::as_str,
                ),
            )
            .env("TILO_APP_STDERR_DIR", logs)
            // The daemon writes the same log as the session, and its lines read as the session's:
            // plain text a failed step's report finds by target and level.
            .env("NO_COLOR", "1")
            // Every launch writes its startup timeline, which the open-budget test reads.
            .env(tilo_startup_metrics::STARTUP_TIMELINE_ENV, "1")
            // The tests of this process mount the trees this device's session serves, so the
            // session records this process as the Files endpoint the login's unit would be.
            .env("TILO_FILES_ENDPOINT_PID", std::process::id().to_string())
            // This device hears the person's other devices on this machine's loopback: the
            // devices a test starts are beside each other there, and the network the runner
            // gives them carries no multicast between them.
            .env(tilo_endpoint_reachability::NETWORKS_ENV, "127.0.0.1")
            // This device's daemon is the person's own service, over their own profile.
            .env_remove("TILO_RUNTIME_PROFILE")
            .env_remove(runtime_collab::wire::shell::SHELL_TOKEN_ENV)
            .env_remove("TILO_TEST_CONTROL")
            .env_remove("TILO_ADMIN_CONTROL")
            .env_remove(INVITATION_ENV)
            .env_remove("TILO_SHELL_JOIN_ADMINISTRATOR")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("DISPLAY");
        for (key, value) in &self.extra_env {
            command.env(key, value);
        }
        // The share periods a test shortened ([`share_view_periods`]): the session renews a
        // viewer's ask that often, and keeps the sharing device's streams up that long after the
        // last one.
        if let Some((again, expiry)) = SHARE_VIEW.get() {
            command
                .env(tilo_session::VIEW_AGAIN_ENV, again.as_millis().to_string())
                .env(
                    tilo_session::VIEW_EXPIRY_ENV,
                    expiry.as_millis().to_string(),
                );
        }
    }

    /// Wait until the session [`Self::begin`] started opens its control socket, failing with what
    /// the device logged if it never does. [`Self::join`] and [`Self::start`] are this after their
    /// begin; a test that watched another device while this one booted asks for it here.
    pub fn ready(&mut self) {
        let Some(told) = self.announced.take() else {
            return;
        };
        match told.recv_timeout(tilo_test_env::hang_bound()) {
            Ok(true) => {}
            Ok(false) | Err(_) => panic!(
                "{}'s session never opened its control socket:\n{}",
                self.label,
                self.log()
            ),
        }
        self.shell = Some(listener_of(&self.control).unwrap_or_else(|| {
            panic!(
                "{}'s control socket names no process listening on it",
                self.label
            )
        }));
    }

    /// Ask the device's session `request` , as `tilo-control` does, and return its answer.
    pub fn ask(&self, request: &Value) -> Value {
        let mut stream = UnixStream::connect(&self.control)
            .unwrap_or_else(|error| panic!("{}'s control socket: {error}", self.label));
        // A request that asks the shell to hold its answer (`wait_ms`) is answered when what it
        // waits for happens: the read outlives that wait by the suite's hang bound, so a shell still
        // waiting for a change is waited for rather than reported as one that did not answer.
        let held = request
            .get("wait_ms")
            .or_else(|| request.pointer("/message/Invoke/wait_ms"))
            .or_else(|| request.pointer("/message/Observe/wait_ms"))
            .or_else(|| request.pointer("/message/ResourceRead/read/wait_ms"))
            .and_then(Value::as_u64)
            .map_or(Duration::ZERO, Duration::from_millis);
        stream
            .set_read_timeout(Some(held.saturating_add(tilo_test_env::hang_bound())))
            .expect("a read timeout");
        stream
            .write_all(format!("{request}\n").as_bytes())
            .expect("the socket reads");
        let mut line = String::new();
        if let Err(error) = BufReader::new(stream).read_line(&mut line) {
            panic!(
                "{} did not answer {request}: {error}; its threads:\n{}\nits session last said:\n{}",
                self.label,
                threads_of(&self.control),
                self.read_line_of_log(),
            );
        }
        serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!(
                "{}'s answer {line:?}: {error}\n{}",
                self.label,
                self.read_line_of_log()
            )
        })
    }

    /// The shell as this device shows it now: the tree, the workspaces its engine holds, its agents
    /// and its chrome.
    pub fn state(&self) -> Value {
        self.ask(&json!({"op": "state_dump"}))
    }

    /// The person's and the device's settings as this device's session reads them.
    pub fn settings(&self) -> Value {
        self.ask(&json!({"op": "settings"}))["answer"].take()
    }

    /// The workspaces this device keeps, by id: a workspace it joined is one of them.
    pub fn held(&self) -> Vec<String> {
        self.state()["state"]["held_workspaces"]
            .as_array()
            .map(|held| {
                held.iter()
                    .filter_map(|workspace| workspace["workspace"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Wait until this device reads `workspace`'s records as its person: the listing the session
    /// keeps there, which a device that holds the workspace reads. A read the workspace refuses is
    /// the reason its records never fill in, so the wait names it and what the device logged.
    pub fn wait_reading(&self, workspace: &str) {
        let deadline = Instant::now()
            .checked_add(joined_within())
            .expect("the wait's bound fits in an Instant");
        let mut pause = Duration::from_millis(10);
        loop {
            match self.read(
                workspace,
                (tilo_action_services::workspace_invocations::PROVIDER, ""),
                None,
                None,
            ) {
                Ok(_) => return,
                Err(error) => {
                    let now = Instant::now();
                    assert!(
                        now < deadline,
                        "{} never read {workspace} as its person: {error}:\n{}",
                        self.label,
                        self.log(),
                    );
                    std::thread::sleep(pause.min(deadline.saturating_duration_since(now)));
                    pause = pause.saturating_mul(2).min(Duration::from_millis(500));
                }
            }
        }
    }

    /// Wait until this device keeps the workspace `workspace` , failing with what its session
    /// logged if it never does.
    pub fn wait_holding(&self, workspace: &str) {
        // A device is told to join as it starts, and the join is the product's own: it gives it
        // `JOIN_PATIENCE` before it tells the person it did not happen, so the wait spans that and
        // the suite's hang bound after it.
        let deadline = Instant::now()
            .checked_add(joined_within())
            .expect("the wait's bound fits in an Instant");
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

    /// Wait until the resources of the apps this device's session runs itself are kept in the
    /// workspace it is in, failing with what its session warned if they never are.
    pub fn wait_apps_kept(&self) {
        let until = tilo_test_env::hang_deadline();
        while self.state()["state"]["apps_kept_in_workspace"].as_bool() != Some(true) {
            assert!(
                Instant::now() < until,
                "{} never kept its apps' resources in the workspace:\n{}",
                self.label,
                self.warnings(),
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// The workspace in front on this device, by id: what the API opens an app into.
    pub fn showing(&self) -> Option<String> {
        let tree = &self.state()["state"]["workspace_tree"];
        let active = tree["active"].as_str()?;
        tree["workspaces"]
            .as_array()?
            .iter()
            .any(|workspace| workspace["id"].as_str() == Some(active))
            .then(|| active.to_owned())
    }

    /// The workspaces on this device, by id, in the shell's order: the ones it made, the ones
    /// it joined and the ones the person's list brought from their other devices.
    pub fn on_shell(&self) -> Vec<String> {
        self.state()["state"]["workspace_tree"]["workspaces"]
            .as_array()
            .map(|workspaces| {
                workspaces
                    .iter()
                    .filter_map(|workspace| workspace["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What this device's session logged of the workspaces it joined and entered, oldest first:
    /// which of them arrived on its shell, and when.
    pub fn join_flow(&self) -> String {
        self.log()
            .lines()
            .filter(|line| line.contains(tilo_startup_metrics::JOIN_FLOW_TARGET))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The publisher this device's session goes by on its own device: what every other device in a
    /// workspace it is in knows it as.
    pub fn publisher(&self) -> Option<String> {
        let own = &self.state()["state"]["peers"]["own"];
        own["publisher"].as_str().map(str::to_owned)
    }

    /// Who this device's session says is present in `workspace` now: each member's publisher, which
    /// is the device, and the name their own word gives them — the rows every surface here draws
    /// its roster from.
    pub fn present(&self, workspace: &str) -> Vec<(String, String)> {
        let peers = &self.state()["state"]["peers"];
        let Some(members) = peers["workspaces"]
            .as_array()
            .and_then(|held| {
                held.iter()
                    .find(|held| held["workspace"] == json!(workspace))
            })
            .and_then(|held| held["members"].as_array())
        else {
            return Vec::new();
        };
        members
            .iter()
            .filter_map(|member| {
                Some((
                    member["publisher"].as_str()?.to_owned(),
                    member["label"].as_str()?.to_owned(),
                ))
            })
            .collect()
    }

    /// Wait until this device says the device `publisher` is present in `workspace` , failing with
    /// who it does say is there if it never does: presence reaches a device by itself, on the
    /// workspace's own app wire.
    pub fn wait_present(&self, workspace: &str, publisher: &str) {
        struct Present<'a> {
            device: &'a Device,
            workspace: &'a str,
            publisher: &'a str,
        }
        impl std::fmt::Display for Present<'_> {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let state = self.device.state();
                write!(
                    formatter,
                    "{} says {} is in {}; it says {:?}, holds {:?}, knows {}, and its live \
                     sessions are {}",
                    self.device.label,
                    self.publisher,
                    self.workspace,
                    self.device.present(self.workspace),
                    self.device.held(),
                    state["state"]["peers"],
                    state["state"]["sharing"]["live"],
                )
            }
        }
        let watched = Present {
            device: self,
            workspace,
            publisher,
        };
        tilo_test_env::wait_until(watched, || {
            self.present(workspace)
                .iter()
                .any(|(present, _)| present == publisher)
        });
    }

    /// Wait until the shell shows the workspace `workspace` in front: a person who joins a workspace
    /// lands in it, so the API opens apps into it rather than into the shell they were on.
    pub fn wait_showing(&self, workspace: &str) {
        let deadline = Instant::now()
            .checked_add(joined_within())
            .expect("the wait's bound fits in an Instant");
        let mut pause = Duration::from_millis(10);
        loop {
            let showing = self.showing();
            if showing.as_deref() == Some(workspace) {
                return;
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "{} never showed {workspace} in front; it shows {showing:?}:\n{}",
                self.label,
                self.log(),
            );
            std::thread::sleep(pause.min(deadline.saturating_duration_since(now)));
            pause = pause.saturating_mul(2).min(Duration::from_millis(500));
        }
    }

    /// The store this device's session reads its installed apps from.
    pub fn store(&self) -> PathBuf {
        self.home.path().join("data/tilo-apps")
    }

    /// Install the app `app` in this device's store from `payload` , as a device that carries the
    /// app has it: one row after another in the registry the session launches installed apps from.
    /// `package` is the digest of the payload's signed root, for an app the session reads the
    /// person's records of before it ever runs.
    fn install(&self, app: &str, (payload, package): &(PathBuf, String)) {
        let store = self.store();
        std::fs::create_dir_all(&store).expect("the device's store");
        let registry = store.join("registry.toml");
        let mut rows = std::fs::read_to_string(&registry).unwrap_or_default();
        writeln!(
            rows,
            "[[entry]]\nid = \"{app}\"\nversion = \"0.1.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n\
             package_id_hex = \"{package}\"",
            payload = payload.display(),
            manifest = payload.join("manifest.toml").display(),
        )
        .expect("the registry is a string");
        std::fs::write(&registry, rows).expect("the store's registry");
    }

    /// Install the tilolang apps a development session runs (Chat among them) in this device's
    /// store, the packages this run built, and have its session serve them as a development
    /// session does (`--seed-apps`): each app's records published from the session itself and
    /// kept in the workspace it holds, so its actions, its notices and its records reach the
    /// person's other devices and everyone else's the way any workspace record does. The runtimes
    /// the apps name go into the device's own store of runtimes ([`super::dev_apps::runtimes`]),
    /// so an app's window starts as it does on a person's computer.
    pub fn install_dev_apps(&self) {
        super::dev_apps::install(&self.store(), &self.home.path().join("state/tilolang"));
        super::dev_apps::install_runtimes(&self.home.path().join("data/tilo-apps/runtimes"));
        self.seeds.set(true);
    }

    /// Install Passwords in this device's store, as a device with the app installed has it: the
    /// tilolang package this run built.
    pub fn install_passwords(&self) {
        super::dev_apps::install_named(&self.store(), &["so.tilo.passwords"]);
    }

    /// Install Chat in this device's store, as a device its person installed it on has it, with
    /// the runtimes its window starts on: a second device of a workspace sends a message through
    /// Chat's action here, without seeding a scenario of its own into the workspace.
    pub fn install_chat(&self) {
        super::dev_apps::install_named(&self.store(), &["so.tilo.chat"]);
        super::dev_apps::install_runtimes(&self.home.path().join("data/tilo-apps/runtimes"));
    }

    /// What this device lists for the running `app`'s `operation`, once the app published it: a
    /// provider the session names by the app's live connection.
    pub fn app_action(&self, app: &str, operation: &str) -> RemoteActionTarget {
        let until = tilo_test_env::hang_deadline();
        loop {
            let listed = self.actions();
            if let Some(row) = listed.iter().find(|row| {
                row.app_id == app
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|action| action.address.action == operation)
                    && row.provider.parse::<u64>().is_ok()
            }) {
                return target(row);
            }
            assert!(
                std::time::Instant::now() < until,
                "{} never published {app}'s {operation}; it lists {app}'s {:?} of {} actions\n{}",
                self.label,
                listed
                    .iter()
                    .filter(|row| row.app_id == app)
                    .filter_map(|row| Some(format!(
                        "{} by {}",
                        row.action.as_ref()?.address.action,
                        row.provider
                    )))
                    .collect::<Vec<_>>(),
                listed.len(),
                self.warnings(),
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    /// Invoke the running `app`'s `operation` with `args` and wait until it settled, as a program
    /// of the person's does: how it ended, the person's confirmation given where it asked first.
    pub fn app_act(&self, app: &str, operation: &str, args: &Value) -> Value {
        let target = self.app_action(app, operation);
        let answer = self.act_wait(&target, args);
        if answer["err"]["code"].as_str() == Some(CONFIRMATION_REQUIRED) {
            return self.act_confirming(&target, args);
        }
        answer
    }

    /// Install the Files app in this device's store, the tilolang package this run built. A
    /// session reads the records of the person's that an installed app keeps from its store,
    /// before the app ever runs, and the daemon that serves this device's trees is that app's
    /// endpoint: a session serves no tree record for an app it cannot read as installed.
    pub fn install_files(&self) {
        super::dev_apps::install_named(&self.store(), &[FILES_APP]);
    }

    /// Install Scratchpad in this device's store, the tilolang package this run built: its one
    /// note is the workspace's record, so what either device writes the other's window shows.
    pub fn install_scratchpad(&self) {
        super::dev_apps::install_named(&self.store(), &[tilo_app_id::apps::SCRATCHPAD]);
    }

    /// Install the Shared Checklist example app in this device's store: the Rust app whose one
    /// checklist is the workspace's record, so what either device writes the other's window
    /// shows.
    pub fn install_shared_checklist(&self) {
        self.install(SHARED_CHECKLIST, shared_checklist_install());
    }

    /// Install Poker in this device's store: the tilolang app whose one table is the
    /// workspace's record, so a seat taken on either device is taken in the other's window.
    pub fn install_poker(&self) {
        super::dev_apps::install_named(&self.store(), &[POKER]);
    }

    /// What this device publishes now, every page of it: its resources and their actions.
    pub fn actions(&self) -> Vec<CatalogueRow> {
        let mut query = CatalogueQuery {
            max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        };
        let mut rows = Vec::new();
        loop {
            let RemoteActionMessage::CatalogueResponse { result, .. } =
                self.frame(RemoteActionMessage::CatalogueRequest {
                    correlation: 1,
                    query: query.clone(),
                })
            else {
                panic!("{} answered a listing with something else", self.label);
            };
            let page = result.unwrap_or_else(|error| panic!("{} lists: {error}", self.label));
            rows.extend(page.rows);
            if page.exhausted || page.next_cursor.is_empty() {
                return rows;
            }
            query.revision = Some(page.revision);
            query.cursor = page.next_cursor;
        }
    }

    /// The one action frame `message` asked of this device's session, and its answer.
    pub fn frame(&self, message: RemoteActionMessage) -> RemoteActionMessage {
        let asked = serde_json::to_value(RemoteActionFrame::new(message)).expect("a frame");
        let answer = self.ask(&asked);
        serde_json::from_value::<RemoteActionFrame>(answer.clone())
            .unwrap_or_else(|error| {
                panic!("{} answered {asked} with {answer}: {error}", self.label)
            })
            .message
    }

    /// The record `record` of `app` in `workspace`, read through the record API of this device's
    /// session as every reader of it reads it, once it moved past `after` or `wait_ms` passed:
    /// the readings (`records`, `revision`), or what the read was refused with.
    pub fn read(
        &self,
        workspace: &str,
        (app, record): (&str, &str),
        after: Option<&str>,
        wait_ms: Option<u32>,
    ) -> Result<Value, tilo_actions::remote::ActionCallError> {
        let answer = self.frame(RemoteActionMessage::ResourceRead {
            correlation: 1,
            on_behalf_of: None,
            read: tilo_actions::remote::ResourceReadRequest {
                workspace: workspace.to_owned(),
                app: app.to_owned(),
                record: record.to_owned(),
                joined: false,
                after: after.map(str::to_owned),
                wait_ms,
                answer: tilo_actions::remote::ReadAnswer::Reading,
            },
        });
        let RemoteActionMessage::ResourceReading { result, .. } = answer else {
            return Err(tilo_actions::remote::ActionCallError::Unavailable {
                reason: format!("{} answered a read with {answer:?}", self.label),
            });
        };
        result.map(|read| serde_json::from_str(&read).expect("a reading is JSON"))
    }

    /// Map a window of `app` called `title` on this device, as an app starting its own
    /// window does: a window of the workspace it shows, kept up while the returned value lives.
    pub fn window(&self, app: &str, title: &str) -> super::window::Client {
        super::window::Client::open(self.directory(), app, title)
    }

    /// Open the installed app `app` on this device, as a person starting it does: what an
    /// installed desktop opens from its applications list.
    pub fn open(&self, app: &str) {
        let answer = self.act(&self.open_target(app), &json!({}));
        assert!(
            answer.get("err").is_none(),
            "{} opens {app}: {answer}\n{}",
            self.label,
            self.read_line_of_log()
        );
    }

    /// The workspace's open of the installed app `app`, in the workspace in front: what the
    /// launcher invokes. The catalogue lists it only while the app is closed, and the open of an
    /// app already running brings its window up the same way, so it is never looked up there.
    pub fn open_target(&self, app: &str) -> RemoteActionTarget {
        let workspace =
            tilo_test_env::wait_for(format!("{} showing a workspace", self.label), || {
                self.showing()
            });
        tilo_actions::remote::application_open_target(&workspace, app)
    }

    /// What this device publishes as `resource`'s `action`, whoever publishes it.
    pub fn offered(&self, resource: &str, action: &str) -> Option<RemoteActionTarget> {
        self.actions()
            .iter()
            .find(|row| {
                row.action.as_ref().is_some_and(|offered| {
                    offered.address.resource == resource && offered.address.action == action
                })
            })
            .map(target)
    }

    /// Invoke `target` on this device, as a program of the person's does, answered once it is
    /// admitted: `{"update": …}` with where it stands, or `{"err": {"code", "message"}}`.
    pub fn act(&self, target: &RemoteActionTarget, args: &Value) -> Value {
        self.invoke(target, args, false, false)
    }

    /// [`Self::act`] followed until the invocation settles: the update it ended with carries the
    /// value it returned when it returns one.
    pub fn act_wait(&self, target: &RemoteActionTarget, args: &Value) -> Value {
        self.invoke(target, args, false, true)
    }

    /// [`Self::act_wait`], where the person says yes to what it asks: the call waits in its asked
    /// record, and they answer it on this connection, which is theirs, as `tilo`'s own prompt
    /// answers it. The call never says yes itself.
    pub fn act_confirming(&self, target: &RemoteActionTarget, args: &Value) -> Value {
        self.invoke(target, args, true, true)
    }

    /// Approve the asked record `record` (`<workspace>/<record>`, as the call's progress names
    /// it), as the person's own connection does.
    fn approve(&self, record: &str) {
        let record = record.split_once('/').map_or(record, |(_, record)| record);
        let target = RemoteActionTarget {
            provider: tilo_action_services::workspace_invocations::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(record, tilo_actions::remote::asked::RESOLVE),
        };
        let request = super::invocation(&target, &json!({"resolution": "approve"}), None, None);
        let answer = self.frame(RemoteActionMessage::Invoke(request));
        assert!(
            matches!(&answer, RemoteActionMessage::Receipt(receipt) if !matches!(
                receipt.status,
                ReceiptStatus::Rejected { .. } | ReceiptStatus::Failed { .. }
            )),
            "{} approves {record}: {answer:?}",
            self.label
        );
    }

    fn invoke(
        &self,
        target: &RemoteActionTarget,
        args: &Value,
        mut approving: bool,
        follow: bool,
    ) -> Value {
        let request = super::invocation(target, args, None, follow.then_some(10_000));
        let invocation = request.invocation;
        let RemoteActionMessage::Receipt(receipt) =
            self.frame(RemoteActionMessage::Invoke(request))
        else {
            panic!("{} answered an invocation with something else", self.label);
        };
        let mut state = match receipt.status {
            ReceiptStatus::Rejected { error } => {
                return json!({"err": {"code": error.code(), "message": error.to_string()}});
            }
            ReceiptStatus::Accepted => InvocationUpdate::Accepted { invocation },
            ReceiptStatus::Completed { output } => {
                InvocationUpdate::Succeeded { invocation, output }
            }
            ReceiptStatus::Failed { error } => InvocationUpdate::Failed { invocation, error },
            ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
                invocation,
                message,
            },
        };
        let deadline = tilo_test_env::hang_deadline();
        while follow && !state.is_terminal() && Instant::now() < deadline {
            let RemoteActionMessage::Update {
                state: Some(now), ..
            } = self.frame(RemoteActionMessage::Observe {
                correlation: 1,
                invocation,
                wait_ms: Some(10_000),
                seen: None,
            })
            else {
                break;
            };
            let read = now.read_answered;
            state = now.update;
            if read {
                break;
            }
            if approving
                && let InvocationUpdate::Progress {
                    phase,
                    message: Some(record),
                    ..
                } = &state
                && phase == tilo_actions::remote::asked::ASKED_PHASE
            {
                approving = false;
                self.approve(record);
            }
        }
        json!({"update": state})
    }

    /// Press the control named `named` , as an assistive technology activates it.
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

    /// Wait until a window of the app `app` is open on this device and drawn, so a test can aim at
    /// it.
    ///
    /// An app that started and stopped before its window came is not one to wait longer for: the
    /// wait ends with what the session logged of it, which is where a sandbox that could not start
    /// (or a launcher that refused) says why. Waiting out the hang bound instead would report a
    /// picture of the shell and nothing about the launch.
    pub fn wait_for_window(&self, app: &str) {
        let already_logged = self.log().lines().count();
        let ended = tilo_test_env::wait_for(NoWindow { device: self, app }, || {
            if self.shows_window(app) {
                return Some(None);
            }
            self.ended_since(already_logged, app).map(Some)
        });
        if let Some(why) = ended {
            panic!("{} did not start {app}: {why}", self.label);
        }
    }

    /// Whether this device shows a window of `app` that its app has drawn: until then the window
    /// stands nowhere on the screen.
    fn shows_window(&self, app: &str) -> bool {
        self.ask(&json!({"op": "windows"}))["answer"]["windows"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["app_id"].as_str() == Some(app) && !row["rect"].is_null())
            })
    }

    /// What this device's session logged of `app` ending, once the log had `already_logged` lines:
    /// the app stopping or failing to open, and the lines the app itself printed before it did.
    fn ended_since(&self, already_logged: usize, app: &str) -> Option<String> {
        let log = self.log();
        let lines: Vec<&str> = log.lines().skip(already_logged).collect();
        let named = format!("app={app}");
        let stopped = lines.iter().position(|line| {
            line.contains(&named)
                && (line.contains("an app stopped") || line.contains("an app could not open"))
        })?;
        let said: Vec<&str> = lines[stopped..]
            .iter()
            .copied()
            .filter(|line| line.contains(&named) || line.contains("tilo-launcher"))
            .take(20)
            .collect();
        Some(said.join("\n"))
    }
}

/// A device whose shell still shows no `app` window, named on the wait that found it short: what it
/// shows, and what its session and its apps said.
struct NoWindow<'a> {
    device: &'a Device,
    app: &'a str,
}

impl std::fmt::Display for NoWindow<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A window is laid out only in the workspace in front: which one that is, and which the
        // device holds, say why one it lists has no place on the screen.
        let state = self.device.state();
        let tree = &state["state"]["workspace_tree"];
        write!(
            formatter,
            "{} to show a {} window; it shows {}\nin front: {} of {}; held: {}\n{}",
            self.device.label,
            self.app,
            self.device.ask(&json!({"op": "windows"})),
            tree["active"],
            tree["workspaces"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|workspace| workspace["id"].to_string())
                .collect::<Vec<_>>()
                .join(", "),
            state["state"]["held_workspaces"],
            self.device.diagnostic()
        )
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The variable naming the invitation a session joins by as it starts.
const INVITATION_ENV: &str = "TILO_SHELL_JOIN_INVITATION_FILE";

/// Start `command`, the program `name`, as a device's process: leading its own process group, so a
/// stop of the group reaches everything it started, and ending with the test, so a test that ends
/// without its destructors (the runner killing it for running past its bound) leaves nothing of
/// its own running on the machine.
/// Every thread's stack of the process `pid`, as gdb reads them where the machine has gdb.
fn stacks(pid: u32) -> String {
    let read = Command::new("gdb")
        .args([
            "-batch",
            "-p",
            &pid.to_string(),
            "-ex",
            "thread apply all bt",
        ])
        .stdin(Stdio::null())
        .output_locked();
    match read {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(error) => format!("(gdb did not read them: {error})"),
    }
}

fn spawn_group(mut command: Command, name: &str) -> Child {
    command.process_group(0).dies_with_the_test();
    command.spawn_locked().unwrap_or_else(|error| {
        let program = Path::new(command.get_program());
        panic!(
            "{name} at {} starts: {error} (the program is there: {}; the directory it is in is \
             there: {})",
            program.display(),
            program.is_file(),
            program.parent().is_some_and(Path::is_dir),
        )
    })
}

/// Copy what `out` prints into `log` until it ends, telling `tell` `true` once it prints
/// `listening`, and `false` when it ends.
fn told_at_end(
    out: std::process::ChildStdout,
    listening: Option<String>,
    tell: mpsc::Sender<bool>,
    log: &std::fs::File,
) {
    let mut logged = log.try_clone().expect("the log's own handle");
    let mut out = BufReader::new(out);
    std::thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            match out.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if listening.as_deref() == Some(line.trim_end()) {
                let _ = tell.send(true);
            }
            let _ = writeln!(logged, "{}", line.trim_end());
        }
        let _ = tell.send(false);
    });
}

/// The process listening on the socket at `control`, as the kernel says of a connection to it.
fn listener_of(control: &Path) -> Option<u32> {
    use std::os::fd::AsRawFd as _;
    let stream = UnixStream::connect(control).ok()?;
    let mut peer = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    // SAFETY: the option is read into `peer`, whose size `length` says, on a socket this owns.
    let read = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut peer).cast(),
            &raw mut length,
        )
    };
    (read == 0).then(|| u32::try_from(peer.pid).ok()).flatten()
}

/// Stop the process group `child` leads, as a device's process is stopped: asked first, and
/// ended if it is still there after a while.
fn end_group(mut child: Child) {
    let pid = i32::try_from(child.id()).unwrap_or(0);
    if pid > 0 {
        // SAFETY: a signal to a process group this test started.
        unsafe {
            libc::killpg(pid, libc::SIGTERM);
        }
    }
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

/// The payload Shared Checklist is installed from here ([`stage_app`]), with the digest of its
/// signed root: the example Rust app this run built, under the manifest and surface its example
/// directory ships.
fn shared_checklist_install() -> &'static (PathBuf, String) {
    static INSTALL: std::sync::OnceLock<(PathBuf, String)> = std::sync::OnceLock::new();
    INSTALL.get_or_init(|| {
        let directory =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples/apps/shared-checklist");
        stage_app(
            "shared-checklist",
            &directory,
            &[(
                tilo_test_env::sibling_binary(
                    "example-shared-checklist",
                    "example-shared-checklist",
                ),
                "example-shared-checklist",
            )],
        )
    })
}

/// `app`'s payload staged in the build's own directory, as an install leaves a package: its
/// manifest and the surface its package ships, the signed root and collection an install writes
/// beside them, and each of `programs` the run built, hardlinked under `bin/` so a device pays for
/// no copy of a debug binary. The session reads a package's surface, and with it the records and
/// person records the app keeps, only from a payload whose signature covers it: a payload staged
/// without one is no install of the app at all. `directory` is the app's own directory, holding
/// the manifest it is installed from and the surface its package ships (an example app's
/// `examples/apps/<name>` is one).
fn stage_app(app: &str, directory: &Path, programs: &[(PathBuf, &str)]) -> (PathBuf, String) {
    use ed25519_dalek::Signer as _;
    use tilo_app_package::blobs::{BlobHash, Collection};
    use tilo_app_package::signed::SignedRoot;

    let exe = std::env::current_exe().expect("the test's own binary");
    let build = programs
        .first()
        .and_then(|(program, _)| program.parent())
        .unwrap_or_else(|| {
            exe.parent()
                .and_then(Path::parent)
                .expect("the build's own directory")
        });
    let payload = build.join(format!("{app}-payload-{}", std::process::id()));
    std::fs::create_dir_all(payload.join("bin")).expect("the package's payload");
    for (from, name) in programs {
        let to = payload.join("bin").join(name);
        let _ = std::fs::remove_file(&to);
        std::fs::hard_link(from, &to).expect("the program this run built");
    }
    // The product's font catalog beside the programs, where an app finds the catalog its package
    // carries: an installed app is handed the one in its runtime bundle, which the manifest here
    // leaves out ([`manifest_of`]). An app without one sets its text in the system's fonts, found
    // by parsing every face the system has at each launch, which no installed app does.
    if let Some(catalog) = tilo_ui_base::font_catalog::catalog_root() {
        link_tree(&catalog, &payload.join("bin").join("fonts"));
    }
    let manifest = manifest_of(app, directory).into_bytes();
    // A built-in app commits its compiled surface; an example ships only the surface its author
    // writes, which is compiled here as the packer compiles it.
    let compiled_path = directory.join("surface.json");
    let surface = if compiled_path.is_file() {
        std::fs::read(&compiled_path).unwrap_or_else(|error| {
            panic!("{app}'s surface at {}: {error}", compiled_path.display())
        })
    } else {
        let authored = directory.join("surface.toml");
        tilo_surface_compile::compile_file(&authored)
            .unwrap_or_else(|error| panic!("{app}'s surface: {error}"))
            .into_bytes()
    };
    std::fs::write(payload.join("manifest.toml"), &manifest).expect("the app's manifest");
    std::fs::write(payload.join("surface.json"), &surface).expect("the app's surface");
    let collection =
        Collection::from_entries([("surface.json".to_string(), BlobHash::of(&surface))]);
    let collection_bytes = collection.encode();
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[19_u8; 32]);
    let mut signed = SignedRoot::unsigned(
        BlobHash::of(&manifest),
        BlobHash::of(&collection_bytes),
        signing_key.verifying_key().to_bytes(),
    );
    // What the root's bytes sign says whether a signature is there, so one is placed before
    // signing: an installed package's root is signed, and the store reads it as such.
    signed.signature = Some([0; 64]);
    signed.signature = Some(signing_key.sign(&signed.signing_bytes()).to_bytes());
    let signed_bytes = signed.encode();
    let package = BlobHash::of(&signed_bytes).to_hex();
    std::fs::write(payload.join("signed-root"), signed_bytes).expect("the package's root");
    std::fs::write(payload.join("collection.qcl"), &collection_bytes)
        .expect("the package's collection");
    (payload, package)
}

/// The manifest an app of this repository is installed from as one of this machine's stores
/// installs it: the app's own description (`tilo-app.toml` in `directory`), without the runtime
/// bundle it names. An installed desktop carries that bundle; a build runs on the machine it was
/// built on.
fn manifest_of(app: &str, directory: &Path) -> String {
    let path = directory.join("tilo-app.toml");
    let manifest = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{app}'s manifest at {}: {error}", path.display()));
    let mut kept = String::new();
    let mut isa = false;
    for line in manifest.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            isa = trimmed == "[package.isa]";
        }
        if isa || trimmed.starts_with("runtime") {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

/// An id of this test's own, distinct from every other it makes: what a request is remembered by.
fn fresh_id() -> [u8; 32] {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let digest = blake3::hash(format!("id/{}/{n}", std::process::id()).as_bytes());
    let mut id = [0; 32];
    id.copy_from_slice(digest.as_bytes());
    id
}

/// The tree `from` at `to`, each file hardlinked where both are on one filesystem and copied where
/// they are not. A link in the tree is followed: the sandbox sees none of its targets.
fn link_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a linked directory");
    for entry in std::fs::read_dir(from).expect("a readable directory") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        let source = entry.path().canonicalize().expect("a file the tree names");
        if source.is_dir() {
            link_tree(&source, &target);
        } else {
            let _ = std::fs::remove_file(&target);
            if std::fs::hard_link(&source, &target).is_err() {
                std::fs::copy(&source, &target).expect("a copied file");
            }
        }
    }
}

/// What each thread of the session whose control socket is `control` is in, as its liveness socket
/// beside it answers: a session that stopped answering says where it waits.
fn threads_of(control: &Path) -> String {
    let asked = UnixStream::connect(tilo_liveness::socket_path(control)).and_then(|mut stream| {
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.write_all(b"{\"op\":\"threads\"}\n")?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        Ok(line)
    });
    let answer = match asked.map(|line| serde_json::from_str::<tilo_liveness::Answer>(&line)) {
        Ok(Ok(answer)) => answer,
        Ok(Err(error)) => return format!("its liveness socket answered no report: {error}"),
        Err(error) => return format!("its liveness socket did not answer: {error}"),
    };
    let tilo_liveness::Answer::Threads(threads) = answer else {
        return format!("{answer:?}");
    };
    threads
        .iter()
        .map(|thread| {
            format!(
                "thread {} {} {} {}:\n{}",
                thread.tid, thread.name, thread.state, thread.wait, thread.backtrace
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The code an action that asks the person first is refused with until they confirm it.
pub const CONFIRMATION_REQUIRED: &str = "action_confirmation_required";

/// What `row` invokes.
pub fn target(row: &CatalogueRow) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: row.provider.clone(),
        address: row
            .action
            .as_ref()
            .expect("a row with an action")
            .address
            .clone(),
    }
}

/// The action `action` of `resource` that `provider` publishes.
pub fn at(provider: &str, resource: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: provider.to_owned(),
        address: tilo_actions::ActionAddress::new(resource, action),
    }
}

/// The workspace's `start` of the closed app `app`, as the host names it.
pub fn start_action(workspace: &str, app: &str) -> RemoteActionTarget {
    at(
        &tilo_actions::remote::workspace_provider(workspace),
        &format!("{}{app}", tilo_actions::remote::APPLICATION_RESOURCE_PREFIX),
        tilo_actions::remote::APPLICATION_START_ACTION,
    )
}

/// Whether `device`'s shell shows a window of `app`.
pub fn shows_window(device: &Device, app: &str) -> bool {
    device.ask(&json!({"op": "windows"}))["answer"]["windows"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["app_id"].as_str() == Some(app)))
}

/// The settings section `section` (`appearance`, `power`) as `device`'s settings service
/// publishes it: the resource whose setters a control, a script or an agent runs,
/// `settings/appearance` for the person's, `devices/<device>/settings/power` for the device's.
pub fn setting_section(device: &Device, section: &str) -> String {
    settings_store::placement::address(section, None, &hex::encode(device.cert.device_public_key))
        .id
}

/// The setter of `field` in the settings section `section` on `device`, as Settings and Quick
/// Settings run it.
pub fn setting(device: &Device, section: &str, field: &str) -> RemoteActionTarget {
    at(
        tilo_actions::remote::settings_resources::PROVIDER,
        &setting_section(device, section),
        &settings_store::workspace::verb(field),
    )
}

/// Set `field` of `section` to `value` on `device`, as the person does in Settings, once it
/// settled: a change the settings refused is the assertion.
///
/// The sections are resources of the person's own workspace, so the edit is not offered until
/// this session reads it there: the set is asked again until it is.
pub fn set_setting(device: &Device, section: &str, field: &str, value: &Value) {
    let target = setting(device, section, field);
    let edit = json!({"value": value});
    let until = tilo_test_env::hang_deadline();
    let mut answer = device.act_wait(&target, &edit);
    while answer["err"]["code"].as_str() == Some("unknown_action") {
        assert!(
            Instant::now() < until,
            "{} is never offered {section}'s setter of {field}: {answer}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(100));
        answer = device.act_wait(&target, &edit);
    }
    if answer["err"]["code"].as_str() == Some(CONFIRMATION_REQUIRED) {
        answer = device.act_confirming(&target, &edit);
    }
    assert_eq!(
        answer["update"]["state"],
        json!("succeeded"),
        "{} sets {section}.{field}: {answer}\n{}",
        device.label,
        device.diagnostic()
    );
}

/// The session's own verb that makes a new workspace, as the workspace menu invokes it.
pub fn new_workspace() -> RemoteActionTarget {
    at(
        tilo_actions::remote::session_actions::PROVIDER,
        "desktop:chrome",
        "new-workspace",
    )
}
