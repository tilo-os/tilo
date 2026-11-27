//! The language runtime the daemon's program mounts, driven as the program mounts it: the
//! agents and triggers that read and change the records of the built-in apps, the packages those
//! records are of compiled from their sources, and the daemon itself through its API, its
//! dispatch and the device link, each test on a daemon of its own.

mod app_records_tests;
mod approved_checks;
mod calendar_bridge_tests;
mod declared_kinds_tests;
mod disk_unlock_tests;
mod founded_apps_tests;
mod hosted_own_agent_tests;
mod install_on_open;
mod mail_bridge_tests;
mod moderation_history_tests;
mod own_agent_tests;
mod record_roots_tests;
mod resource_writes_tests;
mod served_notices_tests;
mod store_lane_tests;
mod task_work_tests;
mod triggers_tests;
mod wakeups_tests;

/// A daemon of its own, in a process of its own, with the language mounted as the program
/// mounts the one it links.
mod host;

mod account_keys;
mod agent_fixture;
mod agent_keys;
mod agent_machine;
mod agent_member;
mod agent_models;
mod agent_on_device;
mod agent_presence;
mod agent_scheduling;
mod agent_terminal;
mod agent_windows;
mod api_admin;
mod api_beside_import;
mod api_hosted;
mod api_key;
mod api_model_asks;
mod api_records;
mod api_workspaces;
mod app_views;
mod chat;
mod customer_workspaces;
mod daemon_flows;
mod desktop_page;
mod device_carry;
mod device_key;
mod device_vpn_tunnel;
mod device_writes;
mod files_collab;
mod files_content;
mod files_paths;
/// What the Files tests read of what a daemon keeps.
mod files_support;
mod files_views;
mod first_device;
/// A person who is not the device's own, admitted by an invitation and asking over the link.
mod guests;
mod hosted_agent_terminal;
mod hosted_mail_accounts;
mod hosted_terminal_fresh_tab;
mod hosted_turns;
mod hosted_web_call;
mod import_rate;
mod in_person;
mod invocations;
mod joining;
mod keeping_pages;
mod migrate_engine;
mod migrate_walk;
mod migrate_window;
mod nearby_send;
mod provided;
mod refresh_reads;
mod search;
mod shared_workspace;
mod shell_link;
// ush's shell is Linux's, so the daemon's shell runs are driven where it builds.
#[cfg(target_os = "linux")]
mod shell_runtime;
#[cfg(target_os = "linux")]
mod terminal_streams;
mod throughput;
mod tracker;
mod workspace_list;
mod workspace_members;
mod workspace_records;

use std::collections::BTreeMap;
use std::sync::Arc;

use collab_types::store::wire::{Command, Edit};
use collab_types::store::{Cell, Resource, Value as StoreValue};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_app_host::services::ResourceRequestContext;
use tilo_app_host::test_support::Started;
use tilo_app_host::triggers::{Firing, Runner};
use tilo_member_types::Actor as Who;
use tilo_resource_store::Stamp;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::service::local_caller;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, TestHost, declare_built_in_app, hosting_device, test_host_holding,
};
use tilo_workspace_service::workspaces::{DocumentRoute, MemberDevice, ResourceWrite, Workspaces};

/// Mount the language runtime the daemon links on `workspaces`, as its program does when it
/// opens: its own roles read and change the built-in apps' records, and the records are served
/// to whoever the workspace's dispatch admits.
pub(crate) fn mount(workspaces: &Workspaces) {
    workspaces
        .provide_tilolang(tilolang_workspace::host())
        .expect("the build's tilolang apps declare their surfaces");
    if let Some(tilolang) = workspaces.tilolang() {
        tilolang.records.serve(workspaces);
    }
}

/// A test's device: its own state on disk, the workspace service with the language mounted, and
/// the person's one workspace made and routable.
pub(crate) struct Device {
    _state: tempfile::TempDir,
    pub(crate) key: ed25519_dalek::SigningKey,
    pub(crate) cert: DeviceCert,
    host: TestHost,
    pub(crate) workspace: String,
}

impl Device {
    pub(crate) fn workspaces(&self) -> &Workspaces {
        self.host.workspaces()
    }

    /// The host, for a test that declares a surface or installs a release itself.
    pub(crate) fn host(&self) -> &TestHost {
        &self.host
    }

    /// The person's id, as the workspace names them.
    pub(crate) fn person(&self) -> String {
        hex::encode(self.cert.user_public_key)
    }

    pub(crate) async fn shutdown(self) {
        self.host.shutdown().await;
    }
}

impl Device {
    /// A device that made the workspace `name`, with `app`'s built-in record declared and routed
    /// there when one is named.
    pub(crate) async fn start(name: &str, app: Option<&str>) -> Device {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = hosting_device();
        let host =
            test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default())).await;
        let workspaces = host.workspaces().clone();
        mount(&workspaces);
        workspaces.set_member_device(Arc::new(MemberDevice {
            cert: cert.clone(),
            signer: Arc::new(key.clone()),
        }));
        let workspace = workspaces.create(name).await.unwrap().workspace;
        if let Some(app) = app {
            declare_built_in_app(&host, app);
            let route = record_route(&workspace, app);
            let _ = workspaces.own_route_keys(&route, true).await.unwrap();
        }
        Device {
            _state: dir,
            key,
            cert,
            host,
            workspace,
        }
    }
}

/// The app surfaces this process decides its changes against, kept once for the whole test
/// binary as a program keeps them: a host decides every change it receives against the surface
/// the change's app publishes, before it opens a workspace, and one it has not kept yet holds
/// the change back.
pub(crate) fn set_surfaces() {
    static KEPT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let kept = KEPT.get_or_init(|| {
        let kept = tempfile::TempDir::new().expect("a directory for the process's surfaces");
        tilo_workspace_service::surfaces::decide_receipt(&kept.path().join("surfaces"));
        kept
    });
    // Kept for the life of the process, as a program keeps it: dropping it would take the
    // surfaces away from a test still running.
    let _ = kept.path();
}

/// The route a record of `app` is opened, written and read by in `workspace`: where its frames
/// travel and the address the record reads and writes name it at.
pub(crate) fn record_route(workspace: &str, app: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: app.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    }
}

/// `operation` of `workspace`'s governance with `arguments`, asked by `cert`'s device over the
/// workspace service's dispatch, as every caller asks it: what a window's button calls.
pub(crate) async fn govern(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    operation: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    let request = InvokeRequest {
        asks: false,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, operation),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let caller = local_caller(cert).in_workspace(workspace);
    let receipt = settled(workspaces.device_actions().invoke(&caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(serde_json::Value::Null)),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("an administration change completes at once: {other:?}"),
    }
}

/// What a pending answer comes to, on this runtime.
pub(crate) async fn settled<T: Send + 'static>(pending: Pending<T>) -> T {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    pending.then(move |value| {
        let _ = sender.send(value);
    });
    tilo_test_env::within("the dispatch's answer", receiver)
        .await
        .unwrap()
        .expect("answered")
}

/// The key a device makes for its agents: one fixed key here.
pub(crate) struct Keys(pub(crate) [u8; 32]);

impl tilo_app_host::agents::agent_keys::AgentKeys for Keys {
    fn grant(
        &self,
        _: tilo_server_protocol::WorkspaceId,
        _: &str,
        _: &str,
        _: [u8; 32],
    ) -> Result<[u8; 32], String> {
        Ok(self.0)
    }

    fn sign(
        &self,
        _: tilo_server_protocol::WorkspaceId,
        _: &[u8; 32],
        _: &mut tilo_actions::remote::InvokeRequest,
    ) -> Result<(), String> {
        Ok(())
    }

    fn made(&self, _: &[u8; 32]) -> Option<(DeviceCert, ed25519_dalek::SigningKey)> {
        None
    }
}

/// An agent's own key, certified by its person's root for that agent in one workspace, as the
/// person's device makes it: each call the agent's turn asks is signed with it, so the workspace
/// reads whose agent asked from the certificate's chain.
pub(crate) struct CertifiedKeys {
    cert: DeviceCert,
    key: ed25519_dalek::SigningKey,
}

impl CertifiedKeys {
    /// `agent`'s key in `workspace`, certified by `root`.
    pub(crate) fn of(root: &ed25519_dalek::SigningKey, workspace: &str, agent: &str) -> Self {
        let workspace = tilo_server_protocol::WorkspaceId::parse(workspace)
            .expect("a workspace id")
            .as_uuid()
            .into_bytes();
        let key = ed25519_dalek::SigningKey::from_bytes(&rand::random());
        let issued = tilo_clock::unix_now_ns();
        // An agent's grant runs out, as every one a person's device makes does.
        let day = 24 * 60 * 60 * 1_000_000_000;
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: agent.into(),
            capabilities: tilo_user_identity::DeviceCapabilities::agent_grant(
                tilo_user_identity::AgentScope::agent_in(workspace, agent),
            ),
            issued_at_ns: issued,
            expires_at_ns: Some(issued + day),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).expect("the agent's grant signs");
        Self { cert, key }
    }
}

impl tilo_app_host::agents::agent_keys::AgentKeys for CertifiedKeys {
    fn grant(
        &self,
        _: tilo_server_protocol::WorkspaceId,
        _: &str,
        _: &str,
        _: [u8; 32],
    ) -> Result<[u8; 32], String> {
        Ok(self.cert.device_public_key)
    }

    fn sign(
        &self,
        workspace: tilo_server_protocol::WorkspaceId,
        _: &[u8; 32],
        request: &mut tilo_actions::remote::InvokeRequest,
    ) -> Result<(), String> {
        request.signed = None;
        let signed = tilo_server_protocol::invocation::sign_invocation_now(
            &workspace.to_string(),
            request,
            &self.cert,
            &self.key,
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| error.to_string())?;
        request.signed = Some(Box::new(signed));
        Ok(())
    }

    fn made(&self, key: &[u8; 32]) -> Option<(DeviceCert, ed25519_dalek::SigningKey)> {
        (key == &self.cert.device_public_key).then(|| (self.cert.clone(), self.key.clone()))
    }
}

/// A record write the app's program makes for one of its actions, as `by` acts: `fields` are its
/// text fields and `sets` its list fields' members, `edits` the values its fields hold whole (a
/// message's rich body among them). The write reaches `workspace`'s store as `app`'s, and an
/// agent's change is the person's it works for.
pub(crate) async fn write(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    app: &str,
    mut write: Write<'_>,
) {
    let invocation = tilo_actions::InvocationId(uuid::Uuid::new_v4());
    let acted_for = matches!(write.by, Who::Member(tilo_actions::MemberId::Agent { .. }))
        .then(|| hex::encode(cert.user_public_key));
    let stamp = Stamp {
        verb: write.verb.to_owned(),
        invoker: Stamp::by(write.by),
        acted_for: acted_for.clone(),
        invocation: invocation.0.to_string(),
        at: jiff::Timestamp::now().to_string(),
        ..Stamp::default()
    };
    let mut held: BTreeMap<String, Cell> = write
        .fields
        .drain(..)
        .map(|(name, value)| (name.to_owned(), Cell::One(StoreValue::Text(value))))
        .collect();
    // A set's members are the values the index holds, each kept as the JSON text it reads back
    // as: a direct conversation's `people` is how its type names who reads it.
    for (name, members) in write.sets.drain(..) {
        held.insert(
            name.to_owned(),
            Cell::Set(
                members
                    .into_iter()
                    .map(|member| StoreValue::Text(member.to_string()))
                    .collect(),
            ),
        );
    }
    let mut commands = vec![Command::Put {
        kind: write.kind.into(),
        id: write.id.into(),
        resource: Resource {
            // A direct conversation is read by the people it names, so its write names no
            // readers.
            readers: write.readers.into(),
            parent: write.parent.map(str::to_owned),
            order: None,
            fields: held,
        },
        stamp: stamp.clone(),
    }];
    if !write.edits.is_empty() {
        commands.push(Command::Edit {
            kind: write.kind.into(),
            id: write.id.into(),
            edits: write
                .edits
                .drain(..)
                .map(|(field, value)| Edit::Set {
                    field: field.into(),
                    value,
                })
                .collect(),
            stamp,
        });
    }
    let caller =
        Caller::authenticated(cert.user_public_key, cert.device_public_key).in_workspace(workspace);
    // An agent writes here as the device hands it its person's session: the device answers for it.
    let serving = (
        invocation,
        match &acted_for {
            Some(person) => tilo_actions::Acting::from(write.by.clone())
                .handed()
                .acting_for(tilo_actions::Acting::person(person.as_str())),
            None => tilo_actions::Acting::from(write.by.clone()),
        },
    );
    let context =
        ResourceRequestContext::authenticated(app, workspace, caller).serving(vec![serving]);
    let command = if commands.len() == 1 {
        commands.pop().unwrap()
    } else {
        Command::Batch(commands)
    };
    workspaces
        .resource_command(&context, command)
        .await
        .unwrap();
}

/// One write [`write`] makes.
pub(crate) struct Write<'a> {
    pub(crate) kind: &'a str,
    pub(crate) id: &'a str,
    pub(crate) parent: Option<&'a str>,
    pub(crate) verb: &'a str,
    pub(crate) by: &'a Who,
    pub(crate) readers: &'a str,
    pub(crate) fields: Vec<(&'a str, String)>,
    pub(crate) sets: Vec<(&'a str, Vec<serde_json::Value>)>,
    pub(crate) edits: Vec<(&'a str, serde_json::Value)>,
}

/// What `runner` fires on the records `writes` told it of since it last looked.
pub(crate) async fn fired(
    runner: &mut Runner,
    writes: &mut tokio::sync::broadcast::Receiver<ResourceWrite>,
    started: &Arc<Started>,
) -> Vec<Firing> {
    let mut since = Vec::new();
    while let Ok(write) = writes.try_recv() {
        since.push(write);
    }
    let before = started.0.lock().unwrap().len();
    runner.pass(since).await;
    started.0.lock().unwrap()[before..].to_vec()
}
