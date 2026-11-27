//! A hosting device and the devices around it, for the tests of workspaces and of
//! the app action servers embedders build on them.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::unreachable,
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
use tilo_actions::remote::ActionProvider as _;
use tilo_server_protocol::WorkspaceId;
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

/// Another device of the person whose root (`[1; 32]`) certified the fixture's hosting
/// device `cert`, admitted to `workspace`.
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

/// A device certified only to keep workspaces: it hosts them without taking part in
/// their channels.
#[must_use]
pub fn hosting_device() -> (SigningKey, DeviceCert) {
    hosting_device_with(DeviceCapabilities::member())
}

/// A holder's device of the person whose root is `[1; 32]`, certified with
/// `capabilities`.
///
/// # Panics
///
/// The certificate cannot be signed.
#[must_use]
pub fn hosting_device_with(capabilities: DeviceCapabilities) -> (SigningKey, DeviceCert) {
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
/// `capabilities`: the key it signs with and its certificate. An agent's grant
/// always ends, so a certificate carrying one is valid for the hour around now.
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
    let (issued_at_ns, expires_at_ns) = if capabilities.agent.is_some() && !capabilities.server_key
    {
        let now = tilo_clock::unix_now_ns();
        (
            now.saturating_sub(1_800_000_000_000),
            Some(now.saturating_add(1_800_000_000_000)),
        )
    } else {
        (1, None)
    };
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns,
        expires_at_ns,
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
    /// Whether it is away ([`Self::away`]): it answers no new connection.
    away: Arc<std::sync::atomic::AtomicBool>,
    /// The relay its endpoint took as its home ([`reached_through`]), closed
    /// after the endpoint that reaches it.
    #[cfg(test)]
    _home: tilo_test_relay::Relay,
}

impl TestHost {
    /// The workspaces it hosts.
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

    /// Answer no new connection until [`Self::back`], as a device that is off or off the network:
    /// whoever dials it is not answered. What is linked already stays.
    pub fn away(&self) {
        self.away.store(true, std::sync::atomic::Ordering::Release);
    }

    /// Answer connections again after [`Self::away`].
    pub fn back(&self) {
        self.away.store(false, std::sync::atomic::Ordering::Release);
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
    /// Whether opening a group waits until it is let go ([`Self::hold_openings`]), and how many
    /// openings wait now.
    openings: tokio::sync::watch::Sender<(bool, usize)>,
}

impl Default for TestGroupKeys {
    fn default() -> Self {
        Self {
            held: Mutex::default(),
            admitted_only: std::sync::atomic::AtomicBool::new(false),
            handed: tokio::sync::watch::Sender::new(0),
            behind: Mutex::default(),
            paused: std::sync::atomic::AtomicBool::new(false),
            openings: tokio::sync::watch::Sender::new((false, 0)),
        }
    }
}

impl TestGroupKeys {
    /// Forget every group's keys, and hold every opening of a group back until
    /// [`Self::let_openings_go`]: an ask that opens a group waits as the key service's does while
    /// it founds a group or a group lets the device in, which takes seconds.
    pub fn hold_openings(&self) {
        self.held.lock().expect("the fixture's keys").clear();
        self.openings.send_modify(|(waiting, _)| *waiting = true);
    }

    /// Let every opening [`Self::hold_openings`] kept back go on.
    pub fn let_openings_go(&self) {
        self.openings.send_modify(|(waiting, _)| *waiting = false);
    }

    /// Wait until an opening of a group waits to be let go.
    pub async fn opening_waits(&self) {
        let mut openings = self.openings.subscribe();
        openings
            .wait_for(|(_, waiting)| *waiting > 0)
            .await
            .expect("the fixture's openings");
    }

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
            if self.openings.borrow().0 {
                let mut openings = self.openings.subscribe();
                self.openings
                    .send_modify(|(_, waiting)| *waiting = waiting.saturating_add(1));
                let _ = openings.wait_for(|(waiting, _)| !*waiting).await;
                self.openings
                    .send_modify(|(_, waiting)| *waiting = waiting.saturating_sub(1));
                if let Some(keys) = self.held.lock().expect("the fixture's keys").get(workspace) {
                    return Some(Ok(keys.clone()));
                }
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
/// taking part in its workspaces' channels as every server that hosts
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

/// [`test_host`] whose workspaces open with `registers` on them, as an embedder's that runs
/// the crates built on the workspaces does.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host_with_registers(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    registers: Vec<crate::workspaces::Register>,
) -> TestHost {
    host_on_with(
        state_root,
        (key, cert),
        registers,
        DeviceKeyGroups {
            keys: Arc::<TestGroupKeys>::default(),
            sink: None,
        },
        None,
        None,
        Vec::new(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
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
    let kind = declare_record_kind(workspaces, app);
    workspaces
        .author_record(workspace, (app, &kind, record), edit)
        .await
}

/// Declare [`record_kind`] of `app` on `workspaces`, a record whose fields the tests write: what
/// running the app's package does. Answers the kind.
pub fn declare_record_kind(workspaces: &Workspaces, app: &str) -> String {
    let kind = record_kind(app);
    declare_record_kind_named(workspaces, app, &kind);
    kind
}

/// Declare `app`'s kind [`names_content`] writes, on `workspaces`: a record whose `blob` field
/// names the content it keeps, as an app's typed write of a file leaves it. Answers the kind.
pub fn declare_naming_kind(workspaces: &Workspaces, app: &str) -> String {
    let kind = format!("tl.{app}.Kept");
    let surface = collab_types::surface::canonical(&serde_json::json!({
        "format": 2,
        "resources": [{
            "kind": kind,
            "label": "Kept",
            "version": 1,
            "fields": [
                {"path": "name", "type": "text"},
                {"path": "blob", "type": "text"},
            ],
            "verbs": [{"id": "keep", "kind": "undoable"}],
        }],
    }))
    .into_bytes();
    workspaces.declare_package_surface(app, &surface);
    kind
}

/// A store write of `app`'s resource `id` of `kind`, reading `field` as naming the content at
/// `hash` under the reader set `readers` ([`collab_types::EVERY_MEMBER`], or the people it
/// names): what an app's typed write of a file leaves in the workspace's store, and so in its
/// index of what each resource names.
pub fn names_content(
    kind: &str,
    id: &str,
    field: &str,
    hash: &str,
    readers: &str,
    invoker: &str,
) -> collab_types::store::wire::Command {
    use collab_types::store::{Cell, Resource, Value};
    let mut fields = BTreeMap::new();
    fields.insert(
        field.to_owned(),
        Cell::One(Value::from(format!("{}{hash}", tilo_content::BLOB_SCHEME))),
    );
    collab_types::store::wire::Command::Put {
        kind: kind.to_owned(),
        id: id.to_owned(),
        resource: Resource {
            readers: readers.to_owned(),
            parent: None,
            order: None,
            fields,
        },
        stamp: tilo_resource_store::Stamp {
            verb: "keep".to_owned(),
            invoker: invoker.to_owned(),
            invocation: format!("keep-{id}"),
            at: "2026-11-27T12:00:00Z".to_owned(),
            ..tilo_resource_store::Stamp::default()
        },
    }
}

/// `command`, a store write of `app`, drafted and kept as this device's own: what an app's typed
/// write leaves in the workspace's store.
///
/// # Errors
/// The workspace is not held here, or its store refused the write.
pub async fn write_store(
    workspaces: &Workspaces,
    workspace: &str,
    app: &str,
    command: collab_types::store::wire::Command,
) -> Result<(), String> {
    let state = workspaces
        .workspace(workspace)
        .await
        .map_err(|error| error.to_string())?;
    super::wanted_keys::write_with_keys(&state, move |mut hosted| {
        let command = command.clone();
        async move {
            let writes =
                tilo_resource_store::wire::draft(&mut workspaces.store_of(&mut hosted), command)
                    .map_err(|refused| refused.error.to_string())?;
            workspaces
                .keep_writes(hosted, workspaces.signing_device(), app, writes, None)
                .await
                .map(|_| ())
        }
    })
    .await
}

/// Declare `kind`, a record of `app` whose fields the tests write, on `workspaces`.
pub fn declare_record_kind_named(workspaces: &Workspaces, app: &str, kind: &str) {
    let surface = record_surface(kind);
    workspaces.declare_package_surface(app, surface.as_bytes());
}

/// Change `app`'s record `record` of `workspace` with `edit`, its kind `kind` declared on
/// `workspaces` first.
///
/// # Errors
/// The workspace is not kept here, or its store refused the write.
pub async fn write_record_of_kind(
    workspaces: &Workspaces,
    workspace: &str,
    (app, kind): (&str, &str),
    record: &str,
    edit: impl FnOnce(&mut automerge::AutoCommit) -> Result<(), String>,
) -> Result<(), String> {
    declare_record_kind_named(workspaces, app, kind);
    workspaces
        .author_record(workspace, (app, kind, record), edit)
        .await
}

/// The compiled surface of `kind`, a record whose fields the tests write anything into.
fn record_surface(kind: &str) -> String {
    let fields: Vec<serde_json::Value> = [
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
        "value",
    ]
    .iter()
    .map(|field| serde_json::json!({ "path": field, "type": "any" }))
    .collect();
    collab_types::surface::canonical(&serde_json::json!({
        "format": 2,
        "resources": [{
            "kind": kind,
            "label": "Record",
            "version": 1,
            "fields": fields,
            "verbs": [{ "id": "type", "kind": "undoable" }],
        }],
    }))
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
    let installed = tilo_peer_state::InstalledApp {
        release,
        installed_by: hex::encode(workspaces.signing_device().cert.user_public_key),
        installed_at_ns: tilo_clock::unix_now_ns(),
    };
    workspaces
        .write_app_records(workspace, vec![(app.to_owned(), Some(installed))])
        .await
        .map(drop)
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
        .write_app_records(workspace, vec![(app.to_owned(), None)])
        .await
        .map(drop)
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
    test_host_holding_with_registers(state_root, key, cert, keys, Vec::new()).await
}

/// [`test_host_holding`] whose workspaces open with `registers` on them, as an embedder's that
/// runs the crates built on the workspaces does.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host_holding_with_registers(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    keys: Arc<TestGroupKeys>,
    registers: Vec<crate::workspaces::Register>,
) -> TestHost {
    host_on_with(
        state_root,
        (key, cert),
        registers,
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
    test_host_keyed_with_registers(state_root, key, cert, Vec::new()).await
}

/// [`test_host_keyed`] whose workspaces open with `registers` on them, as an embedder's that
/// runs the crates built on the workspaces does.
///
/// # Panics
///
/// The key service or the host cannot open.
#[cfg(feature = "device-keys")]
pub async fn test_host_keyed_with_registers(
    state_root: &Path,
    key: &SigningKey,
    cert: &DeviceCert,
    registers: Vec<crate::workspaces::Register>,
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
    let host = host_on_with(
        state_root,
        (key, cert),
        registers,
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
    host_on_with(
        state_root,
        (key, cert),
        Vec::new(),
        channels,
        grants,
        member,
        app_files,
        #[cfg(not(target_arch = "wasm32"))]
        tilolang,
    )
    .await
}

/// [`host_on`], with the crates built on the workspaces registering on them as they open
/// ([`crate::service::ServiceConfig::registers`]).
async fn host_on_with(
    state_root: &Path,
    (key, cert): (&SigningKey, &DeviceCert),
    registers: Vec<crate::workspaces::Register>,
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
    let config = ServiceConfig {
        max_workspace_grants: grants,
        member,
        app_files,
        registers,
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
    };
    let host = Arc::new(
        WorkspaceService::open(
            config,
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
    let away = Arc::new(std::sync::atomic::AtomicBool::new(false));
    for (alpn, handler) in host.protocols() {
        router = router.accept(
            alpn,
            Unless {
                away: Arc::clone(&away),
                handler,
            },
        );
    }
    TestHost {
        host,
        router: router.spawn(),
        away,
        #[cfg(test)]
        _home: home,
    }
}

/// A protocol of a [`TestHost`], answered unless the host is away ([`TestHost::away`]).
struct Unless {
    away: Arc<std::sync::atomic::AtomicBool>,
    handler: Box<dyn iroh::protocol::DynProtocolHandler>,
}

impl std::fmt::Debug for Unless {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Unless").finish_non_exhaustive()
    }
}

impl iroh::protocol::ProtocolHandler for Unless {
    async fn accept(
        &self,
        connection: iroh::endpoint::Connection,
    ) -> Result<(), iroh::protocol::AcceptError> {
        if self.away.load(std::sync::atomic::Ordering::Acquire) {
            connection.close(0u32.into(), b"away");
            return Ok(());
        }
        self.handler.accept(connection).await
    }

    async fn shutdown(&self) {
        self.handler.shutdown().await;
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
/// The workspace is not hosted here, or its governance does not read.
pub async fn project_access(workspaces: &Workspaces, workspace: &str) {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a hosted workspace");
    let (device_access, workspace_id) = {
        let hosted = state.lock().await;
        let device_access = hosted
            .peer()
            .governance()
            .member_device_access(tilo_clock::wall())
            .expect("the workspace's device_access");
        (device_access, hosted.workspace_id())
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
/// The workspace is not hosted here.
pub async fn restricted_until(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    person: [u8; 32],
) -> Option<u64> {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a hosted workspace");
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
        self.enter_signed_by((key, cert), entry).await.unwrap()
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
/// The workspace is not hosted here, or its governance did not take them.
pub async fn record_grants(
    workspaces: &Workspaces,
    workspace: &str,
    grants: &[tilo_action_policy::Grant],
) {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a hosted workspace");
    {
        let hosted = state.lock().await;
        let key = workspaces.inner.host.key.clone();
        hosted
            .peer()
            .governance_mut()
            .put_member_grants(grants, &key, tilo_clock::wall())
            .expect("the host records its workspace's grants");
    }
    project_access(workspaces, workspace).await;
}

/// Take the grants `ids` name back from `workspace`'s governance as its host, then project the
/// access the host decides its changes against.
///
/// # Panics
///
/// The workspace is not hosted here, or its governance did not take the change.
pub async fn revoke_grants(
    workspaces: &Workspaces,
    workspace: &str,
    ids: &[tilo_action_policy::GrantId],
) {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("a hosted workspace");
    {
        let hosted = state.lock().await;
        let key = workspaces.inner.host.key.clone();
        hosted
            .peer()
            .governance_mut()
            .remove_member_grants(ids, &key, tilo_clock::wall())
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
    let owner = SigningKey::from_bytes(&[1; 32]);
    invitation_by(&owner, serving, serving_cert, workspace, seed, recipient).await
}

/// [`invitation`] into a workspace `owner` owns, who grants it.
pub async fn invitation_by(
    owner: &SigningKey,
    serving: &TestHost,
    serving_cert: &DeviceCert,
    workspace: &str,
    seed: u8,
    recipient: &DeviceCert,
) -> String {
    use ed25519_dalek::Signer as _;
    let mut grant = signed_pre_admission(workspace, &[seed; 8]);
    grant.owner_user_public_key = owner.verifying_key().to_bytes();
    grant.signature = owner.sign(&grant.digest().unwrap()).to_bytes();
    let issued = serving
        .workspaces()
        .pre_admit(
            serving_cert,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant,
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

/// Whoever's turns an agent engine takes here: the agents a test's device holds keys for, or
/// every agent's, or none.
pub enum ActsAs {
    /// Every agent of every workspace the device runs.
    EveryAgent,
    /// No agent's: this device takes no turn.
    NoAgent,
    /// Only the agents named.
    Only(&'static [&'static str]),
}

impl crate::work::AgentEngine for ActsAs {
    fn acts_as<'a>(
        &'a self,
        _workspace: &'a str,
        agent: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            match self {
                Self::EveryAgent => true,
                Self::NoAgent => false,
                Self::Only(agents) => agents.contains(&agent),
            }
        })
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

/// A device serving workspace `name`, the device it invited, and the
/// invitation that device redeems.
pub async fn shared(
    serving_state: &std::path::Path,
    name: &str,
    seed: u8,
) -> (TestHost, (SigningKey, DeviceCert), WorkspaceSummary, String) {
    shared_with(serving_state, name, seed, laptop(seed)).await
}

thread_local! {
    /// The keys of the workspaces' every-member groups, as the devices of one test hold them: a
    /// group hands a device it admits the keys its other devices seal with. Each device holding
    /// keys of its own would seal what the others cannot open.
    static GROUP_KEYS: Arc<TestGroupKeys> = Arc::default();
}

/// The group keys the devices of this test share.
pub fn group_keys() -> Arc<TestGroupKeys> {
    GROUP_KEYS.with(Arc::clone)
}

/// The laptop of the person of `seed`.
pub fn laptop(seed: u8) -> (SigningKey, DeviceCert) {
    device_cert(
        &SigningKey::from_bytes(&[seed; 32]),
        "laptop",
        DeviceCapabilities::member(),
    )
}

/// [`shared`], inviting `device`.
pub async fn shared_with(
    serving_state: &std::path::Path,
    name: &str,
    seed: u8,
    device: (SigningKey, DeviceCert),
) -> (TestHost, (SigningKey, DeviceCert), WorkspaceSummary, String) {
    let (serving_key, serving_cert) = hosting_device();
    let serving = test_host_holding(serving_state, &serving_key, &serving_cert, group_keys()).await;
    invited(serving, &serving_cert, name, seed, device).await
}

/// A service on `state`, serving the member copies of `(key, cert)` from the moment it opens.
pub async fn member_service(
    state: &std::path::Path,
    (key, cert): (SigningKey, DeviceCert),
) -> TestHost {
    test_host_holding_as_member(state, &key, &cert, group_keys()).await
}

/// What a pending answer comes to, on this runtime.
pub async fn settled<T: Send + 'static>(pending: tilo_actions::remote::Pending<T>) -> T {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    pending.then(move |value| {
        let _ = sender.send(value);
    });
    tilo_test_env::within("the governance answer", receiver)
        .await
        .unwrap()
        .expect("answered")
}

/// A new person admitted to `workspace` as an Editor, through the owner's
/// pre-admission: their device's certificate.
pub async fn admit(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    seed: u8,
) -> DeviceCert {
    admit_device(workspaces, workspace, owner, seed).await.1
}

/// [`admit`], with the admitted device's key.
pub async fn admit_device(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    seed: u8,
) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let token = [seed; 8];
    let issued = workspaces
        .pre_admit(
            owner,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(workspace, &token),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let (key, cert) = guest_device();
    let reply = workspaces
        .admit_proven(
            issued.signed_invitation,
            (cert.clone(), &key),
            rand::random(),
            &format!("Person {seed}"),
            Some(token.as_slice()),
        )
        .await;
    assert!(
        matches!(
            reply,
            tilo_server_protocol::workspaces::JoinReply::Admitted { .. }
        ),
        "{reply:?}"
    );
    ADMITTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((cert.clone(), key.clone()));
    (key, cert)
}

/// Every device [`admit_device`] admitted, with its key: what a member's device signs its calls
/// with.
static ADMITTED: std::sync::Mutex<Vec<(DeviceCert, ed25519_dalek::SigningKey)>> =
    std::sync::Mutex::new(Vec::new());

/// The key of `device`, when [`admit_device`] admitted it.
pub fn admitted_key(device: &DeviceCert) -> Option<ed25519_dalek::SigningKey> {
    ADMITTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(cert, _)| cert.device_public_key == device.device_public_key)
        .map(|(_, key)| key.clone())
}

/// A device of `person` (their root key, as hex) that [`admit_device`] admitted, with its key.
pub fn admitted_device_of(person: &str) -> Option<(DeviceCert, ed25519_dalek::SigningKey)> {
    ADMITTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(cert, _)| hex::encode(cert.user_public_key) == person)
        .cloned()
}

/// The person whose device this is lives in `zone`: their own workspace is founded, and the
/// zone is the one they chose in their region, as their page chooses it.
pub async fn lives_in(workspaces: &Workspaces, zone: &str) {
    crate::workspaces::person_workspace::found(workspaces)
        .await
        .expect("the person's own workspace");
    let person = workspaces.device_cert().user_public_key;
    workspaces
        .change_region(
            &person,
            crate::workspaces::region::Region::CHOOSE_ZONE.with(zone.to_owned()),
        )
        .await
        .expect("the person chooses their zone");
}

/// Each agent the workspace's record names, by id: its name and whether it still works.
pub async fn agents_listed(
    workspaces: &Workspaces,
    workspace: &str,
) -> Vec<(String, String, bool)> {
    workspaces
        .read_resource::<tilo_workspace_agent::WorkspaceAgents>(
            workspace,
            (
                tilo_workspace_agent::OWNER,
                &collab_types::workspace_document_id(tilo_workspace_agent::KIND),
            ),
        )
        .await
        .expect("the record reads")
        .map(|state| {
            state
                .agents
                .iter()
                .map(|agent| (agent.id.0.clone(), agent.name.clone(), agent.is_active()))
                .collect()
        })
        .unwrap_or_default()
}

/// `operation` of `workspace`'s agents with `arguments`, asked by the device `device` over this
/// device's dispatch, as every caller asks it: what it answered, or why not.
pub async fn ask_agents(
    workspaces: &Workspaces,
    workspace: &str,
    device: &DeviceCert,
    (operation, arguments): (&str, serde_json::Value),
) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
    use tilo_actions::remote::ActionProvider as _;
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    let request = tilo_actions::remote::InvokeRequest {
        asks: false,
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget::workspace_governance(
            workspace, operation,
        ),
        argument: tilo_actions::remote::InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let caller = crate::service::local_caller(device).in_workspace(workspace);
    let receipt = settled(workspaces.device_actions().invoke(&caller, request)).await?;
    match receipt.status {
        tilo_actions::remote::ReceiptStatus::Completed { output } => {
            Ok(output.value.unwrap_or(serde_json::Value::Null))
        }
        tilo_actions::remote::ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("an agent's change completes at once: {other:?}"),
    }
}

/// The agents' operations the catalogue offers `device` in `workspace`.
pub async fn offered_agents(
    workspaces: &Workspaces,
    workspace: &str,
    device: &DeviceCert,
) -> Vec<String> {
    let query = tilo_actions::remote::CatalogueQuery {
        provider: Some(tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.into()),
        parent_resource: Some(tilo_actions::remote::workspace_governance_resource(
            workspace,
        )),
        max_results: 100,
        ..tilo_actions::remote::CatalogueQuery::default()
    };
    let page = workspaces
        .catalogue(
            tilo_server_protocol::WorkspaceId::parse(workspace),
            &crate::service::local_caller(device),
            Some(device),
            &query,
        )
        .await;
    let mut offered: Vec<String> = page
        .rows
        .into_iter()
        .filter_map(|row| row.action)
        .filter_map(|action| {
            tilo_actions::remote::workspace_governance_operation(&action.address)
                .map(|(_, granted)| granted.to_owned())
        })
        .filter(|operation| super::actions::is_agents_operation(operation))
        .collect();
    offered.sort();
    offered
}

/// A new workspace of `workspaces`, its governance served as actions: its name.
pub async fn hosted(workspaces: &Workspaces) -> String {
    let summary = needed(workspaces.create("Team").await, "a new workspace");
    if workspaces.inner.jobs.governance().is_none() {
        super::ServiceWorkspaceActions::new().attach(workspaces);
    }
    summary.workspace
}

/// The agent `scribe` of the person both devices are of, admitted to `workspace` with its own
/// key and let run the bridge's actions there.
pub async fn admit_scribe(workspaces: &Workspaces, workspace: &str) -> (SigningKey, DeviceCert) {
    let root = persons_root();
    let (key, mut cert) = device_cert(&root, "Agent: Scribe", DeviceCapabilities::nothing());
    if tilo_user_identity::sign_device_cert(&mut cert, &root).is_err() {
        unreachable!("a fixture's agent certificate signs");
    }
    let now = tilo_clock::unix_now_ns();
    let Some(workspace_id) = tilo_server_protocol::WorkspaceId::parse(workspace) else {
        unreachable!("the workspace the devices share parses as an id")
    };
    let Ok(delegation) = tilo_server_protocol::delegation::Delegation::sign(
        &root,
        cert.device_public_key,
        tilo_server_protocol::delegation::Delegate::Agent {
            agent_id: "scribe".into(),
        },
        "Scribe",
        [10, 20, 30],
        tilo_server_protocol::delegation::DelegateScope {
            workspaces: vec![workspace_id],
            apps: Vec::new(),
        },
        now.saturating_sub(1_000_000_000),
        now.saturating_add(3_600_000_000_000),
    ) else {
        unreachable!("a fixture's agent delegation signs")
    };
    {
        let Ok(state) = workspaces.workspace(workspace).await else {
            unreachable!("the workspace the devices share is held")
        };
        let hosted = state.lock().await;
        if hosted
            .peer()
            .governance_mut()
            .admit_agent(cert.clone(), delegation, &key, tilo_clock::wall())
            .is_err()
        {
            unreachable!("the agent is admitted to the workspace");
        }
    }
    let person = hex::encode(root.verifying_key().to_bytes());
    record_grants(
        workspaces,
        workspace,
        &[tilo_action_policy::Grant::new(
            "scribe-bridges",
            tilo_action_policy::Grantee::agent("scribe", person.as_str()),
            tilo_action_policy::TreePath::workspace(workspace).rest(),
            tilo_action_types::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(person),
        )],
    )
    .await;
    (key, cert)
}

/// The actions of `device`, as `workspaces`' device reaches it: over the link `device` keeps open
/// to it, or dialled by its key.
pub fn reach_device(
    workspaces: &Workspaces,
    device: [u8; 32],
    workspace: &str,
) -> tilo_peer_state::actions::RemoteDevice {
    let Ok(endpoint) = iroh_base::EndpointId::from_bytes(&device) else {
        unreachable!("a device's key is an endpoint id")
    };
    let service = workspaces.action_service();
    service.remote_held(device, workspace).unwrap_or_else(|| {
        service.remote(
            &workspaces.inner.host.endpoint,
            &iroh_base::EndpointAddr::new(endpoint),
            workspace,
        )
    })
}

/// The actions of the device at `address`, as `workspaces`' device dials it there: what a device
/// that finds another by its key through the network's discovery reaches, where a test has none.
pub fn reach_device_at(
    workspaces: &Workspaces,
    address: &iroh_base::EndpointAddr,
    workspace: &str,
) -> tilo_peer_state::actions::RemoteDevice {
    workspaces
        .action_service()
        .remote(&workspaces.inner.host.endpoint, address, workspace)
}

/// The root of the person both devices are of: the one the first device's fixture is
/// certified by.
pub fn persons_root() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

/// `device` as a caller presents itself.
pub fn caller(device: &DeviceCert) -> tilo_actions::remote::Caller {
    tilo_actions::remote::Caller::authenticated(device.user_public_key, device.device_public_key)
}

/// `request` of `verb` on `workspace` with `argument`, addressed as the workspace's governance
/// publishes it.
pub fn request(
    workspace: &str,
    verb: &str,
    argument: serde_json::Value,
) -> tilo_actions::remote::InvokeRequest {
    use tilo_actions::remote::{InvokeArgument, RemoteActionTarget, WORKSPACE_GOVERNANCE_PROVIDER};
    let serde_json::Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    tilo_actions::remote::InvokeRequest {
        asks: false,
        invocation: tilo_actions::InvocationId::new(),
        target: match argument.get("person").and_then(serde_json::Value::as_str) {
            // Removing is published on the person it removes.
            Some(person) if verb == tilo_action_policy::roles::governance::REMOVE_MEMBER => {
                RemoteActionTarget {
                    provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new(
                        tilo_actions::remote::workspace_member(workspace, person),
                        "remove",
                    ),
                }
            }
            // Pinning, unpinning and uninstalling are published on the app they change.
            _ => match (
                argument.get("app").and_then(serde_json::Value::as_str),
                verb,
            ) {
                (
                    Some(app),
                    super::actions::UNINSTALL_APP
                    | super::actions::PIN_APP
                    | super::actions::UNPIN_APP,
                ) => RemoteActionTarget {
                    provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new(
                        tilo_actions::remote::workspace_app(workspace, app),
                        match verb {
                            super::actions::UNINSTALL_APP => "uninstall",
                            super::actions::PIN_APP => "pin",
                            _ => "unpin",
                        },
                    ),
                },
                _ => RemoteActionTarget::workspace_governance(workspace, verb),
            },
        },
        argument: if argument.is_empty() {
            InvokeArgument::None
        } else {
            InvokeArgument::Json(argument)
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// `request` again, as its person's approve of it in `workspace` admits it: what only the
/// dispatch hands a provider, once their approve of the call's asked record admits it again.
pub fn approved(
    request: &tilo_actions::remote::InvokeRequest,
    workspace: &str,
) -> tilo_actions::remote::InvokeRequest {
    use tilo_actions::remote::Confirmation;
    use tilo_actions::remote::asked::{Approval, RecordRef};
    let mut again = request.clone();
    again.confirmation = Some(Confirmation::Asked(Approval::new(
        RecordRef {
            workspace: workspace.to_owned(),
            record: request.invocation.0.to_string(),
            device: None,
        },
        None,
    )));
    again
}

/// `request` as `caller` sends it from `device` in `workspace`: signed with the device's key
/// when the caller is its own person, or an agent that device handed its session to, and
/// [`admit_device`] admitted it, as a person's device signs every call it sends. An agent with a
/// key of its own signs under that key, never its person's device's.
pub fn signed_from(
    request: &tilo_actions::remote::InvokeRequest,
    workspace: &str,
    (device, caller): (&DeviceCert, &tilo_actions::remote::Caller),
) -> tilo_actions::remote::InvokeRequest {
    let person = hex::encode(device.user_public_key);
    let acting = caller.acting();
    let mut request = request.clone();
    if let Some(key) = admitted_key(device)
        && (!acting.is_agent() || acting.handed)
        && acting.people() == [person.as_str()]
    {
        request.signed = Some(Box::new(
            tilo_server_protocol::invocation::sign_invocation_now(
                workspace,
                &request,
                device,
                &key,
                tilo_clock::unix_now_ns(),
            )
            .expect("a member's device signs its call"),
        ));
    }
    request
}

impl super::actions::ServiceWorkspaceActions {
    /// This host for requests about workspace `workspace_id` from the device `certificate`,
    /// each call its own person sends signed with that device's key, as a member's device signs
    /// every call it sends ([`signed_from`]).
    pub fn device_page(
        &self,
        workspace_id: WorkspaceId,
        certificate: &DeviceCert,
    ) -> impl tilo_actions::remote::ActionProvider + Clone + Send + 'static {
        SignedBy {
            page: self.in_workspace_as(workspace_id, certificate),
            device: certificate.clone(),
            workspace: workspace_id.to_string(),
        }
    }
}

/// `page`, each call its device's person sends signed by `device` ([`signed_from`]).
#[derive(Clone)]
struct SignedBy<P> {
    page: P,
    device: DeviceCert,
    workspace: String,
}

impl<P: tilo_actions::remote::ActionProvider> tilo_actions::remote::ActionProvider for SignedBy<P> {
    fn installed_surface(&self, app_id: &str) -> tilo_actions::remote::Pending<Option<Vec<u8>>> {
        self.page.installed_surface(app_id)
    }

    fn query(
        &self,
        caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        self.page.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        // The device signs each call it sends, an answer sent again included.
        let mut request = request;
        request.signed = None;
        let request = signed_from(&request, &self.workspace, (&self.device, caller));
        self.page.invoke(caller, request)
    }

    fn observe(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        self.page.observe(caller, id)
    }

    fn next_change(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::Pending<()>> {
        self.page.next_change(caller, id)
    }

    fn cancel(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        self.page.cancel(caller, id)
    }
}

/// A new workspace of `workspaces`, the workspace carrying it, and its governance
/// as actions.
pub async fn hosted_governed(
    workspaces: &Workspaces,
) -> (String, WorkspaceId, super::actions::ServiceWorkspaceActions) {
    let summary = needed(workspaces.create("Team").await, "a new workspace");
    (
        summary.workspace,
        summary.workspace_id,
        host_dispatch(workspaces),
    )
}

/// The one dispatch that serves `workspaces`: the host's own where it has one, else one
/// attached now as the first. A person's answer to a call that waits for them comes back to
/// that one, and a host is handed no second.
pub fn host_dispatch(workspaces: &Workspaces) -> super::actions::ServiceWorkspaceActions {
    workspaces.inner.jobs.governance().unwrap_or_else(|| {
        let host = super::actions::ServiceWorkspaceActions::new();
        host.attach(workspaces);
        host
    })
}

thread_local! {
    /// The person a test's calls wait for, where one answers ([`Answering`]).
    static ANSWERING: std::cell::RefCell<Option<Arc<Answering>>> =
        const { std::cell::RefCell::new(None) };
}

/// The person the calls of a test wait for, answering every one the same way from the moment
/// they are made, and what they were shown.
pub struct Answering {
    approves: bool,
    asked: Mutex<Vec<tilo_actions::remote::asked::AskedFacts>>,
}

impl Answering {
    /// From now on in this test, the person approves every call that waits for them.
    pub fn approving() -> Arc<Self> {
        Self::answering_as(true)
    }

    /// From now on in this test, the person declines every call that waits for them.
    pub fn declining() -> Arc<Self> {
        Self::answering_as(false)
    }

    fn answering_as(approves: bool) -> Arc<Self> {
        let answering = Arc::new(Self {
            approves,
            asked: Mutex::default(),
        });
        ANSWERING.set(Some(Arc::clone(&answering)));
        answering
    }

    /// What they were shown, call by call.
    pub fn asked(&self) -> Vec<tilo_actions::remote::asked::AskedFacts> {
        self.asked.lock().unwrap().clone()
    }
}

/// `first`, the answer to `request` by `caller` on the connection `certificate` certified (to
/// the workspace `workspace_id`, or to the host), as the person it waits for answers it, where
/// it waits for one ([`Answering`]): approved, what the call answers once the person's approve
/// of its asked record carried it out; declined, or answered by nobody (`None`), it is
/// admitted no more and the caller hears what they were shown.
///
/// A call this host serves runs as its job: `Accepted` while it runs, and `Completed` once it
/// ends without asking. One that waits is kept as its asked record, which the person answers
/// there through `tilo.invocations#resolve`, and the call is followed to its end.
pub async fn answered(
    workspaces: &Workspaces,
    (workspace_id, certificate): (Option<WorkspaceId>, Option<&DeviceCert>),
    caller: &tilo_actions::remote::Caller,
    request: &tilo_actions::remote::InvokeRequest,
    first: Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
) -> Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError> {
    use tilo_actions::remote::{ActionCallError, ReceiptStatus};
    let answering = ANSWERING.with_borrow(Clone::clone);
    if let Err(ActionCallError::ConfirmationRequired(facts)) = &first {
        let facts = (**facts).clone();
        return approved_at_once(
            workspaces,
            (workspace_id, certificate),
            caller,
            request,
            first,
            answering.as_ref(),
            &facts,
        )
        .await;
    }
    let first = first?;
    if !matches!(first.status, ReceiptStatus::Accepted) {
        return Ok(first);
    }
    let (Some(service), Some(store)) =
        (workspaces.inner.jobs.governance(), workspaces.asked_store())
    else {
        return Ok(first);
    };
    // Whether it waits for its person, or ends first. It is answered the moment its record
    // shows, which the host may record as waiting a moment later: the answer stands either way.
    // A call no workspace was named for (one the host's own actions answer) is found by its
    // record, which names the workspace it waits in.
    let kept = tilo_test_env::within("the call waits for its person or ends", async {
        loop {
            let named = workspace_id.map(|id| id.to_string()).or_else(|| {
                store
                    .record(&request.invocation.0.to_string())
                    .map(|at| at.workspace)
            });
            if let Some(workspace) = named
                && let Some(waiting) = store
                    .waiting_in(&workspace)
                    .into_iter()
                    .find(|waiting| waiting.invocation == request.invocation)
            {
                return Some((waiting, WorkspaceId::parse(&workspace)));
            }
            if let Some(id) = workspace_id
                && service
                    .in_workspace(id)
                    .observe(caller, &request.invocation)
                    .is_some_and(|state| state.is_terminal())
            {
                return None;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let Some((waiting, workspace_id)) = kept else {
        return match workspace_id {
            Some(id) => ended_as(&service.in_workspace(id), caller, request.invocation).await,
            None => Ok(first),
        };
    };
    let Some(workspace_id) = workspace_id else {
        return Ok(first);
    };
    let here = service.in_workspace(workspace_id);
    // It waits in its asked record: nobody answering here leaves it there, and the caller hears
    // what its person is shown.
    let Some(answering) = &answering else {
        return Err(tilo_actions::remote::ActionCallError::ConfirmationRequired(
            Box::new(waiting.asked.facts.clone()),
        ));
    };
    answered_as_its_person(workspaces, caller, request, &waiting, answering, &here).await
}

/// The call of `request` the asked record `waiting` keeps, answered as the person it asks: an
/// approve admits the same call again, which ends when it ran; a decline ends it with nothing
/// done, and the caller's call was refused for want of their yes.
async fn answered_as_its_person(
    workspaces: &Workspaces,
    caller: &tilo_actions::remote::Caller,
    request: &tilo_actions::remote::InvokeRequest,
    waiting: &tilo_remote_actions::asked::Waiting,
    answering: &Answering,
    here: &(impl tilo_actions::remote::ActionProvider + Clone + Send + Sync + 'static),
) -> Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError> {
    use tilo_actions::remote::{
        ActionCallError, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };
    answering
        .asked
        .lock()
        .unwrap()
        .push(waiting.asked.facts.clone());
    let person: [u8; 32] = hex::decode(&waiting.person)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| ActionCallError::Refused {
            reason: "the ask names no person to answer it".into(),
        })?;
    let answering_as = tilo_actions::remote::Caller::authenticated(
        person,
        caller.device_public_key().copied().unwrap_or(person),
    );
    let resolution = if answering.approves {
        tilo_actions::remote::asked::Offer::Approve
    } else {
        tilo_actions::remote::asked::Offer::Decline
    };
    let Ok(serde_json::Value::Object(argument)) =
        serde_json::to_value(tilo_actions::remote::asked::Resolve {
            resolution,
            ..tilo_actions::remote::asked::Resolve::default()
        })
    else {
        return Err(ActionCallError::InvalidRequest {
            reason: "the answer does not read".into(),
        });
    };
    let resolve = InvokeRequest {
        asks: false,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_action_services::workspace_invocations::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                &waiting.asked.record.record,
                tilo_actions::remote::asked::RESOLVE,
            ),
        },
        argument: InvokeArgument::Json(argument),
        on_behalf_of: None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    settled(workspaces.resolve_asked(&answering_as, &resolve)).await?;
    if !answering.approves {
        return Err(ActionCallError::ConfirmationRequired(Box::new(
            waiting.asked.facts.clone(),
        )));
    }
    ended_as(here, caller, request.invocation).await
}

/// `first`, a call refused for the person's yes, answered once `answering` approves it: the same
/// call admitted again as its person, and what it answers once it ran. A decline, or nobody
/// answering, leaves the refusal as it is.
async fn approved_at_once(
    workspaces: &Workspaces,
    (workspace_id, certificate): (Option<WorkspaceId>, Option<&DeviceCert>),
    caller: &tilo_actions::remote::Caller,
    request: &tilo_actions::remote::InvokeRequest,
    first: Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    answering: Option<&Arc<Answering>>,
    facts: &tilo_actions::remote::asked::AskedFacts,
) -> Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError> {
    use tilo_actions::remote::ReceiptStatus;
    let Some(answering) = answering else {
        return first;
    };
    answering.asked.lock().unwrap().push(facts.clone());
    if !answering.approves {
        return first;
    }
    let workspace = workspace_id.map(|id| id.to_string()).unwrap_or_default();
    let value = Box::pin(workspaces.invoke(
        workspace_id,
        caller,
        certificate,
        &approved(request, &workspace),
    ))
    .await?;
    Ok(tilo_actions::remote::InvocationReceipt {
        invocation: request.invocation,
        status: ReceiptStatus::Completed {
            output: tilo_actions::ActionOutput {
                value: Some(value),
                ..tilo_actions::ActionOutput::default()
            },
        },
    })
}

/// What invocation `invocation` ended as, followed on `here` until it is terminal: the way a
/// caller follows a call at any moment, including the one its person's approve admitted again.
async fn ended_as(
    here: &(impl tilo_actions::remote::ActionProvider + Clone + Send + Sync + 'static),
    caller: &tilo_actions::remote::Caller,
    invocation: tilo_actions::InvocationId,
) -> Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError> {
    let state = tilo_test_env::within("the answered call's end", async {
        loop {
            match here.observe(caller, &invocation) {
                Some(state) if state.is_terminal() => return state,
                _ => tokio::task::yield_now().await,
            }
        }
    })
    .await;
    Ok(state.receipt())
}

/// `request` by `caller` on the connection `device` certified to the host, answered by its
/// person where it waits for them and one answers ([`Answering`]).
pub async fn host_operation_on(
    host: &super::actions::ServiceWorkspaceActions,
    device: &DeviceCert,
    caller: &tilo_actions::remote::Caller,
    request: tilo_actions::remote::InvokeRequest,
) -> Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError> {
    let first = settled(host.as_host(device).invoke(caller, request.clone())).await;
    let workspaces = host
        .shared
        .workspaces
        .get()
        .and_then(crate::workspaces::WeakWorkspaces::upgrade)
        .expect("the host's workspaces are held");
    answered(&workspaces, (None, Some(device)), caller, &request, first).await
}

/// A request to create a person's own workspace, under a fresh invocation id.
pub fn own_request(argument: serde_json::Value) -> tilo_actions::remote::InvokeRequest {
    let mut request = request(
        "",
        tilo_server_protocol::workspaces::CREATE_OWN_WORKSPACE,
        argument,
    );
    request.target = tilo_actions::remote::RemoteActionTarget {
        provider: tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER.into(),
        address: tilo_actions::ActionAddress::new(
            tilo_server_protocol::workspaces::HOST_WORKSPACES_RESOURCE,
            tilo_server_protocol::workspaces::CREATE_OWN_WORKSPACE,
        ),
    };
    request
}

/// Ask the host `request`, as `device`, and answer what a host operation completes with.
pub async fn host_operation(
    host: &super::actions::ServiceWorkspaceActions,
    device: &DeviceCert,
    request: tilo_actions::remote::InvokeRequest,
) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
    let receipt = host_operation_on(host, device, &caller(device), request).await?;
    match receipt.status {
        tilo_actions::remote::ReceiptStatus::Completed { output } => {
            Ok(output.value.unwrap_or(serde_json::Value::Null))
        }
        other => panic!("a host operation completes at once: {other:?}"),
    }
}

/// A person's own workspace made by the host, as `device` asks it.
pub async fn create_own(
    host: &super::actions::ServiceWorkspaceActions,
    device: &DeviceCert,
    argument: serde_json::Value,
) -> Result<tilo_server_protocol::workspaces::CreatedWorkspace, tilo_actions::remote::ActionCallError>
{
    host_operation(host, device, own_request(argument))
        .await
        .map(|value| serde_json::from_value(value).unwrap())
}

/// Name the home workspace whose people `workspaces` keeps workspaces of
/// their own for, as a site's server names its own: returns it.
pub async fn home_of(workspaces: &Workspaces) -> String {
    let home = needed(workspaces.create("Lounge").await, "the home workspace").workspace;
    assert!(workspaces.admit_own_through(&home));
    home
}

/// A host keeping workspaces of people's own for the people its home
/// workspace lets in: returns the home.
pub async fn open_home(workspaces: &Workspaces) -> String {
    workspaces
        .offer_own_workspaces(super::OwnWorkspaceOffer::free("tilo.test"))
        .expect("the host offers workspaces of people's own");
    home_of(workspaces).await
}

/// Let `person` in as the site does for someone it invites: an invitation
/// into `home` names them.
pub async fn let_in(workspaces: &Workspaces, home: &str, person: [u8; 32]) -> Vec<u8> {
    workspaces
        .issue(
            home,
            workspaces.device_cert(),
            super::IssueInvitationRequest {
                request_id: rand::random(),
                role: tilo_peer_state::join::Role::Viewer,
                recipient_user_public_key: Some(person),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .expect("the invitation is issued")
        .signed_invitation
}

/// The workspaces `host`, a dispatch attached to them, answers for.
pub fn workspaces_served(host: &super::actions::ServiceWorkspaceActions) -> Workspaces {
    host.shared
        .workspaces
        .get()
        .and_then(crate::workspaces::WeakWorkspaces::upgrade)
        .expect("the dispatch serves the workspaces it was attached to")
}

/// A person's root as the device that keeps it runs what waits for it
/// ([`super::at_root`]): it certifies an agent's key for one workspace, and runs none of a
/// host's own root-only operations.
#[cfg(not(target_arch = "wasm32"))]
pub struct RootIn(pub SigningKey);

#[cfg(not(target_arch = "wasm32"))]
impl super::at_root::RootKept for RootIn {
    fn runs(&self, _: &tilo_actions::remote::InvokeRequest) -> bool {
        false
    }

    fn run(
        &self,
        request: tilo_actions::remote::InvokeRequest,
        _: tilo_actions::remote::Caller,
    ) -> super::at_root::RootRun {
        panic!("this root runs no operation of a host's own: {request:?}")
    }

    fn certify_agent(
        &self,
        person: [u8; 32],
        key: [u8; 32],
        workspace: &str,
        _: &str,
        name: &str,
    ) -> super::at_root::Certifying {
        assert_eq!(
            person,
            self.0.verifying_key().to_bytes(),
            "only its own person's root is kept here"
        );
        let scope = tilo_user_identity::AgentScope::parse(workspace);
        let now = tilo_clock::unix_now_ns();
        let mut cert = scope.map(|scope| DeviceCert {
            user_public_key: person,
            device_public_key: key,
            device_label: name.to_owned(),
            capabilities: DeviceCapabilities::agent_grant(scope),
            issued_at_ns: now,
            expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
            signature: [0; 64],
            issuer: None,
        });
        let signed = cert
            .as_mut()
            .ok_or_else(|| format!("{workspace} is no workspace's id"))
            .and_then(|cert| sign_device_cert(cert, &self.0).map_err(|error| error.to_string()))
            .map(|()| cert);
        Box::pin(async move { signed })
    }
}

/// Write `record` of `app`'s `kind` in `workspace` as the device `workspaces`, a site, writes
/// the person's asked calls there as: what a site that went beyond its grant would write.
///
/// # Errors
/// The site was granted no asked calls there, or the write is refused.
pub async fn write_as_asked_calls(
    workspaces: &Workspaces,
    workspace: &str,
    (app, kind, record): (&str, &str, &str),
) -> Result<(), String> {
    let device = workspaces
        .asks_device(workspace)
        .await
        .ok_or("this site was granted no asked calls there")??;
    workspaces
        .author_record_as(device, workspace, (app, kind, record), |doc| {
            automerge::transaction::Transactable::put(
                doc,
                automerge::ROOT,
                "written by the site",
                true,
            )
            .map_err(|error| error.to_string())
        })
        .await
}

/// `workspaces`' copy of `workspace` hears from `device`, one of the person's, of `revocations`.
pub async fn hear_revocations(
    workspaces: &Workspaces,
    workspace: &str,
    device: &DeviceCert,
    revocations: Vec<tilo_user_identity::RevocationEntry>,
) {
    let state = workspaces
        .workspace(workspace)
        .await
        .expect("the copy is kept here")
        .lock()
        .await
        .peer
        .clone();
    state
        .write(|replica| {
            replica.receive_from(
                tilo_peer_state::TransportOrigin::UntrustedPresence(device.device_public_key),
                neighbour_wire::NeighbourMsg::IdentityHello {
                    protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                    cert: Box::new(device.clone()),
                    revocations,
                },
                tilo_clock::wall(),
            )
        })
        .expect("the copy takes the revocations in");
}
