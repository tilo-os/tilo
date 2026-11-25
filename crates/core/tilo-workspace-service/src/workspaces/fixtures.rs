//! A governing holder and the devices around it, for the tests of workspaces and of
//! the app action servers embedders build on them.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use key_group::{KeyGroup, MemberKey};

/// The content keys a fixture's device seals its records under
/// ([`GroupKeys`]): what a test that builds its own documents hands the service.
pub use key_group::ContentKeys;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use super::{
    DeviceKeyGroups, GroupKeys, GroupKeysAnswerFuture, KeysChangedFuture, MemberDevice,
    PreAdmitRequest, WorkspaceSummary, Workspaces,
};
use crate::service::{ServiceConfig, ServiceIdentity, WorkspaceService};

/// `result`, which a fixture's setup cannot go without: failing there fails the test that stands
/// on it, loudly, rather than leaving a test to pass over a setup that did nothing.
///
/// # Panics
///
/// `result` is an `Err`.
pub fn needed<T, E: std::fmt::Display>(result: Result<T, E>, what: &str) -> T {
    result.unwrap_or_else(|error| panic!("{what}: {error}"))
}

/// Another device of the person whose root (`[1; 32]`) certified the fixture's governor
/// `cert`, admitted to `workspace`.
pub async fn member_device(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    seed: u8,
) -> (SigningKey, DeviceCert) {
    let member_key = SigningKey::from_bytes(&[seed; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    workspaces
        .enter(workspace, (&member, &member_key))
        .await
        .expect("the person's further device lists itself");
    (member_key, member)
}

/// A holder certified only to keep workspaces: it governs them without taking part in
/// their channels.
#[must_use]
pub fn governor() -> (SigningKey, DeviceCert) {
    governor_with(DeviceCapabilities::member())
}

/// A holder's device of the person whose root is `[1; 32]`, certified with
/// `capabilities`.
///
/// # Panics
///
/// The certificate cannot be signed.
#[must_use]
pub fn governor_with(capabilities: DeviceCapabilities) -> (SigningKey, DeviceCert) {
    device_cert(&SigningKey::from_bytes(&[1; 32]), "home", capabilities)
}

/// A person's device certificate, as a guest's browser makes one.
pub fn guest_device() -> (SigningKey, DeviceCert) {
    device_cert(
        &SigningKey::from_bytes(&rand::random()),
        "guest",
        DeviceCapabilities::member(),
    )
}

/// A new device of `root`'s person, labelled `label` and certified with
/// `capabilities`: the key it signs with and its certificate.
///
/// # Panics
///
/// The certificate cannot be signed.
#[must_use]
pub fn device_cert(
    root: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).expect("a fixture certificate signs");
    (key, cert)
}

/// A certified workspace service on its own endpoint, routed as an embedder routes
/// it.
pub struct TestHost {
    host: Arc<WorkspaceService>,
    router: Router,
    /// The relay its endpoint took as its home ([`reached_through`]), closed
    /// after the endpoint that reaches it.
    #[cfg(test)]
    _home: tilo_test_relay::Relay,
}

impl TestHost {
    /// The workspaces it governs.
    #[must_use]
    pub fn workspaces(&self) -> &Workspaces {
        self.host.workspaces()
    }

    /// The host itself.
    #[must_use]
    pub fn host(&self) -> &WorkspaceService {
        &self.host
    }

    /// The host, shared with what answers its steps from another thread, as an embedder
    /// installs its service where its apps reach it.
    #[must_use]
    pub fn shared(&self) -> Arc<WorkspaceService> {
        Arc::clone(&self.host)
    }

    /// The durable records of its workspaces.
    #[must_use]
    pub fn workspace_documents(&self) -> crate::workspace_documents::WorkspaceDocuments {
        self.host.workspace_documents()
    }

    /// What a member dials.
    #[must_use]
    pub fn node_addr(&self) -> iroh_base::EndpointAddr {
        self.host.node_addr()
    }

    /// Stop the host, then its router and endpoint.
    pub async fn shutdown(self) {
        self.host.shutdown().await;
        let _closed = self.router.shutdown().await;
    }
}

/// The channel service of a device that takes part in its workspaces' channels:
/// it opens a channel's keys the first time something starts the channel's
/// record, as the channel service of the device a page or a bot runs on does,
/// and answers with the same keys every time after.
pub struct TestGroupKeys {
    held: Mutex<BTreeMap<String, ContentKeys>>,
    /// Whether the device opens no group itself: it holds keys only once they are handed to it
    /// ([`Self::hand`]), as a device a group has not let in yet.
    admitted_only: std::sync::atomic::AtomicBool,
    /// How many times keys were handed to the device, for whoever waits for them.
    handed: tokio::sync::watch::Sender<u64>,
    /// The groups a read asked the person's other devices for the frames of, and the epoch the
    /// device held at the time.
    behind: Mutex<Vec<(collab_types::WorkspaceId, u64)>>,
    /// Whether the key service is not ready: it answers no group until it is unpaused, as a
    /// device service whose persisted keys are not open yet.
    paused: std::sync::atomic::AtomicBool,
}

impl Default for TestGroupKeys {
    fn default() -> Self {
        Self {
            held: Mutex::default(),
            admitted_only: std::sync::atomic::AtomicBool::new(false),
            handed: tokio::sync::watch::Sender::new(0),
            behind: Mutex::default(),
            paused: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl TestGroupKeys {
    /// Simulate startup of a device service whose persisted keys are not open yet.
    pub fn pause(&self, paused: bool) {
        self.paused
            .store(paused, std::sync::atomic::Ordering::Release);
    }

    /// The keys of a device that opens no group itself: it reads a sealed record only once a
    /// group's keys are handed to it ([`Self::hand`]), as a device that joined a workspace holds
    /// none until the group lets it in.
    #[must_use]
    pub fn admitted_only() -> Self {
        Self {
            admitted_only: std::sync::atomic::AtomicBool::new(true),
            ..Self::default()
        }
    }

    /// The device holds no group's keys any more and opens none itself, as a host whose state is
    /// fresh and whose channel has not let it in yet.
    pub fn hold_nothing(&self) {
        self.held.lock().expect("the fixture's keys").clear();
        self.admitted_only
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.handed
            .send_modify(|handed| *handed = handed.wrapping_add(1));
    }

    /// Hand the fixture the keys of `workspace`'s every-member group that `other` holds, as the
    /// group lets a new member in: never the keys of a group of fewer readers.
    ///
    /// # Panics
    ///
    /// `workspace` is no workspace's id, or `other` holds no keys of its group.
    pub fn hand_every_member_of(&self, other: &Self, workspace: &str) {
        let group = collab_types::reader_group(
            collab_types::WorkspaceId::parse(workspace).expect("a workspace id"),
            collab_types::EVERY_MEMBER,
        )
        .expect("every member's group")
        .to_string();
        let keys = other
            .held
            .lock()
            .expect("the other fixture's keys")
            .get(&group)
            .expect("the other fixture holds every member's keys")
            .clone();
        self.hand(workspace, keys);
    }

    /// The groups a read asked the person's other devices for the frames of, and the epoch this
    /// device held then, oldest first.
    #[must_use]
    pub fn asked_behind(&self) -> Vec<(collab_types::WorkspaceId, u64)> {
        self.behind.lock().expect("the fixture's asks").clone()
    }

    /// Hand the fixture `keys` as every member's keys of `workspace`: keys that reach a device
    /// after it opened.
    ///
    /// # Panics
    ///
    /// `workspace` is no workspace's id.
    pub fn hand(&self, workspace: &str, keys: ContentKeys) {
        let group = collab_types::reader_group(
            collab_types::WorkspaceId::parse(workspace).expect("a workspace id"),
            collab_types::EVERY_MEMBER,
        )
        .expect("every member's group")
        .to_string();
        self.held
            .lock()
            .expect("the fixture's keys")
            .insert(group, keys);
        self.handed
            .send_modify(|handed| *handed = handed.wrapping_add(1));
    }
}

impl GroupKeys for TestGroupKeys {
    fn group_keys<'a>(
        &'a self,
        workspace: &'a str,
        readers: &'a str,
        create: bool,
    ) -> GroupKeysAnswerFuture<'a> {
        Box::pin(async move {
            // The route the endpoint seals for: a workspace's every-member group, or the
            // record's own group when the route names one.
            let _ = readers;
            if self.paused.load(std::sync::atomic::Ordering::Acquire) {
                return Some(Err("the fixture's key service is not ready".into()));
            }
            let group = neighbour_wire::KeyScope::parse(workspace)
                .expect("a key scope")
                .group()
                .to_string();
            let workspace = group.as_str();
            let held = {
                let keys = self.held.lock().expect("the fixture's keys");
                keys.get(workspace).cloned()
            };
            if let Some(keys) = held {
                return Some(Ok(keys));
            }
            if !create
                || self
                    .admitted_only
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Some(Err("this channel has no keys yet".into()));
            }
            let keys = KeyGroup::create(
                collab_types::WorkspaceId::parse(workspace).expect("a workspace id"),
                MemberKey::generate(workspace).expect("a fixture member key"),
            )
            .expect("a fixture channel opens")
            .content_keys()
            .expect("a fresh channel holds its epoch's keys");
            self.held
                .lock()
                .expect("the fixture's keys")
                .insert(workspace.to_owned(), keys.clone());
            Some(Ok(keys))
        })
    }

    fn keys_changed<'a>(&'a self, _workspace: &'a str, after: u64) -> KeysChangedFuture<'a> {
        let mut handed = self.handed.subscribe();
        Box::pin(async move {
            let handed = handed.wait_for(|handed| *handed != after).await.ok()?;
            Some(*handed)
        })
    }

    fn behind(&self, group: collab_types::WorkspaceId, held: u64) {
        self.behind
            .lock()
            .expect("the fixture's asks")
            .push((group, held));
    }
}

/// Name `workspace` as `name` in the person's list of workspaces, kept by the device `hint`
/// signs for, as a device of the person's that keeps their own workspace (`writer`) writes
/// the list.
///
/// # Panics
///
/// `writer` keeps no copy of its person's workspace, or the list could not be written.
pub async fn list_workspace(
    writer: &Workspaces,
    workspace: &str,
    name: &str,
    hint: &tilo_peer_state::join::TransportHint,
) {
    let person = writer.device_cert().user_public_key;
    let own = tilo_server_protocol::servers::person_workspace_id(&person);
    let kept = super::person_list::KeptAt {
        device: hint.host.device_public_key,
        hint: Some(hint.clone()),
    };
    for invoke in super::person_list::listing(workspace, name, Some(&kept)) {
        super::person_workspace::apply_on(
            writer,
            &own,
            tilo_server_protocol::workspaces::IDENTITY_LIST,
            invoke,
        )
        .await
        .expect("the person's list names the workspace");
    }
}

/// Whether the person's list of workspaces, as `reader`'s copy of their workspace has it, names
/// `workspace`: `false` while the list has not reached it.
pub async fn lists(reader: &Workspaces, workspace: &str) -> bool {
    let person = reader.device_cert().user_public_key;
    super::person_workspace::read_on::<tilo_user_identity::WorkspaceList>(
        reader,
        &tilo_server_protocol::servers::person_workspace_id(&person),
        tilo_server_protocol::workspaces::IDENTITY_LIST,
    )
    .await
    .is_ok_and(|list| {
        list.listed()
            .any(|(listed, _)| listed.to_string() == workspace)
    })
}

/// A certified host of `cert`'s device keeping its state under `state_root`,
/// taking part in its workspaces' channels as every server that governs
/// workspaces does: what its members seal it opens, and what it writes itself
/// it seals.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
/// A device's own keys, as the channel service of the device a workspace is founded on holds
/// them: what a founding seals the workspace's records with.
#[must_use]
pub fn founding_device() -> crate::documents::device_keys::DeviceKeys {
    Arc::<TestGroupKeys>::default()
}

pub async fn test_host(state_root: &Path, key: &SigningKey, cert: &DeviceCert) -> TestHost {
    test_host_with_grants(state_root, key, cert, None).await
}

/// The kind of `app`'s record [`write_record`] writes: one whose fields the tests write
/// anything into.
#[must_use]
pub fn record_kind(app: &str) -> String {
    format!("test.{app}.record")
}

/// Change `app`'s record `record` of `workspace` with `edit`, as this service's own write of it
/// in the workspace's store, its kind ([`record_kind`]) declared first.
///
/// # Errors
/// The workspace is not held here, or its store refused the write.
pub async fn write_record(
    workspaces: &Workspaces,
    workspace: &str,
    (app, record): (&str, &str),
    edit: impl FnOnce(&mut automerge::AutoCommit) -> Result<(), String>,
) -> Result<(), String> {
    let kind = record_kind(app);
    let field_lines: Vec<String> = [
        "attachment",
        "attachments",
        "body",
        "edge",
        "more",
        "part",
        "photo",
        "photos",
        "picture",
        "round",
        "text",
        "title",
    ]
    .iter()
    .map(|field| format!("  {{ path = \"{field}\", type = \"any\" }},\n"))
    .collect();
    let fields = field_lines.join("");
    let surface = format!(
        "format = 2\n\n[[resources]]\nkind = \"{kind}\"\nlabel = \"Record\"\nversion = 1\n\
         fields = [\n{fields}]\n\n[[resources.verbs]]\nid = \"type\"\nkind = \"undoable\"\n"
    );
    workspaces.declare_package_surface(app, surface.as_bytes());
    workspaces
        .author_record(workspace, (app, &kind, record), edit)
        .await
}

/// Name `app`'s `surface`, the `surface.toml` its package ships, on `host`, as running that
/// package does: the types and verbs its workspaces judge their resource writes by. A test whose
/// app is a package this build carries declares its own surface ([`declare_built_in_app`]); one
/// whose app no package exists for passes the surface it writes by.
pub fn declare_app(host: &TestHost, app: &str, surface: &[u8]) {
    host.workspaces().declare_package_surface(app, surface);
}

/// [`declare_app`] for `app`, a package this build carries: its own `surface.toml`, compiled
/// from its sources, as a device that runs it names it.
///
/// # Panics
///
/// This build carries no package `app`.
#[cfg(feature = "agents")]
pub fn declare_built_in_app(host: &TestHost, app: &str) {
    let surface = host
        .workspaces()
        .tilolang()
        .and_then(|tilolang| tilolang.records.surface(app))
        .unwrap_or_else(|| panic!("this build carries no package {app}"));
    declare_app(host, app, &surface);
}

/// Install `app` in `workspace` as the person this service's device acts for, at the version
/// whose surface this device declares for it ([`declare_app`]), or a version that publishes
/// none where it declares none, as the workspace's `install` action records one: the workspace
/// runs that version, so the app's writes name it, and installing gives the app its own records.
///
/// # Errors
/// The workspace refused the install.
pub async fn install_app(
    workspaces: &Workspaces,
    workspace: &str,
    app: &str,
) -> Result<(), String> {
    let surface = collab_types::app_surface::hash_of(app)
        .map(hex::encode)
        .unwrap_or_default();
    let release = tilo_peer_state::workspace_membership::AppRelease {
        app: app.into(),
        version: "1.0.0".into(),
        surface,
        package: "00".repeat(32),
        publisher: "00".repeat(32),
        file: None,
        dev: true,
        pinned: false,
        payload: tilo_peer_state::workspace_membership::AppPayload::Tapp,
    };
    workspaces
        .commit_own_change(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::InstallApp(release),
            None,
        )
        .await
}

/// Uninstall `app` from `workspace` as the person this service's device acts for.
///
/// # Errors
/// The workspace refused it.
pub async fn uninstall_app(
    workspaces: &Workspaces,
    workspace: &str,
    app: &str,
) -> Result<(), String> {
    workspaces
        .commit_own_change(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::UninstallApp {
                app: app.into(),
            },
            None,
        )
        .await
}

/// Revoke the grant `id` in `workspace` as the person this service's device acts for, as the
/// workspace's Permissions tab does: the workspace's grants decide whether they may.
///
/// # Errors
/// Why the workspace refused the revocation.
pub async fn revoke_grant(
    workspaces: &Workspaces,
    workspace: &str,
    id: tilo_action_policy::GrantId,
) -> Result<(), String> {
    workspaces
        .commit_own_change(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Revoke(id),
            },
            None,
        )
        .await
}

/// Take `grant` into `workspace`'s policy as the person this service's device acts for: the
/// answer they give for an agent of theirs, which the policy then decides that agent's calls by.
///
/// # Errors
///
/// Why the workspace refused the grant, or does not hold this device as a member.
pub async fn answer_grant(
    workspaces: &Workspaces,
    workspace: &str,
    grant: tilo_action_policy::Grant,
) -> Result<(), String> {
    workspaces
        .commit_own_change(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Grant(grant),
            },
            None,
        )
        .await
}

/// The same host, keeping `grants` pre-admissions per workspace
/// ([`crate::service::ServiceConfig::max_workspace_grants`]).
///
/// # Panics
///
/// The endpoint cannot bind or the server cannot open.
pub async fn test_host_with_grants(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    grants: Option<usize>,
) -> TestHost {
    host_on(
        state_root,
        key,
        cert,
        DeviceKeyGroups {
            keys: Arc::<TestGroupKeys>::default(),
            sink: None,
        },
        grants,
        None,
        Vec::new(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`test_host`] on a device that takes part in no workspace's channels: it seals nothing, and
/// keeps every document it writes in the clear, as a device whose embedder runs no channel
/// service beside it does.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host_in_the_clear(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
) -> TestHost {
    host_on(
        state_root,
        key,
        cert,
        None,
        None,
        None,
        Vec::new(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`test_host`] whose device holds `keys`: the same keys open
/// what a host before it on the same state sealed, as a device's channel
/// service keeps its keys across a restart.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host_holding(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    keys: Arc<TestGroupKeys>,
) -> TestHost {
    host_on(
        state_root,
        key,
        cert,
        DeviceKeyGroups { keys, sink: None },
        None,
        None,
        Vec::new(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`test_host`] whose device's native apps kept their files below `homes`
/// ([`crate::service::ServiceConfig::app_files`]).
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host_with_app_files(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    homes: Vec<std::path::PathBuf>,
) -> TestHost {
    host_on(
        state_root,
        key,
        cert,
        DeviceKeyGroups {
            keys: Arc::<TestGroupKeys>::default(),
            sink: None,
        },
        None,
        None,
        homes,
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`test_host_holding`] opened as the device `key`/`cert` acts as, as every device's service is
/// opened: the member copies it kept from its last run sync again from the moment it opens.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host_holding_as_member(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    keys: Arc<TestGroupKeys>,
) -> TestHost {
    host_on(
        state_root,
        key,
        cert,
        DeviceKeyGroups { keys, sink: None },
        None,
        Some(Arc::new(MemberDevice {
            cert: cert.clone(),
            signer: Arc::new(key.clone()),
        })),
        Vec::new(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`test_host_holding`] whose service opens with the language runtime `tilolang` mounted, as
/// the daemon's program opens the one it links: the types and verbs of the tilolang apps the
/// build carries are declared on it, so a member's write of one of their records is judged.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
#[cfg(not(target_arch = "wasm32"))]
pub async fn test_host_holding_mounting(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    keys: Arc<TestGroupKeys>,
    tilolang: crate::tl_records::Tilolang,
) -> TestHost {
    host_on(
        state_root,
        key,
        cert,
        DeviceKeyGroups { keys, sink: None },
        None,
        None,
        Vec::new(),
        Some(tilolang),
    )
    .await
}

/// [`test_host_holding_mounting`] opened as the device `key`/`cert` acts as, as every device's
/// service is opened: a device's member copies sync from the first workspace it holds, and what
/// a start replays of its store is judged as the device it is, from the moment it opens.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
#[cfg(not(target_arch = "wasm32"))]
pub async fn test_host_holding_mounting_as_member(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    keys: Arc<TestGroupKeys>,
    tilolang: crate::tl_records::Tilolang,
) -> TestHost {
    host_on(
        state_root,
        key,
        cert,
        DeviceKeyGroups { keys, sink: None },
        None,
        Some(Arc::new(MemberDevice {
            cert: cert.clone(),
            signer: Arc::new(key.clone()),
        })),
        Vec::new(),
        Some(tilolang),
    )
    .await
}

/// [`test_host`] whose device takes part in its workspaces' key groups as a device does: through
/// its own key service, which settles each group with the key services of the workspace's other
/// devices. A test that says who holds which keys, or what a device can open, runs on these, not
/// on [`TestGroupKeys`], whose devices share one set of keys.
///
/// # Panics
///
/// The key service or the host cannot open.
#[cfg(feature = "device-keys")]
pub async fn test_host_keyed(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
) -> (TestHost, crate::own_device::KeyService) {
    let signer: Arc<dyn collab_types::signer::DeviceSigner> = Arc::new(key.clone());
    let keys = crate::own_device::KeyService::open(
        "test-groups",
        &state_root.join("groups"),
        key_group::VaultKey::new([5; 32]),
        signer,
        Arc::new(tilo_workspace_keys::NoSealChecks),
    )
    .expect("the device's key service opens");
    let host = host_on(
        state_root,
        key,
        cert,
        keys.groups(),
        None,
        None,
        Vec::new(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await;
    (host, keys)
}

/// `cert`'s device hosting under `state_root`, taking part in the workspaces'
/// channels through `channels`, or in none, keeping their documents in the clear.
async fn host_on(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    channels: impl Into<Option<DeviceKeyGroups>>,
    grants: Option<usize>,
    member: Option<Arc<MemberDevice>>,
    app_files: Vec<std::path::PathBuf>,
    #[cfg(not(target_arch = "wasm32"))] tilolang: Option<crate::tl_records::Tilolang>,
) -> TestHost {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()));
    #[cfg(test)]
    let home = tilo_test_relay::relay().await;
    #[cfg(test)]
    let endpoint = endpoint.relay_mode(iroh::RelayMode::Custom(home.relays()));
    let endpoint = endpoint.bind().await.expect("a loopback endpoint binds");
    #[cfg(test)]
    reached_through(&endpoint, &home).await;
    let host = Arc::new(
        WorkspaceService::open(
            ServiceConfig {
                max_workspace_grants: grants,
                member,
                app_files,
                #[cfg(not(target_arch = "wasm32"))]
                tilolang,
                // Each device knows only the app surfaces named to it: tests run side by side in one
                // process, and a surface one of them names must not change what another's stores
                // admit.
                surfaces: Arc::default(),
                // A server of other people's workspaces takes their work in them.
                ..ServiceConfig::new(
                    state_root.to_owned(),
                    tilo_server_protocol::WorkSetting {
                        from: tilo_server_protocol::WorkFrom::Anyone,
                        ..tilo_server_protocol::WorkSetting::own_person()
                    },
                )
            },
            ServiceIdentity {
                user_public_key: cert.user_public_key,
                device_signing_key: key.clone(),
                device_cert: cert.clone(),
                revocations: RevocationList::new(),
            },
            endpoint.clone(),
            channels.into(),
        )
        .await
        .expect("the host opens"),
    );
    let mut router = Router::builder(endpoint);
    for (alpn, handler) in host.protocols() {
        router = router.accept(alpn, handler);
    }
    TestHost {
        host,
        router: router.spawn(),
        #[cfg(test)]
        _home: home,
    }
}

/// Wait until `endpoint` names `home` as the relay it is reached through.
///
/// A server waits at its opening for its endpoint to name where it is
/// reached ([`tilo_endpoint_reachability::Reaching`]), so no workspace learns
/// an address it must rename, and gives up after iroh's net report timeout. An endpoint that has
/// no relay to reach waits that whole bound out: five seconds for every host a
/// test opens. This package's tests give the fixture's endpoint a server on this
/// machine's loopback instead, as a host on a network has one, and the host
/// opens as soon as its endpoint names it. An embedder's tests reach this
/// fixture without that server, which is a development dependency here, and
/// wait as before.
#[cfg(test)]
async fn reached_through(endpoint: &iroh::Endpoint, home: &tilo_test_relay::Relay) {
    use iroh::Watcher as _;
    tilo_test_env::within("the fixture host's endpoint names its relay", async {
        let mut watch = endpoint.watch_addr();
        let mut addr = watch.get();
        while !addr.relay_urls().any(|url| url == home.url()) {
            let Ok(next) = watch.updated().await else {
                panic!("the endpoint closed before it named its relay");
            };
            addr = next;
        }
    })
    .await;
}

/// Project `workspace`'s members onto the server's own runtime for it, as a
/// settled decision does: what a member's change is decided against on its way
/// in, before the workspace's governance reaches the server by its own path.
///
/// # Panics
///
/// The workspace is not governed here, or its governance does not read.
pub async fn project_access(workspaces: &Workspaces, workspace: &str) {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a governed workspace");
    let (device_access, workspace_id) = {
        let governed = state.lock().await;
        let device_access = governed
            .peer()
            .governance()
            .governed_device_access(tilo_clock::wall())
            .expect("the workspace's device_access");
        (device_access, governed.workspace_id())
    };
    let runtime = workspaces
        .inner
        .host
        .workspaces
        .lock()
        .await
        .get(&workspace_id)
        .cloned()
        .expect("the server serves the workspace");
    runtime.lock().await.device_access = Some(Arc::new(device_access));
}

/// Until when `workspace` restricts `person`, as `host` reads its policy.
///
/// # Panics
///
/// The workspace is not governed here.
pub async fn restricted_until(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    person: [u8; 32],
) -> Option<u64> {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a governed workspace");
    let now = tilo_clock::wall();
    let permissions = state
        .lock()
        .await
        .peer()
        .permissions(host.device_public_key, now)
        .expect("the host's permissions");
    permissions.policy.restricted_until(
        &hex::encode(person),
        &tilo_action_policy::Context::at_time(now),
    )
}

/// A pre-admission of person "Ada" to `workspace` as an editor for `token`,
/// signed by the owner whose root is `[1; 32]`.
///
/// # Panics
///
/// The grant cannot be digested.
#[must_use]
pub fn signed_pre_admission(
    workspace: &str,
    token: &[u8],
) -> tilo_server_protocol::workspaces::PreAdmission {
    signed_pre_admission_as(
        workspace,
        token,
        tilo_server_protocol::workspaces::Role::Editor,
    )
}

/// A pre-admission of person "Ada" to `workspace` with `role` for `token`, signed
/// by the owner whose root is `[1; 32]`.
///
/// # Panics
///
/// The grant cannot be digested.
#[must_use]
pub fn signed_pre_admission_as(
    workspace: &str,
    token: &[u8],
    role: tilo_server_protocol::workspaces::Role,
) -> tilo_server_protocol::workspaces::PreAdmission {
    use ed25519_dalek::Signer as _;
    use sha2::Digest as _;
    let owner = SigningKey::from_bytes(&[1; 32]);
    let mut grant = tilo_server_protocol::workspaces::PreAdmission {
        workspace: workspace.to_owned(),
        role,
        token_sha256: sha2::Sha256::digest(token).into(),
        label: "Ada".into(),
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        owner_user_public_key: owner.verifying_key().to_bytes(),
        signature: [0; 64],
    };
    grant.signature = owner.sign(&grant.digest().unwrap()).to_bytes();
    grant
}

/// One person's device joining `workspace` through its entry, as a Tilo computer
/// does: the entry's invitation, the challenge it answers, then the Complete,
/// which admits it. Answers the Complete it sent and the host's answer.
///
/// # Panics
///
/// The entry does not admit the device.
pub async fn entry_join(
    workspaces: &Workspaces,
    host: &DeviceCert,
    workspace: &str,
    (device, device_cert): (&SigningKey, &DeviceCert),
    seed: u8,
) -> (
    tilo_peer_state::join::JoinTransportRequest,
    tilo_peer_state::join::JoinTransportResponse,
) {
    let role = tilo_server_protocol::workspaces::Role::Editor;
    entry_join_as(
        workspaces,
        host,
        workspace,
        (device, device_cert),
        seed,
        role,
    )
    .await
}

/// One person's device joining `workspace` through its entry with `role`, as a Tilo
/// computer does: the entry's invitation, the challenge it answers, then the
/// Complete, which admits it. Answers the Complete it sent and the host's
/// answer.
///
/// # Panics
///
/// The entry does not admit the device.
pub async fn entry_join_as(
    workspaces: &Workspaces,
    host: &DeviceCert,
    workspace: &str,
    (device, device_cert): (&SigningKey, &DeviceCert),
    seed: u8,
    role: tilo_server_protocol::workspaces::Role,
) -> (
    tilo_peer_state::join::JoinTransportRequest,
    tilo_peer_state::join::JoinTransportResponse,
) {
    use tilo_delivery_protocol::workspace_join::WorkspaceJoinService as _;
    use tilo_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest, JoinTransportResponse,
        sign_join_proof,
    };
    let token = [seed; 8];
    let issued = workspaces
        .pre_admit(
            host,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission_as(workspace, &token, role),
                recipient_user_public_key: Some(device_cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let service = workspaces.join_service();
    let endpoint = device_cert.device_public_key;
    let request_id = [seed.wrapping_add(100); 32];
    let JoinTransportResponse::Challenge(answer) = service
        .handle(
            endpoint,
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: issued.signed_invitation.clone(),
                device_cert: device_cert.clone(),
                request_id,
            }),
        )
        .await
    else {
        panic!("no challenge");
    };
    // The device signs its own admission, as a joining device does.
    let redemption = tilo_peer_state::join::verify_connection_redemption(
        &issued.signed_invitation,
        device_cert,
        request_id,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let admission = tilo_peer_state::own_admission::own_admission(
        workspace,
        &answer.governance,
        &redemption,
        None,
        device,
        tilo_clock::wall(),
    )
    .unwrap();
    let proof = sign_join_proof(answer.challenge, device).unwrap();
    let complete = JoinTransportRequest::Complete(JoinCompleteRequest { proof, admission });
    let response = service.handle(endpoint, complete.clone()).await;
    drop(service);
    (complete, response)
}

impl Workspaces {
    /// `cert`'s device asking for `entry` on its session, as a device does: where the host
    /// answers that it may come in once it signs its own admission, it signs it with `key`
    /// ([`tilo_peer_state::own_admission::sign_entry`]) and asks again. The host's last answer.
    ///
    /// # Panics
    ///
    /// The device cannot sign what the host answered.
    pub async fn enter_signed(
        &self,
        (key, cert): (&SigningKey, &DeviceCert),
        entry: tilo_server_protocol::entering::Entry,
    ) -> tilo_server_protocol::entering::Entered {
        use crate::handshake::Admit;
        use tilo_server_protocol::entering::{Entered, Through};
        let answered = Admit::enter(self, cert, entry.clone()).await;
        let Entered::Sign {
            governance,
            through,
        } = answered
        else {
            return answered;
        };
        let invitation = match &through {
            Through::Invitation { invitation, .. } => Some(invitation.as_slice()),
            Through::Entry => None,
        };
        let entering = &entry.entering;
        let admission = tilo_peer_state::own_admission::sign_entry(
            &entering.workspace,
            &governance,
            (invitation, entering.request_id),
            Some(entering.name.as_str()),
            (cert, key),
            tilo_clock::wall(),
        )
        .await
        .unwrap();
        let mut signed = entry;
        signed.entering.through = through;
        signed.entering.admission = admission;
        Admit::enter(self, cert, signed).await
    }

    /// Admit the device `cert` certifies with `invitation`, under the request
    /// `request_id`, as a device entering on its session is: it signs its own
    /// admission with `key` on the workspace's member list, named `name`
    /// ([`tilo_peer_state::own_admission`]), and the workspace takes it in,
    /// with the owner's pre-admission `token` where the invitation was issued
    /// under one. The caller vouches that the device proved it holds its key
    /// on the connection that carries the redemption.
    ///
    /// # Panics
    ///
    /// The invitation names no workspace held here, or does not admit the
    /// device.
    pub async fn admit_proven(
        &self,
        invitation: Vec<u8>,
        (cert, key): (DeviceCert, &SigningKey),
        request_id: [u8; 32],
        name: &str,
        token: Option<&[u8]>,
    ) -> tilo_server_protocol::workspaces::JoinReply {
        // A device that cannot sign itself in under the invitation is refused
        // for the reason every receiver would refuse it: it sends nothing.
        let admission = match self
            .own_admission_of(&invitation, (&cert, key), request_id, name)
            .await
        {
            Ok(admission) => admission,
            Err(refused) => return tilo_server_protocol::workspaces::JoinReply::Refused(refused),
        };
        self.redeem_on_connection(
            super::pages::Presented {
                invitation,
                cert,
                request_id,
            },
            admission,
            name,
            token,
        )
        .await
    }

    /// The admission `key`'s device, `cert`'s, signs itself for `invitation`
    /// on the member list the host holds now: what a joining device sends
    /// with its redemption.
    ///
    /// # Panics
    ///
    /// The invitation names no workspace held here.
    ///
    /// # Errors
    ///
    /// The device could not sign itself in under the invitation, as every
    /// receiver would refuse it.
    pub async fn own_admission_of(
        &self,
        invitation: &[u8],
        (cert, key): (&DeviceCert, &SigningKey),
        request_id: [u8; 32],
        name: &str,
    ) -> Result<Vec<Vec<u8>>, String> {
        use tilo_peer_state::join_log::GovernanceMessages as _;
        let now = tilo_clock::unix_now_ns();
        let workspace = tilo_peer_state::join_log::invitation_workspace(invitation, now)
            .unwrap()
            .to_string();
        let governance = self
            .workspace(&workspace)
            .await
            .unwrap()
            .lock()
            .await
            .peer()
            .governance_messages()
            .unwrap();
        let redemption = tilo_peer_state::join::verify_connection_redemption(
            invitation,
            cert,
            request_id,
            &tilo_user_identity::RevocationList::new(),
            now,
        )?;
        tilo_peer_state::own_admission::own_admission(
            &workspace,
            &governance,
            &redemption,
            Some(name),
            key,
            tilo_clock::wall(),
        )
    }
}

/// Record `grants` in `workspace`'s governance as its host, then project the access the host
/// decides its changes against: what a person's Settings writes there, without a client.
///
/// # Panics
///
/// The workspace is not governed here, or its governance did not take them.
pub async fn record_grants(
    workspaces: &Workspaces,
    workspace: &str,
    grants: &[tilo_action_policy::Grant],
) {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a governed workspace");
    {
        let governed = state.lock().await;
        let key = workspaces.inner.host.key.clone();
        governed
            .peer()
            .governance_mut()
            .put_governed_grants(grants, &key, tilo_clock::wall())
            .expect("the host records its workspace's grants");
    }
    project_access(workspaces, workspace).await;
}

/// Take the grants `ids` name back from `workspace`'s governance as its host, then project the
/// access the host decides its changes against.
///
/// # Panics
///
/// The workspace is not governed here, or its governance did not take the change.
pub async fn revoke_grants(
    workspaces: &Workspaces,
    workspace: &str,
    ids: &[tilo_action_policy::GrantId],
) {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a governed workspace");
    {
        let governed = state.lock().await;
        let key = workspaces.inner.host.key.clone();
        governed
            .peer()
            .governance_mut()
            .remove_governed_grants(ids, &key, tilo_clock::wall())
            .expect("the host takes its workspace's grants back");
    }
    project_access(workspaces, workspace).await;
}

/// `serving` creates the workspace `name` and invites `device` into it: the serving device, the
/// device it invites, the workspace and the invitation's text.
pub async fn invited(
    serving: TestHost,
    serving_cert: &DeviceCert,
    name: &str,
    seed: u8,
    device: (SigningKey, DeviceCert),
) -> (TestHost, (SigningKey, DeviceCert), WorkspaceSummary, String) {
    let created = serving
        .workspaces()
        .create(name)
        .await
        .expect("a workspace is created");
    let invitation = invitation(&serving, serving_cert, &created.workspace, seed, &device.1).await;
    (serving, device, created, invitation)
}

/// The text of the invitation `serving` issues for `recipient`'s person into `workspace`.
pub async fn invitation(
    serving: &TestHost,
    serving_cert: &DeviceCert,
    workspace: &str,
    seed: u8,
    recipient: &DeviceCert,
) -> String {
    let issued = serving
        .workspaces()
        .pre_admit(
            serving_cert,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(workspace, &[seed; 8]),
                recipient_user_public_key: Some(recipient.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .expect("the serving device issues an invitation");
    invitation_text(&issued.signed_invitation)
}

/// The text `signed_invitation` is handed to its person in: what a device redeems with
/// [`Workspaces::join_member`].
#[must_use]
pub fn invitation_text(signed_invitation: &[u8]) -> String {
    tilo_peer_state::join::encode_invitation_text(signed_invitation)
}

/// The membership a replica holding nothing but `workspace`'s membership log, as `workspaces`
/// sends it to a peer, replays: none of the workspace's records and none of its keys.
///
/// # Panics
/// `workspaces` does not hold `workspace`.
pub async fn membership_log_alone(
    workspaces: &Workspaces,
    workspace: &str,
) -> tilo_action_policy::WorkspacePolicy {
    let state = workspaces.workspace(workspace).await.unwrap();
    let message = state.lock().await.peer().workspace_message();
    let neighbour_wire::NeighbourMsg::SyncResp { changes, .. } = message else {
        panic!("a workspace sends its log whole: {message:?}");
    };
    let log = tilo_peer_state::workspace::candidate(None, &changes).unwrap();
    tilo_peer_state::members::replay(&log, workspace)
}

/// The runs a test's broker was asked to start, each of which ran to its end at once.
#[cfg(feature = "agents")]
#[derive(Default)]
pub struct Started(pub Mutex<Vec<crate::apps::triggers::Firing>>);

#[cfg(feature = "agents")]
impl crate::apps::triggers::StartRun for Started {
    fn start(
        &self,
        firing: &crate::apps::triggers::Firing,
        _: tilo_automation::AutomationInvocationId,
    ) -> Result<u64, tilo_actions::remote::ActionCallError> {
        let mut started = self.0.lock().unwrap();
        started.push(firing.clone());
        Ok(u64::try_from(started.len()).unwrap())
    }

    fn ended(
        &self,
        firing: &crate::apps::triggers::Firing,
        run: tilo_automation::AutomationInvocationId,
    ) -> Option<tilo_automation::InvocationReport> {
        Some(tilo_automation::InvocationReport {
            invocation: run,
            shortcut: firing.shortcut.clone(),
            version: None,
            trigger: firing.trigger.kind().into(),
            mode: tilo_automation::InvocationMode::Unattended,
            steps: Vec::new(),
            output: None,
            degraded: Vec::new(),
            outcome: tilo_automation::InvocationOutcome::Completed,
        })
    }
}

/// Save `shortcut` in `workspace`, signed by the device `cert` names.
pub async fn save_signed(
    workspaces: &Workspaces,
    workspace: &str,
    mut shortcut: tilo_automation::Shortcut,
    (key, cert): (&SigningKey, &DeviceCert),
) {
    tilo_peer_state::workspace::sign_saved_shortcut(workspace, &mut shortcut, cert, key).unwrap();
    workspaces
        .commit_own_membership(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Settings(
                tilo_peer_state::workspace_membership::SettingsChange::Shortcuts(
                    tilo_automation::ShortcutChange::Save(Box::new(shortcut)),
                ),
            ),
            None,
        )
        .await
        .unwrap();
}

/// A shortcut whose trigger is a change of an issue in the tasks app.
pub fn on_issue_change() -> tilo_automation::Shortcut {
    serde_json::from_value(serde_json::json!({
        "id": "tidy",
        "name": "Tidy",
        "steps": [],
        "triggers": [{
            "trigger": "resource_changed",
            "kind": "tl.so.tilo.tasks.Task",
            "actions": [],
            "fields": [],
        }],
    }))
    .unwrap()
}

/// Whoever's turns an agent takes here: the agent `scout`, as a device that holds its key.
pub struct ActsAsScout;

impl crate::work::AgentEngine for ActsAsScout {
    fn acts_as<'a>(
        &'a self,
        _workspace: &'a str,
        agent: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { agent == "scout" })
    }
}

/// Link `key`'s device, `cert`'s, to `host`'s workspace `workspace` from a copy of the workspace
/// whose member list does not list it, as a member's further device reaches it: the host admits
/// the dial to sign the device's own row and nothing else, ends that link once the row is in, and
/// the device dials again and catches up as the member device it now is. Answers the link that
/// runs, which ends with the returned handle.
///
/// # Panics
///
/// The host refused either dial, or the device never caught up.
#[cfg(test)]
pub async fn list_itself_on_link(
    host: &TestHost,
    workspace: &str,
    (key, cert): (&SigningKey, &DeviceCert),
) -> tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>> {
    let home = tilo_test_relay::relay().await;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Custom(home.relays()))
        .bind()
        .await
        .unwrap();
    let workspaces = host.workspaces();
    let host_device = workspaces.host_device();
    let host_cert = workspaces.device_cert().clone();
    let dialer = Arc::new(tilo_server_client::link::Dialer::from_endpoint(
        tilo_server_client::ServerAddress::at(host.node_addr()),
        endpoint,
    ));
    let copy = workspaces
        .state(workspace)
        .await
        .expect("the host holds the workspace");
    let link =
        tilo_peer_state::direct::DirectSync::new(copy, key.clone()).with_certificate(cert.clone());
    let dial = || async {
        let transport = dialer
            .link(tilo_server_client::link::Purpose::Records)
            .await
            .expect("the host answers a records link");
        transport
            .prove_member(cert, key, &host_cert)
            .await
            .expect("the host proves the device");
        link.dial_over(Arc::new(transport), host_device).await
    };
    let listing_itself = dial()
        .await
        .expect("the host admits the device to list itself");
    let ended = tilo_test_env::within(
        "the host ends the link once the device's own row is in",
        listing_itself.run(&link),
    )
    .await;
    assert_eq!(
        ended,
        Err(tilo_server_protocol::refusal::Refusal::Listed),
        "the link the device listed itself on ends, telling it so"
    );
    let connected = dial()
        .await
        .expect("the host admits the listed device as a member device");
    let mut caught_up = link.caught_up();
    let running = {
        let link = link.clone();
        let dialer = Arc::clone(&dialer);
        tokio::spawn(async move {
            let ran = connected.run(&link).await;
            drop(dialer);
            ran
        })
    };
    tilo_test_env::within("the listed device catches up with the host", async {
        caught_up.wait_for(|caught| *caught).await.unwrap();
    })
    .await;
    running
}
