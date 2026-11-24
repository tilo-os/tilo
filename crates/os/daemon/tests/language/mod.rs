//! The workspace service with a language runtime mounted on it, as the daemon's program mounts
//! the one it links: the agents and triggers that read and change the records of the built-in
//! apps are driven here, and the packages those records are of compile from their sources.

mod hosted_own_agent_tests;
mod moderation_history_tests;
mod own_agent_tests;
mod record_roots_tests;
mod served_notices_tests;
mod task_work_tests;
mod triggers_tests;

use std::collections::BTreeMap;
use std::sync::Arc;

use tilo_action_policy::Actor as Who;
use tilo_actions::remote::Caller;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_resource_store::wire::{Command, Edit};
use tilo_resource_store::{Cell, Resource, Stamp, Value as StoreValue};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::apps::services::ResourceRequestContext;
use tilo_workspace_service::apps::triggers::{Firing, Runner};
use tilo_workspace_service::workspaces::fixtures::{
    Started, TestGroupKeys, TestHost, declare_built_in_app, governor, test_host_holding,
};
use tilo_workspace_service::workspaces::{MemberDevice, ResourceWrite, Workspaces};

/// Mount the language runtime the daemon links on `workspaces`, as its program does when it
/// opens: its own roles read and change the built-in apps' records, and the records are served
/// to whoever the workspace's dispatch admits.
pub(crate) fn mount(workspaces: &Workspaces) {
    workspaces.provide_tilolang(tilolang_workspace::host());
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
        let (key, cert) = governor();
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
            let route = DocumentRoute {
                app_id: app.into(),
                cluster_id: workspace.clone(),
                document: String::new(),
            };
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

/// The key a device makes for its agents: one fixed key here.
pub(crate) struct Keys(pub(crate) [u8; 32]);

impl tilo_workspace_service::agents::agent_keys::AgentKeys for Keys {
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
    let acted_for = matches!(write.by, Who::Member(tilo_actions::MemberId::Agent(_)))
        .then(|| hex::encode(cert.user_public_key));
    let stamp = Stamp {
        verb: write.verb.to_owned(),
        invoker: Stamp::by(write.by),
        acted_for,
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
    let serving = (invocation, write.by.clone());
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
