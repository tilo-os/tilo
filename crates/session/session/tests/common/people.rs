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

use std::fmt::Write as _;
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

/// The invitation text a site issues for a person: a [`tilo_workspace_join`] invitation, as a link
/// carries it.
pub type Invitation = String;

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

/// How long a device is given to hold the workspace it joined and to show it in front: the join is
/// the product's own, which gives it [`tilo_session::sync::JOIN_PATIENCE`] before the person is told
/// it did not happen, and the suite's hang bound after that.
pub fn joined_within() -> Duration {
    tilo_session::sync::JOIN_PATIENCE.saturating_add(tilo_test_env::hang_bound())
}

/// A site: a relay of this machine that governs one workspace, hosts it for the people in it and
/// admits people to it at its door.
pub struct Site {
    runtime: tokio::runtime::Runtime,
    relay: Option<tilo_relay::daemon::Relay>,
    /// The workspaces this site governs.
    pub workspaces: tilo_relay::workspaces::Workspaces,
    /// The person this site is: the workspace's owner.
    pub owner: [u8; 32],
    /// The site's device, the one it governs and signs with — a device of [`Self::owner`], as a
    /// home peer is.
    pub host: DeviceCert,
    /// The workspace the site serves here, by id.
    pub workspace: String,
    /// The workspace's name.
    pub name: String,
    /// Where the site is reached: its entry, as a link names it.
    pub entry: String,
    /// The door the site serves, kept up until the site is dropped.
    _door: Responder,
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
        if let Some(relay) = self.relay.take() {
            self.runtime.block_on(relay.shutdown());
        }
    }

    /// `owner`'s site: a relay of this machine governing the workspace `name`,
    /// listening on loopback with its directory and door served over HTTP.
    pub fn start(owner: &Person, name: &str) -> Self {
        // The site runs in this process: its log is this test's stderr.
        install_log();
        // The site keeps the keys of the channels it seals in the platform secret store, which this
        // process keeps in memory: a site without one can host no sealed record, and its people's
        // devices reach no plane.
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
        let host_key = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
            .expect("the site's device key");
        let (relay, workspaces, workspace) = runtime.block_on(async {
            let relay = tilo_relay::daemon::Relay::start_with_relay_mode(
                tilo_relay::RelayConfig {
                    state_root: state.path().join("host"),
                    // Devices of this machine find the site on loopback, as they do on a person's
                    // own network.
                    iroh_listen: "127.0.0.1:0".parse().expect("a loopback address"),
                    ..tilo_relay::RelayConfig::default()
                },
                tilo_relay::daemon::RelayIdentity {
                    user_public_key: owner.public,
                    device_signing_key: host_key.clone(),
                    device_cert: host.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                },
                // The site relays through this machine's loopback relay, as its people's devices
                // do: neither side reaches number 0.
                test_relay::relay_mode(),
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
            // The site's own device is in the workspace it governs, as a site's is: it hosts what
            // its people record there.
            workspaces
                .enter(&workspace, &host)
                .await
                .expect("the site's device is in its workspace");
            (relay, workspaces, workspace)
        });
        let door = door_responder(&workspace, name);
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
            state,
        }
    }

    /// Delete the workspace, as its owner does: final, for everyone in it.
    pub fn delete(&self) {
        self.runtime
            .block_on(self.workspaces.delete(&self.workspace))
            .expect("the site deletes its workspace");
    }

    /// Where the site's relay is reached, as a link to something it hands out names it.
    pub fn node_addr(&self) -> iroh::EndpointAddr {
        self.relay
            .as_ref()
            .expect("the site's relay runs")
            .node_addr()
    }

    /// The workspace `workspace` as the site's host carries it, by its id here: what a machine an
    /// environment runs in is handed, and what a page action names.
    pub fn workspace_id(&self, workspace: &str) -> Option<u64> {
        self.runtime
            .block_on(self.workspaces.summary(workspace))
            .ok()
            .map(|summary| summary.workspace_id)
    }

    /// Make the workspace public with `role` for everyone who joins at the door, as its owner does:
    /// from now on the door admits people.
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

    /// Admit `person` at the site's door, as their own entry through it asks
    /// it, and answer the invitation the door lets them in with: one only
    /// they can redeem.
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
        let (issued, _token) = self
            .runtime
            .block_on(self.workspaces.admit_listed(
                &self.workspace,
                &DoorJoin {
                    user_public_key: hex::encode(person),
                    display_name: "Someone".to_owned(),
                },
            ))
            .map_err(|refusal| refusal.to_string())?;
        Ok(tilo_workspace_join::encode_invitation_text(
            &issued.signed_invitation,
        ))
    }

    /// Make `person` one of the workspace's `role` , as its owner does: a moderator invites and
    /// removes, an editor writes.
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

    /// The invitation the site's owner issues for `person` , as a person invites someone into their
    /// workspace: it binds the person's root, so only their devices redeem it.
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

    /// The workspace's people as its host records them: each person's root in hex, and their role
    /// and whether they are removed.
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

    /// Remove `person` and every device of theirs from the workspace, as its owner does: from now
    /// on the site hosts nothing of the workspace for them.
    pub fn remove(&self, person: [u8; 32]) {
        self.runtime
            .block_on(self.workspaces.remove_member(
                &self.workspace,
                &self.host,
                tilo_relay::workspaces::RemoveMemberRequest {
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

    /// The apps the workspace's governance lets author its records, as its host records them now.
    pub fn authors(&self) -> std::collections::BTreeSet<String> {
        self.runtime
            .block_on(self.workspaces.app_authors(&self.workspace))
            .expect("the workspace's app authors")
    }

    /// The records of the workspace this site holds, by the app and record they belong to: what its
    /// people wrote there, as their host keeps it.
    pub fn records(&self) -> Vec<(String, String)> {
        self.runtime
            .block_on(self.workspaces.record_routes(&self.workspace))
            .expect("the site's records")
    }

    /// What the site's copy of record `record` of `app` holds, saved, read through the keys it
    /// holds as a member of the record's channel: `None` while it holds no such record, or cannot
    /// open it yet.
    pub fn record(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        self.runtime
            .block_on(
                self.workspaces
                    .record_document(&self.workspace, app, record),
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

    /// Wait until the workspace's governance lets `app` author its records: what the owner's device
    /// granting it there leaves the host holding, and what a member's device joins with.
    pub fn wait_for_author(&self, app: &str) {
        tilo_test_env::wait_until(format!("the workspace lets {app} author"), || {
            self.authors().contains(app)
        });
    }

    /// Let the apps `apps` author the workspace's records, as the host of a workspace does before
    /// its people open them there ([`tilo_relay::workspaces::Workspaces::settle_app_authorship`]):
    /// a member whose role lets them write writes through an app the workspace lets author, and a
    /// member may not grant that themselves — the workspace's own devices do.
    pub fn author_apps(&self, apps: &[&str]) {
        let apps: std::collections::BTreeSet<String> =
            apps.iter().map(|app| (*app).to_owned()).collect();
        self.runtime
            .block_on(self.workspaces.settle_app_authorship(
                &self.workspace,
                &self.host,
                &apps,
                &std::collections::BTreeSet::new(),
            ))
            .expect("the workspace lets its people's apps author");
        for app in apps {
            self.wait_for_author(&app);
        }
    }

    /// The workspace's shared desktop as the site keeps it now: its strips and windows, or nothing
    /// while the workspace holds no desk of its own.
    pub fn desktop(&self) -> Option<tilo_desktop_model::DesktopTree> {
        let bytes = self.record(
            tilo_desktop_model::ownership::DESKTOP_APP,
            tilo_desktop_model::ownership::DESKTOP_RECORD,
        )?;
        let document = automerge::AutoCommit::load(&bytes).ok()?;
        tilo_desktop_model::shared_record::read(&document, None).map(|desk| desk.tree)
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

/// How many windows the workspace's shared desk holds, as the site keeps it.
pub fn windows(site: &Site) -> usize {
    titles_at(site).len()
}

/// The key `device` gives the workspace `site` serves, while it holds it.
fn workspace_key(device: &Device, site: &Site) -> Option<u64> {
    device.state()["state"]["held_workspaces"]
        .as_array()?
        .iter()
        .find(|workspace| workspace["room"].as_str() == Some(site.workspace.as_str()))
        .and_then(|workspace| workspace["workspace"].as_u64())
}

/// Bring the workspace `site` serves to the front on `device`'s desk, so an app it opens is a
/// window of that workspace's shared desk.
pub fn show(device: &Device, site: &Site) {
    tilo_test_env::wait_until(format!("{} holds the workspace", device.label), || {
        workspace_key(device, site).is_some()
    });
    let key = workspace_key(device, site).expect("the device holds the workspace");
    let shown = device.act(
        "so.tilo.session",
        "session:desk\u{1f}switch-workspace",
        &json!({"workspace": key.to_string()}),
    );
    assert!(
        shown.get("err").is_none(),
        "{} shows {}: {shown}\n{}",
        device.label,
        site.name,
        device.log()
    );
}

/// The window titles `device` shows on the workspace `site` serves: the shared desk it reads out of
/// the workspace.
pub fn titles_on(device: &Device, site: &Site) -> Vec<String> {
    let state = device.state();
    let tree: tilo_desktop_model::DesktopTree =
        serde_json::from_value(state["state"]["workspace_tree"].clone()).unwrap_or_else(|error| {
            panic!("{}'s desk does not read: {error}\n{state}", device.label)
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

/// The window titles the site keeps on the workspace's shared desk.
pub fn titles_at(site: &Site) -> Vec<String> {
    site.desktop().map_or_else(Vec::new, |desk| {
        let mut titles: Vec<String> = desk
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

/// Wait until the site keeps a window titled `title` on the workspace's desk and every device in
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
                device.on_desk(),
                device.diagnostic()
            )?;
        }
        Ok(())
    }
}

/// Two people in one workspace Ada's site hosts, both editors, each at a device of their own: the
/// site lets its desk author the workspace's records, as a workspace's host does before its people
/// open windows there.
pub fn two_people_at_a_site() -> (Site, Person, Person, Device, Device) {
    two_people_at_a_site_carrying(&[], |_| {})
}

/// [`two_people_at_a_site`] for people whose devices carry the Files app: a session reads the apps
/// it has when it starts, so each device has them before its session does, and the workspace
/// lets them author its records, as a host lets the first-party apps author theirs.
pub fn two_people_at_a_site_with_files() -> (Site, Person, Person, Device, Device) {
    two_people_at_a_site_carrying(&[FILES_APP], Device::install_files)
}

/// The app whose record a workspace's tree is.
pub const FILES_APP: &str = "so.tilo.file-explorer";

/// The two people's laptops in the site's workspace, each carrying what `install` puts in its
/// store before its session starts, and the workspace letting `authored` author its records.
fn two_people_at_a_site_carrying(
    authored: &[&str],
    install: impl Fn(&Device),
) -> (Site, Person, Person, Device, Device) {
    let (site, ada, bob, ada_laptop) = site_carrying(authored, &install);
    let bob_laptop = joined_carrying(&site, &bob, "Bob's laptop", &install);
    site.wait_for_member(bob.public);
    (site, ada, bob, ada_laptop, bob_laptop)
}

/// [`two_people_at_a_site_with_files`] before the second person joins: Ada's laptop holds the
/// workspace, and what brings Bob in is the invitation her session hands out for it.
pub fn a_site_with_files_for_one_laptop() -> (Site, Person, Person, Device) {
    site_carrying(&[FILES_APP], &Device::install_files)
}

/// The site the two people share and the first person's laptop in it, carrying what `install`
/// puts in its store, with the workspace letting `authored` author its records.
fn site_carrying(authored: &[&str], install: &impl Fn(&Device)) -> (Site, Person, Person, Device) {
    let ada = Person::new("Ada");
    let bob = Person::new("Bob");
    let site = Site::start(&ada, "Studio");
    let authored: Vec<&str> = std::iter::once(tilo_desktop_model::ownership::DESKTOP_APP)
        .chain(authored.iter().copied())
        .collect();
    site.author_apps(&authored);
    let ada_laptop = joined_carrying(&site, &ada, "Ada's laptop", install);
    (site, ada, bob, ada_laptop)
}

/// The invitation `device`'s session hands out for the workspace it holds, made by the desk's own
/// action at `role` for `person`: what an app shares, and what only `person`'s devices redeem.
pub fn shared_invitation(device: &Device, role: &str, person: [u8; 32]) -> Invitation {
    let answer = desk_answer(
        device,
        &json!({"role": role, "recipient": hex::encode(person)}),
    );
    let update = &answer["answer"]["receipt"]["update"];
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

/// The answer `device`'s desk gives when asked to hand out an invitation into the workspace it
/// shows, as the app sharing what is on screen asks it.
pub fn desk_answer(device: &Device, args: &serde_json::Value) -> serde_json::Value {
    device.act_wait(
        tilo_actions::remote::session_actions::PROVIDER,
        &format!(
            "{}\u{1f}{}",
            tilo_actions::remote::session_actions::DESK_RESOURCE,
            tilo_actions::remote::session_actions::ISSUE_INVITATION
        ),
        args,
    )
}

/// One person with two devices of their own in one workspace their site hosts, both editors: the
/// two sessions a person's laptop and their phone run, sharing what they record there.
pub fn two_devices_of_one_person() -> (Site, Person, Device, Device) {
    two_devices_of_one_person_carrying(|_| {})
}

/// A device of `person`'s, joined to the workspace `site` serves and holding it: what the owner's
/// invitation admits, as a device redeems it.
fn joined(site: &Site, person: &Person, label: &str) -> Device {
    joined_carrying(site, person, label, &|_| {})
}

/// [`joined`] for a device carrying what `install` puts in its store, which it has before its
/// session starts.
pub fn joined_carrying(
    site: &Site,
    person: &Person,
    label: &str,
    install: &impl Fn(&Device),
) -> Device {
    let mut device = person.device(label);
    install(&device);
    let invitation = site.invite(person.public, WorkspaceRole::Editor);
    device.join(&invitation);
    device.wait_holding(&site.workspace);
    device
}

/// [`two_devices_of_one_person`] for two devices that carry the Files app: a session reads the apps
/// it has when it starts, so each device has them before its session does.
pub fn two_devices_of_one_person_with_files() -> (Site, Person, Device, Device) {
    two_devices_of_one_person_carrying(Device::install_files)
}

/// The person's laptop and phone, each carrying what `install` puts in its store: both are made
/// first, then told of the person's fleet, then started — a session reads the apps and the devices
/// of the person it has when it starts.
fn two_devices_of_one_person_carrying(install: impl Fn(&Device)) -> (Site, Person, Device, Device) {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.author_apps(&[tilo_desktop_model::ownership::DESKTOP_APP]);
    let mut laptop = ada.device("Ada's laptop");
    let mut phone = ada.device("Ada's phone");
    share_the_fleet(&ada, &[&laptop, &phone]);
    for device in [&mut laptop, &mut phone] {
        install(device);
        let invitation = site.invite(ada.public, WorkspaceRole::Editor);
        device.join(&invitation);
        device.wait_holding(&site.workspace);
    }
    (site, ada, laptop, phone)
}

/// Tell each of `devices` about the rest of the person's fleet, as the machine that enrolls a
/// device does ([`tilo_identity_host::provisioning`]): a device admits what the fleet names, and
/// dials the siblings it holds, so what the person records reaches their other devices.
pub fn share_the_fleet(person: &Person, devices: &[&Device]) {
    let identity_id = hex::encode(person.public);
    for device in devices {
        let fleet = tilo_identity_host::device_fleet::DeviceFleetStore::at(
            device.directory().join("config/tilo/identity"),
            device.identity(),
        );
        for other in devices {
            fleet
                .remember_certificate(&identity_id, other.cert.clone())
                .expect("the person's device joins their fleet");
        }
    }
}

/// What a site serves: its entry and the directory of what it lists. The door itself is the
/// workspace's host's, which a device enters through on its session with the host
/// ([`Site::admit`] asks it as that entry does).
fn door_responder(workspace: &str, name: &str) -> Responder {
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
                        "admission": format!("door/directory/{workspace_answered}/"),
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
            _ => Some(Reply::status(404)),
        }
    });
    *origin.lock().expect("the site's origin") = format!("{}/", responder.url());
    responder
}

/// The log of what runs in this test's own process — the site's relay above all — as `RUST_LOG`
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

/// A person: the root their devices are certified under, and the identity this machine provisions
/// them with before any of their devices run.
pub struct Person {
    /// What they go by.
    pub name: String,
    /// Their root, as a workspace names them.
    pub public: [u8; 32],
    root: runtime_identity::SigningIdentity,
    /// The identity the first use of this machine provisioned for them: the record each of their
    /// devices carries, and the root secret devices certify their own keys with.
    identity: tempfile::TempDir,
}

impl Person {
    /// The person `name` , provisioned as a computer's first use provisions them.
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

    /// Certify the device key `paths` holds under this person's root, as the person admits a device
    /// of theirs: `label` is what it is called, and the capabilities are the ones the identity
    /// layer grants a joined device.
    pub fn certify(&self, paths: &IdentityPaths, label: &str) -> DeviceCert {
        self.certify_with(paths, label, None)
    }

    /// The configuration home the first use of this machine provisioned the
    /// person in.
    pub fn config_home(&self) -> PathBuf {
        self.identity.path().join("config")
    }

    /// The identity root holding this machine's device key and certificate.
    pub fn identity_root(&self) -> PathBuf {
        self.identity.path().join("system")
    }

    /// The home the first use of this machine provisioned them in.
    pub fn home(&self) -> PathBuf {
        self.identity.path().join("home")
    }

    /// Certify the device key `paths` holds as this person's site: the device a site runs, which
    /// governs the workspaces it hosts and takes part in their channels, so the records it keeps
    /// are sealed ones.
    pub fn certify_as_host(&self, paths: &IdentityPaths, label: &str) -> DeviceCert {
        self.certify_with(
            paths,
            label,
            Some(tilo_user_identity::DeviceCapabilities::host()),
        )
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
        let mut cert = tilo_user_identity::persist::provision_joined_device(
            paths,
            self.public,
            sign,
            label,
            tilo_clock::unix_now_ns(),
        )
        .expect("the device is certified under its person's root");
        if let Some(capabilities) = capabilities {
            // What a person lets this device do is theirs to say; the certificate is signed again
            // over what it now says.
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

    /// Another of this person's devices, certified under their root as a device is enrolled:
    /// `label` is what it is called.
    pub fn device(&self, label: &str) -> Device {
        let home = tempfile::tempdir().expect("a directory for the device");
        let root = home.path();
        for directory in ["config", "data", "state", "cache", "runtime", "home"] {
            std::fs::create_dir_all(root.join(directory)).expect("a device directory");
        }
        // The record of who the person is, which every device of theirs holds: never the root
        // secret, which stays with the person.
        copy_dir(
            &self.identity.path().join("config/tilo/identity"),
            &root.join("config/tilo/identity"),
        );
        // This device relays through this machine's loopback relay, as the site does: no session
        // of a test reaches the person's relays or their directory out on the internet.
        test_relay::relay_through_loopback_in(&root.join("config"));
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

impl Person {
    /// A machine copied from `source`, one of this person's devices, as
    /// `tilo-vm vm branch` copies a disk: `source`'s whole directory, taken
    /// while its session is stopped, after `source` booted once on a build
    /// that records its machine. The copy boots on a machine of its own, so
    /// the boot's own re-key ([`branch::settle`]) gives it a device key of
    /// its own; this person then certifies that key, as one approval on a
    /// device holding their root does, and the copy installs the certificate
    /// as a device that joined without the root does.
    ///
    /// On a real copy the state sits at the same path. Here the copy is
    /// elsewhere, and the secret store files the master key of the session's
    /// channel vault under the vault's path, so that key is filed under the
    /// copy's path as well: what the copied disk would have found.
    pub fn branch(&self, source: &Device) -> (Device, tilo_user_identity::branch::Branched) {
        use sha2::Digest as _;
        use tilo_user_identity::branch::{self, Branch, Machine, Settled};
        assert!(
            source.child.is_none(),
            "a disk is copied while its machine is off"
        );
        let source_machine = Machine::parse("4c4c4544-0042-3510-8048-b4c04f4d3232").unwrap();
        let branch_machine = Machine::parse("8a1f02d6-91c4-4f0e-9b9a-2d1f5c3e7a10").unwrap();
        let boot = |paths: &IdentityPaths, machine: &Machine| {
            branch::settle(
                paths,
                Some(machine),
                &Branch {
                    // Nothing a test device keeps is sealed to its key: its
                    // secrets are in the file store `TILO_SECRETS_DIR` names,
                    // under a key of that store's own.
                    stores: vec![],
                    machine: None,
                    now_ns: tilo_clock::unix_now_ns(),
                    after: &|_| Ok(()),
                },
            )
            .expect("the boot settles which device this is")
        };
        assert!(matches!(
            boot(&source.identity(), &source_machine),
            Settled::Recorded
        ));

        let home = tempfile::tempdir().expect("a directory for the copy");
        copy_machine(source.directory(), home.path());
        let vault = |root: &Path| {
            let root = root.join("state/tilo/collaboration/mls");
            format!(
                "mls-master-{}",
                hex::encode(sha2::Sha256::digest(root.as_os_str().as_encoded_bytes()))
            )
        };
        refile_secret(
            &home.path().join("secrets"),
            "so.tilo.collaboration.mls",
            &vault(source.directory()),
            &vault(home.path()),
        );

        let paths = IdentityPaths::under(home.path().join("identity"));
        let Settled::Branched(branched) = boot(&paths, &branch_machine) else {
            panic!("the copy did not re-key");
        };
        // The copy holds no root: its certificate waits for the person.
        assert!(matches!(
            branched.certification,
            branch::Certification::Pending(_)
        ));
        let mut cert = DeviceCert {
            user_public_key: self.public,
            device_public_key: branched.device,
            device_label: branched.label.clone(),
            capabilities: source.cert.capabilities.clone(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        let digest = tilo_user_identity::device_cert_signing_digest(&cert)
            .expect("a device certificate digest");
        cert.signature = self
            .root
            .sign(&digest)
            .expect("the person's root signs the copy");
        tilo_user_identity::persist::install_device_cert(&paths, &cert, tilo_clock::unix_now_ns())
            .expect("the copy installs the certificate its person signed");
        let control = home.path().join("control.sock");
        (
            Device {
                person: self.name.clone(),
                label: branched.label.clone(),
                cert,
                control,
                home,
                child: None,
            },
            *branched,
        )
    }
}

/// File the secret `app` keeps under `from` in the file store at `dir` under
/// `to` as well, as that store seals it: bound to its app and name, under the
/// store's own key. The store is opened directly because a process opens the
/// store `TILO_SECRETS_DIR` names once, and this test's is not the copy's.
fn refile_secret(dir: &Path, app: &str, from: &str, to: &str) {
    use sha2::Digest as _;
    use tilo_seal::{Context, Label, SymmetricKey, envelope};
    let record = |name: &str| [app.as_bytes(), &[0], name.as_bytes()].concat();
    let file = |record: &[u8]| {
        dir.join(format!(
            "{}.secret",
            hex::encode(sha2::Sha256::digest(record))
        ))
    };
    let key =
        SymmetricKey::from_slice(&std::fs::read(dir.join("store.key")).expect("the store's key"))
            .expect("the store's key");
    let Ok(sealed) = std::fs::read(file(&record(from))) else {
        return;
    };
    let secret = envelope::open(
        &key,
        Context::new(Label::FileSecretStore, &record(from)),
        &sealed,
    )
    .expect("the copied secret opens");
    let resealed = envelope::seal(
        &key,
        Context::new(Label::FileSecretStore, &record(to)),
        &secret,
    )
    .expect("the secret seals");
    let target = file(&record(to));
    std::fs::write(&target, resealed).expect("the secret is filed");
    std::fs::set_permissions(
        &target,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
    )
    .expect("a mode");
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

/// One device of one person, running a session of its own on this machine.
pub struct Device {
    /// The person whose device this is.
    pub person: String,
    /// What the device is called.
    pub label: String,
    /// The certificate its person's root signed for it, as a workspace admits it.
    pub cert: DeviceCert,
    home: tempfile::TempDir,
    child: Option<Child>,
    control: PathBuf,
}

impl Device {
    /// Start this device's session without joining anything: a session on someone's desk before
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

    /// Stop this device's session, as the person closing their laptop does.
    pub fn stop(&mut self) {
        let Some(child) = self.child.take() else {
            return;
        };
        // The session leads its own process group: an ending reaches everything it started.
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
            "windows: {}\nwhat its session warned:\n{}\nlast lines of its log:\n{tail}\n\
             what its apps said:\n{apps}",
            self.ask(&json!({"op": "windows"})),
            self.warnings(),
        )
    }

    /// Run this device's session: its own home, its own identity root, its own state and its own
    /// control socket, as a device's session runs.
    fn spawn(&mut self, invitation: Option<&str>) {
        let root = self.home.path().to_path_buf();
        let control = self.control.clone();
        let logs = self.logs_dir();
        std::fs::create_dir_all(&logs).expect("a directory for what the device logs");
        // A device started again keeps what its session said when it stopped: what it answered
        // last, a crash included, is what a step that fails after the restart reads.
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs.join("session.log"))
            .expect("a log for the session");
        let _ = writeln!(log, "\n--- this device's session starts ---");
        let mut command = Command::new(env!("CARGO_BIN_EXE_tilo-session"));
        command
            .args([
                "--backend",
                "headless",
                // The person's desk, with the shell that shows it: what a device's session mounts
                // and what [`Device::open`] opens into.
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
            // The apps installed on this device: what its desk opens, and what it starts them from.
            .env("TILO_APPS_DATA_DIR", root.join("data/tilo-apps"))
            // This device keeps the channel keys of the workspaces it is in here, as a machine with
            // no keychain does: without them it holds a workspace it cannot read or reach its
            // people through.
            .env(tilo_secrets::SECRETS_DIR_ENV, root.join("secrets"))
            .env(FILE_SECRETS_ENV, "1")
            // The session says at info why a join did not land: what a failed step reads. A test
            // that needs more of it asked first ([`log_sessions_at`]).
            .env("RUST_LOG", SESSION_LOG.get().map_or("info", String::as_str))
            .env("TILO_APP_STDERR_DIR", &logs)
            // The tests of this process mount the trees this device's session serves, so the
            // session records this process as the Files endpoint the login's unit would be.
            .env("TILO_FILES_ENDPOINT_PID", std::process::id().to_string())
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

    /// Ask the device's session `request` , as `tilo-control` does, and return its answer.
    pub fn ask(&self, request: &Value) -> Value {
        let mut stream = UnixStream::connect(&self.control)
            .unwrap_or_else(|error| panic!("{}'s control socket: {error}", self.label));
        // A request that asks the desk to hold its answer (`wait_ms`) is answered when what it
        // waits for happens: the read outlives that wait by the suite's hang bound, so a desk still
        // waiting for a change is waited for rather than reported as one that did not answer.
        let held = request
            .get("wait_ms")
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
                "{} did not answer {request}: {error}; its session last said:\n{}",
                self.label,
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

    /// The desk as this device shows it now: the tree, the workspaces its engine holds, its agents
    /// and its chrome.
    pub fn state(&self) -> Value {
        self.ask(&json!({"op": "state_dump"}))
    }

    /// The workspaces this device keeps, by id: a workspace it joined is one of them.
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

    /// The workspace in front on this device's desk, by id: what the door opens an app into.
    pub fn showing(&self) -> Option<String> {
        let tree = &self.state()["state"]["workspace_tree"];
        let active = tree["active"].as_u64()?;
        tree["workspaces"]
            .as_array()?
            .iter()
            .find(|workspace| workspace["id"].as_u64() == Some(active))
            .and_then(|workspace| workspace["room"].as_str().map(str::to_owned))
    }

    /// The workspaces on this device's desk, by id, in the desk's order: the ones it made, the ones
    /// it joined and the ones the person's list brought from their other devices.
    pub fn on_desk(&self) -> Vec<String> {
        self.state()["state"]["workspace_tree"]["workspaces"]
            .as_array()
            .map(|workspaces| {
                workspaces
                    .iter()
                    .filter_map(|workspace| workspace["room"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What this device's session logged of the workspaces it joined and entered, oldest first:
    /// which of them arrived on its desk, and when.
    pub fn join_flow(&self) -> String {
        self.log()
            .lines()
            .filter(|line| line.contains(tilo_startup_metrics::JOIN_FLOW_TARGET))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The publisher this device's session goes by at its own seat: what every other device in a
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
            .and_then(|held| held.iter().find(|held| held["room"] == json!(workspace)))
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
    /// workspace's own plane.
    pub fn wait_present(&self, workspace: &str, publisher: &str) {
        struct Present<'a> {
            device: &'a Device,
            workspace: &'a str,
            publisher: &'a str,
        }
        impl std::fmt::Display for Present<'_> {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    formatter,
                    "{} says {} is in {}; it says {:?}",
                    self.device.label,
                    self.publisher,
                    self.workspace,
                    self.device.present(self.workspace)
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

    /// Wait until the desk shows the workspace `workspace` in front: a person who joins a workspace
    /// lands in it, so the door opens apps into it rather than into the desk they were on.
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

    /// Install Chat in this device's store, as a device with the app installed has it: the app this
    /// run built, under its own manifest and surface.
    pub fn install_chat(&self) {
        self.install("so.tilo.chat", chat_install());
    }

    /// Install Passwords in this device's store, as a device with the app installed has it.
    pub fn install_passwords(&self) {
        self.install("so.tilo.passwords", passwords_install());
    }

    /// Install Model Providers in this device's store, as a device with the app installed has it.
    pub fn install_model_providers(&self) {
        self.install("so.tilo.model-providers", model_providers_install());
    }

    /// The provider and id this device lists for the running `app`'s `operation`, once the app
    /// published it: a provider the session names by the app's live connection.
    pub fn app_action(&self, app: &str, operation: &str) -> (String, String) {
        let mut named = None;
        tilo_test_env::wait_until(
            format!("{} publishes {app}'s {operation}", self.label),
            || {
                named = self.actions()["answer"]["actions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|row| {
                        row["application"] == app
                            && row["operation"] == operation
                            && row["provider"]
                                .as_str()
                                .is_some_and(|provider| provider.parse::<u64>().is_ok())
                    })
                    .map(|row| {
                        (
                            row["provider"].as_str().expect("a provider").to_owned(),
                            row["id"].as_str().expect("an action's id").to_owned(),
                        )
                    });
                named.is_some()
            },
        );
        named.expect("the action the app published")
    }

    /// Invoke the running `app`'s `operation` with `args` and wait until it settled, as a program
    /// of the person's does: the receipt, the person's confirmation given where it asked first.
    pub fn app_act(&self, app: &str, operation: &str, args: &Value) -> Value {
        let (provider, id) = self.app_action(app, operation);
        let answer = self.act_wait(&provider, &id, args);
        if answer["err"]["code"].as_str() == Some("needs_confirmation") {
            return self.act_confirming(&provider, &id, args);
        }
        answer
    }

    /// Install Tasks in this device's store, as a device with the app installed has it: the app this
    /// run built, with the tracker's surface its package ships.
    pub fn install_tasks(&self) {
        self.install("so.tilo.tasks", tasks_install());
    }

    /// Install the Files app in this device's store as a desktop that carries it has it: the surface
    /// its package ships, under a signed root. A session reads the records of the person's that an
    /// installed app keeps from its store, before the app ever runs, and the daemon that serves this
    /// device's trees is that app's endpoint: a session serves no tree record for an app it cannot
    /// read as installed.
    pub fn install_files(&self) {
        self.install("so.tilo.file-explorer", files_install());
    }

    /// The actions this device publishes now.
    pub fn actions(&self) -> Value {
        self.ask(&json!({"op": "actions_enumerate"}))
    }

    /// Map a window of `app` called `title` on this device's desk, as an app starting its own
    /// window does: a window of the workspace it shows, kept up while the returned value lives.
    pub fn window(&self, app: &str, title: &str) -> super::window::Client {
        super::window::Client::open(self.directory(), app, title)
    }

    /// Open the installed app `app` on this device's desk, as a person starting it does: what an
    /// installed desktop opens from its applications list.
    pub fn open(&self, app: &str) {
        let answer = self.act("applications", app, &json!({}));
        assert!(
            answer.get("err").is_none(),
            "{} opens {app}: {answer}\n{}",
            self.label,
            self.read_line_of_log()
        );
    }

    /// Invoke `id` of `provider` on this device, as a program of the person's does, and return the
    /// receipt.
    pub fn act(&self, provider: &str, id: &str, args: &Value) -> Value {
        self.ask(&json!({
            "op": "actions_invoke",
            "provider": provider,
            "id": id,
            "args": args,
        }))
    }

    /// [`Self::act`] held open until the invocation settles, as `tilo-control` does when it waits:
    /// the answer says how the action ended, and carries the value it returned when it returns one.
    pub fn act_wait(&self, provider: &str, id: &str, args: &Value) -> Value {
        self.invoke(provider, id, args, false)
    }

    /// [`Self::act_wait`], with the person's own confirmation of an action that asked first: what
    /// pressing Allow in its prompt answers.
    pub fn act_confirming(&self, provider: &str, id: &str, args: &Value) -> Value {
        self.invoke(provider, id, args, true)
    }

    /// [`Self::act_wait`], with what the person said when the action asked first (`confirmed`),
    /// followed until it settles.
    fn invoke(&self, provider: &str, id: &str, args: &Value, confirmed: bool) -> Value {
        let started = self.ask(&json!({
            "op": "actions_invoke",
            "provider": provider,
            "id": id,
            "args": args,
            "confirmed": confirmed,
        }));
        let Some(number) = started["answer"]["receipt"]["broker_invocation_id"].as_u64() else {
            return started;
        };
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let answer = self.ask(&json!({
                "op": "actions_status",
                "invocation_id": number,
                "wait_ms": 30_000,
            }));
            match answer["answer"]["receipt"]["update"]["state"].as_str() {
                Some("accepted" | "progress") if Instant::now() < deadline => {}
                _ => return answer,
            }
        }
    }

    /// Chat as this device runs it: the actions the app publishes to its session's door, and the
    /// answers they give.
    pub fn chat(&self) -> Chat<'_> {
        Chat { device: self }
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

    /// Wait until a window of the app `app` is open on this device's desk.
    ///
    /// An app that started and stopped before its window came is not one to wait longer for: the
    /// wait ends with what the session logged of it, which is where a sandbox that could not start
    /// (or a launcher that refused) says why. Waiting out the hang bound instead would report a
    /// picture of the desk and nothing about the launch.
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

    /// Whether this device's desk shows a window of `app` .
    fn shows_window(&self, app: &str) -> bool {
        self.ask(&json!({"op": "windows"}))["answer"]["windows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["app_id"].as_str() == Some(app)))
    }

    /// What this device's session logged of `app` ending, once the log had `already_logged` lines:
    /// the app stopping, and the lines the app itself printed before it did.
    fn ended_since(&self, already_logged: usize, app: &str) -> Option<String> {
        let log = self.log();
        let lines: Vec<&str> = log.lines().skip(already_logged).collect();
        let stopped = lines.iter().position(|line| {
            line.contains("an app stopped") && line.contains(&format!("app={app}"))
        })?;
        let said: Vec<&str> = lines[stopped..]
            .iter()
            .copied()
            .filter(|line| line.contains(&format!("app={app}")) || line.contains("tilo-launcher"))
            .take(20)
            .collect();
        Some(said.join("\n"))
    }
}

/// A device whose desk still shows no `app` window, named on the wait that found it short: what it
/// shows, and what its session and its apps said.
struct NoWindow<'a> {
    device: &'a Device,
    app: &'a str,
}

impl std::fmt::Display for NoWindow<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} to show a {} window; it shows {}\n{}",
            self.device.label,
            self.app,
            self.device.ask(&json!({"op": "windows"})),
            self.device.diagnostic()
        )
    }
}

/// Chat as a device runs it: the app publishes its reading and sending as actions of its own, so a
/// test asks the app what it shows rather than its session or its records.
pub struct Chat<'a> {
    device: &'a Device,
}

impl Chat<'_> {
    /// The provider and id the session publishes for the app's `operation`,
    /// once the running app published it: the app names its own actions, so a
    /// caller asks rather than writing them down.
    fn action(&self, operation: &str) -> (String, String) {
        let mut named = None;
        tilo_test_env::wait_until(
            format!("{} publishes Chat's {operation}", self.device.label),
            || {
                let answer = self.device.actions();
                named = answer["answer"]["actions"].as_array().and_then(|rows| {
                    rows.iter()
                        // The running app's own: a provider the session names by the app's
                        // live connection, not the launcher that would start it again.
                        .find(|row| {
                            row["application"] == "so.tilo.chat"
                                && row["operation"] == operation
                                && row["provider"]
                                    .as_str()
                                    .is_some_and(|provider| provider.parse::<u64>().is_ok())
                        })
                        .map(|row| {
                            (
                                row["provider"].as_str().expect("a provider").to_owned(),
                                row["id"].as_str().expect("an action's id").to_owned(),
                            )
                        })
                });
                named.is_some()
            },
        );
        named.expect("the action the session published")
    }

    /// Invoke the app's `operation` with `args` and wait for what it answered: the value it
    /// returned, or the refusal it gave. An action the workspace asks the person about first is
    /// answered, as the person at the device answers its prompt.
    fn invoke(&self, operation: &str, args: &Value) -> Value {
        let answer = self.settled(operation, args);
        let update = &answer["answer"]["receipt"]["update"];
        match update["state"].as_str() {
            Some("succeeded") => update["output"]["value"].clone(),
            Some("failed") => panic!(
                "{} Chat's {operation} failed: {}\n{answer}\n{}",
                self.device.label,
                update["output"]["error"],
                self.device.diagnostic()
            ),
            other => panic!(
                "{} Chat's {operation} ended as {other:?}: {answer}\n{}",
                self.device.label,
                self.device.diagnostic()
            ),
        }
    }

    /// The app's answer to `operation` with `args` once it settled: the person's confirmation given
    /// where the action asked for it.
    pub fn settled(&self, operation: &str, args: &Value) -> Value {
        let (provider, id) = self.action(operation);
        let answer = self.device.act_wait(&provider, &id, args);
        if answer["err"]["code"].as_str() == Some("needs_confirmation") {
            return self.device.act_confirming(&provider, &id, args);
        }
        answer
    }

    /// The person a conversation with `peer` is with, as the app's direct actions take it: their
    /// root key, which the workspace names them by.
    fn conversation_key(peer: &str) -> String {
        peer.to_owned()
    }

    /// Open the conversation with `peer` in the app, as clicking their name does: what is on screen
    /// is read, so their messages stop counting as unread here.
    pub fn open(&self, peer: &str) {
        self.invoke(
            "open-direct-conversation",
            &json!({"public_key": Self::conversation_key(peer)}),
        );
    }

    /// Send `text` to the person `peer` , as typing it in the conversation does: the app answers
    /// when it accepted the message into the record.
    pub fn send(&self, peer: &str, text: &str) {
        let answer = self.invoke(
            "send-direct-message",
            &json!({"public_key": Self::conversation_key(peer), "text": text}),
        );
        assert_eq!(
            answer["accepted"],
            json!(true),
            "{} sends to {peer}: {answer}",
            self.device.label
        );
    }

    /// The conversation with `peer` as Chat's record holds it here now: its messages in the order
    /// the app shows them, and how many of them its person has not read.
    pub fn conversation(&self, peer: &str) -> Conversation {
        self.read(peer).unwrap_or_else(|refusal| {
            panic!(
                "{} Chat's read of {peer} failed: {refusal}\n{}",
                self.device.label,
                self.device.diagnostic()
            )
        })
    }

    /// The conversation with `peer` as Chat's record on this device holds it, or why there is
    /// none: a person who is in no workspace with this device's person has no conversation here,
    /// and until the workspace's people are in its shared record nobody has one.
    fn read(&self, peer: &str) -> Result<Conversation, String> {
        self.held(None).conversation(peer)
    }

    /// Chat's record and the first part of its messages (all a conversation here writes), as this
    /// device holds them, once they differ from the revision `after`: read through the record door
    /// as plain JSON, which wakes the read on the next write to any of them.
    fn held(&self, after: Option<&str>) -> HeldChat {
        let wait = tilo_test_env::hang_bound();
        let answer = self.device.ask(&json!({
            "op": "record_read",
            "app": "so.tilo.chat",
            "records": ["", chat::parts::part(0).to_string()],
            "after": after,
            "wait_ms": u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
        }));
        assert!(
            answer.get("err").is_none(),
            "{} reads Chat's record: {answer}",
            self.device.label
        );
        let answer = &answer["answer"];
        let mut values = answer["records"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|record| record["value"].clone());
        HeldChat {
            me: hex::encode(self.device.cert.user_public_key),
            revision: answer["revision"].as_str().unwrap_or_default().to_owned(),
            chat: values.next().unwrap_or_default(),
            parts: values.collect(),
        }
    }

    /// Wait until Chat's record here holds what `wanted` names, woken by each write to it: `done`
    /// says what it holds instead, `None` once it holds that.
    fn wait_for(&self, wanted: &str, mut done: impl FnMut(&HeldChat) -> Option<String>) {
        // Chat's record is sealed in the workspace's channel, and what a device reads of it follows
        // the channel service's own longest wait between asking for its keys: the wait spans that
        // and the suite's hang bound, and each answer it is woken by is its progress.
        let deadline = Instant::now()
            .checked_add(
                tilo_channels::asking_again_wait().saturating_add(tilo_test_env::hang_bound()),
            )
            .expect("the wait's bound fits in an Instant");
        let mut after = None;
        loop {
            let held = self.held(after.as_deref());
            let Some(instead) = done(&held) else {
                return;
            };
            assert!(
                Instant::now() < deadline,
                "{DIAG}{} {instead} instead of {wanted}\n\
                 what the session warned and failed at:\n{}",
                self.device.label,
                self.device.warnings(),
                DIAG = self.device.diagnostic()
            );
            after = Some(held.revision);
        }
    }

    /// The texts of the conversation with `peer` , in the order Chat shows them.
    pub fn texts(&self, peer: &str) -> Vec<String> {
        self.conversation(peer).texts()
    }

    /// How many of the messages `peer` wrote this device's person has not read, as Chat counts
    /// them.
    pub fn unread(&self, peer: &str) -> u64 {
        self.conversation(peer).unread
    }

    /// Why Chat has no conversation with `peer` here: `None` when it has one.
    pub fn refusal(&self, peer: &str) -> Option<String> {
        self.read(peer).err()
    }

    /// Wait until Chat offers `peer` as someone to talk to: until the workspace's people are in its
    /// shared record, a conversation with them reads as unavailable rather than as held.
    pub fn wait_for_contact(&self, peer: &str) {
        self.wait_for(&format!("{peer} to talk to"), |held| {
            held.conversation(peer).err()
        });
    }

    /// Wait until the conversation with `peer` shows `texts` in that order,
    /// and nothing else.
    pub fn wait_for_texts(&self, peer: &str, texts: &[&str]) {
        self.wait_for_texts_ordered(peer, texts, true);
    }

    /// Wait until the conversation with `peer` shows `texts` , and nothing else, in whatever order:
    /// for messages written at the same time on devices that had not seen each other's, whose order
    /// is the records' and not the writers'.
    pub fn wait_for_texts_in_any_order(&self, peer: &str, texts: &[&str]) {
        self.wait_for_texts_ordered(peer, texts, false);
    }

    fn wait_for_texts_ordered(&self, peer: &str, texts: &[&str], in_order: bool) {
        let mut wanted: Vec<&str> = texts.to_vec();
        if !in_order {
            wanted.sort_unstable();
        }
        self.wait_for(&format!("{texts:?} from {peer}"), |held| {
            let mut shown = held.conversation(peer).map(|held| held.texts());
            if let (Ok(shown), false) = (&mut shown, in_order) {
                shown.sort_unstable();
            }
            match shown {
                Ok(shown) if shown.iter().map(String::as_str).eq(wanted.iter().copied()) => None,
                shown => Some(format!("shows {shown:?}")),
            }
        });
    }

    /// Wait until Chat counts `wanted` messages of `peer` as unread here.
    pub fn wait_for_unread(&self, peer: &str, wanted: u64) {
        self.wait_for(&format!("{wanted} unread from {peer}"), |held| {
            match held.conversation(peer).map(|held| held.unread) {
                Ok(count) if count == wanted => None,
                count => Some(format!("counts {count:?}")),
            }
        });
    }
}

/// Chat's record and the parts of its messages as a device holds them, read at `revision`.
struct HeldChat {
    /// The device's person, as the workspace names them.
    me: String,
    revision: String,
    chat: Value,
    /// Each part of the chain, in its order.
    parts: Vec<Value>,
}

impl HeldChat {
    /// The conversation with `peer` as Chat shows it, or why there is none: its messages in the
    /// chain's order (each part's, by the order key the record keeps), and how many of the
    /// messages `peer` wrote lie past this person's read mark (a part, and how many of the
    /// conversation's messages in it they saw).
    fn conversation(&self, peer: &str) -> Result<Conversation, String> {
        use chat::record::{Chat, Participant};
        let people = self.chat["people"]
            .as_object()
            .filter(|people| !people.is_empty());
        let Some(people) = people else {
            return Err("the workspace's people are not in Chat's record yet".into());
        };
        if peer == self.me
            || people
                .get(peer)
                .is_none_or(|person| person["agent"] == true)
        {
            return Err(format!("Chat knows no person {peer}"));
        }
        let channel = Chat::direct_id([
            Participant::person(self.me.clone()),
            Participant::person(peer),
        ])
        .to_string();
        let mark = &self.chat["read"][&self.me][&channel];
        let (from, seen) = (
            mark["part"].as_u64().unwrap_or(0),
            mark["seen"].as_u64().unwrap_or(0),
        );
        let mut messages = Vec::new();
        let mut unread = 0u64;
        for (index, part) in (0u64..).zip(&self.parts) {
            let mut held: Vec<(&String, &Value)> = part["messages"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, entry)| entry["v"]["channel"] == channel.as_str())
                .collect();
            held.sort_by(|(a, x), (b, y)| (x["k"].as_str(), a).cmp(&(y["k"].as_str(), b)));
            let skip = if index == from { seen } else { 0 };
            let top = held
                .iter()
                .filter(|(_, entry)| entry["v"]["parent"].is_null());
            let past = top
                .skip(usize::try_from(skip).unwrap_or(usize::MAX))
                .filter(|(_, entry)| index >= from && entry["v"]["author"] != self.me.as_str())
                .count();
            unread = unread.saturating_add(u64::try_from(past).unwrap_or(u64::MAX));
            messages.extend(held.into_iter().map(|(id, entry)| {
                let message = &entry["v"];
                json!({"id": id, "author": message["author"], "text": message["text"], "at": message["ts"]})
            }));
        }
        Ok(Conversation { messages, unread })
    }
}

/// A conversation as Chat shows it: its messages in the app's order, and what of it its person has
/// not read.
pub struct Conversation {
    /// Each message as the app answers it: its id, its author and its text.
    pub messages: Vec<Value>,
    /// How many of the conversation's messages its person has not read.
    pub unread: u64,
}

impl Conversation {
    /// The messages' texts, in the app's order.
    fn texts(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter_map(|message| message["text"].as_str().map(str::to_owned))
            .collect()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The payload Chat is installed from here ([`stage`]), with the digest of its signed root.
fn chat_install() -> &'static (PathBuf, String) {
    static INSTALL: std::sync::OnceLock<(PathBuf, String)> = std::sync::OnceLock::new();
    INSTALL.get_or_init(|| {
        stage(
            "chat",
            &[
                (tilo_test_env::sibling_binary("chat", "chat"), "chat"),
                (
                    tilo_test_env::sibling_binary("tilo-runtime", "tilo-runtime"),
                    "tilo-runtime",
                ),
            ],
        )
    })
}

/// The payload Files is installed from here ([`stage`]), with the digest of its signed root.
fn files_install() -> &'static (PathBuf, String) {
    static INSTALL: std::sync::OnceLock<(PathBuf, String)> = std::sync::OnceLock::new();
    INSTALL.get_or_init(|| stage("file-explorer", &[]))
}

/// The payload Passwords is installed from here ([`stage`]), with the digest of its signed root.
fn passwords_install() -> &'static (PathBuf, String) {
    static INSTALL: std::sync::OnceLock<(PathBuf, String)> = std::sync::OnceLock::new();
    INSTALL.get_or_init(|| {
        stage(
            "passwords",
            &[(
                tilo_test_env::sibling_binary("passwords", "passwords"),
                "passwords",
            )],
        )
    })
}

/// The payload Model Providers is installed from here ([`stage`]), with the digest of its signed
/// root.
fn model_providers_install() -> &'static (PathBuf, String) {
    static INSTALL: std::sync::OnceLock<(PathBuf, String)> = std::sync::OnceLock::new();
    INSTALL.get_or_init(|| {
        stage(
            "model-providers",
            &[(
                tilo_test_env::sibling_binary("model-providers", "model-providers"),
                "model-providers",
            )],
        )
    })
}

/// The payload Tasks is installed from here ([`stage`]), with the digest of its signed root.
fn tasks_install() -> &'static (PathBuf, String) {
    static INSTALL: std::sync::OnceLock<(PathBuf, String)> = std::sync::OnceLock::new();
    INSTALL.get_or_init(|| {
        stage(
            "tasks",
            &[(tilo_test_env::sibling_binary("tasks", "tasks"), "tasks")],
        )
    })
}

/// `app`'s payload staged in the build's own directory, as an install leaves a package: its
/// manifest and the surface its package ships, the signed root and collection an install writes
/// beside them, and each of `programs` the run built, hardlinked under `bin/` so a device pays for
/// no copy of a debug binary. The session reads a package's surface, and with it the records and
/// person records the app keeps, only from a payload whose signature covers it: a payload staged
/// without one is no install of the app at all.
fn stage(app: &str, programs: &[(PathBuf, &str)]) -> (PathBuf, String) {
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
    let manifest = manifest_of(app).into_bytes();
    let surface_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../../apps/{app}/surface.toml"));
    let surface = std::fs::read(&surface_path)
        .unwrap_or_else(|error| panic!("{app}'s surface at {}: {error}", surface_path.display()));
    std::fs::write(payload.join("manifest.toml"), &manifest).expect("the app's manifest");
    std::fs::write(payload.join("surface.toml"), &surface).expect("the app's surface");
    let collection =
        Collection::from_entries([("surface.toml".to_string(), BlobHash::of(&surface))]);
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
/// installs it: the app's own description (`apps/<app>/tilo-app.toml`), without the runtime bundle
/// it names. An installed desktop carries that bundle; a build runs on the machine it was built on.
fn manifest_of(app: &str) -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../../apps/{app}/tilo-app.toml"));
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

/// The role a member holds in a workspace, as [`WorkspaceRole`] names the workspace's listing of
/// it.
fn member_role(role: WorkspaceRole) -> tilo_workspace_join::MemberRole {
    match role {
        WorkspaceRole::Viewer => tilo_workspace_join::MemberRole::Viewer,
        WorkspaceRole::Commenter => tilo_workspace_join::MemberRole::Commenter,
        WorkspaceRole::Editor => tilo_workspace_join::MemberRole::Editor,
        WorkspaceRole::Moderator => tilo_workspace_join::MemberRole::Moderator,
    }
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

/// Copy the tree `from` to `to` , keeping the modes files are read with.
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
