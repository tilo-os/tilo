#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

//! An agent this session admits reads the apps' resources from the device's daemon, over the
//! shell's link: a command answered there, and a query it holds open asked again on each write
//! the daemon takes.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

use tilo_resource_store::wire::{Command, Reply};
use tilo_resource_store::{Cell, Query, Resource, Stamp, Value};
use tilo_workspace_service::apps::services::{
    Caller, ProcessServices, PublishedResources, Reading, ResourceRequestContext, ResourceServices,
    ResourceWatch,
};
use tilo_workspace_service::apps::shell::{
    ShellActions, ShellCaller, ShellDispatch, ShellLinks, ShellServices,
};
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, governor_with, test_host_holding,
};

use super::DaemonResources;

const APP: &str = "so.tilo.tasks";
const KIND: &str = "tasks.issue";

struct NoActions;

impl ShellActions for NoActions {
    fn dispatch(&self, _: &ShellCaller, _: Option<String>) -> ShellDispatch {
        Box::pin(std::future::ready(Err("no actions here".to_owned())))
    }
}

struct NoStanding;

impl PublishedResources for NoStanding {
    fn snapshot(
        &self,
        _: &Caller,
        _: &str,
    ) -> tokio::sync::oneshot::Receiver<Result<Reading, String>> {
        tokio::sync::oneshot::channel().1
    }
    fn watch(
        &self,
        _: &Caller,
        _: &str,
        _: tokio::sync::mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        Err("nothing stands here".to_owned())
    }
    fn unwatch(&self, _: ResourceWatch) {}
    fn invoke(
        &self,
        _: &Caller,
        _: &str,
        _: &str,
        _: Vec<u8>,
    ) -> tokio::sync::oneshot::Receiver<runtime_collab::wire::Receipt> {
        tokio::sync::oneshot::channel().1
    }
}

fn put(id: &str, title: &str) -> Command {
    Command::Put {
        kind: KIND.to_owned(),
        id: id.to_owned(),
        resource: Resource {
            readers: "*".into(),
            parent: None,
            order: None,
            fields: BTreeMap::from([("title".to_owned(), Cell::One(Value::Text(title.into())))]),
        },
        stamp: Stamp {
            verb: "add".to_owned(),
            invocation: format!("put-{id}"),
            ..Stamp::default()
        },
    }
}

fn titles(page: &tilo_resource_store::Page) -> Vec<Value> {
    page.resources
        .iter()
        .map(|(_, kept)| kept.scalar("title"))
        .collect()
}

/// The daemon's dispatch over `workspaces`, and this session's resources read from it over the
/// shell link it makes for `person`.
async fn over_the_link(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    socket: &std::path::Path,
    person: [u8; 32],
) -> (
    tilo_workspace_service::apps::collab_broker::BrokerHandle,
    DaemonResources,
) {
    use tokio::io::AsyncWriteExt as _;
    let socket = socket.to_path_buf();
    let links = Arc::new(ShellLinks::default());
    let dispatch = tilo_workspace_service::apps::collab_broker::spawn_with_services(
        socket.clone(),
        None,
        tilo_workspace_service::apps::collab_outbound::PeerPublisher::disabled(),
        None,
        ProcessServices {
            shell: Some(Arc::new(ShellServices {
                processes: None,
                links: Arc::clone(&links),
                socket: socket.clone(),
                documents: Arc::new(
                    tilo_workspace_service::apps::service_reads::ServiceDocuments::of(
                        workspaces.clone(),
                    ),
                ),
                writes: Arc::new(
                    tilo_workspace_service::apps::service_writes::ServiceWrites::of(
                        workspaces.clone(),
                    ),
                ),
                document_writes: Arc::new({
                    let workspaces = workspaces.clone();
                    move || workspaces.document_writes()
                }),
                actions: Arc::new(NoActions),
                standing: Arc::new(NoStanding),
                published: None,
                workspaces: workspaces.clone(),
            })),
            ..ProcessServices::default()
        },
        Arc::default(),
        Arc::new(tilo_workspace_service::apps::collab_broker::FanoutState::new()),
    )
    .unwrap();
    let secret = links.make(ShellCaller { person });
    let mut link = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let mut opening = runtime_collab::wire::shell::SHELL_MAGIC.to_vec();
    opening.extend_from_slice(&secret);
    link.write_all(&opening).await.unwrap();
    let (link, _) = runtime_collab::ShellClient::open(link);
    let shell = tilo_session_client::daemon_documents::DaemonShell::new(
        Ok(link),
        tokio::runtime::Handle::current(),
    );
    let resources = DaemonResources {
        writes: tilo_session_client::daemon_documents::DaemonWritesFeed::start(&shell),
        shell,
    };
    (dispatch, resources)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_reads_and_follows_resources_over_the_shell_link() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let host = test_host_holding(
        &dir.path().join("host"),
        &key,
        &cert,
        Arc::new(TestGroupKeys::default()),
    )
    .await;
    let workspaces = host.workspaces().clone();
    workspaces.set_member_device(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Tasks").await.unwrap().workspace;
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    let _ = workspaces.own_route_keys(&route, true).await.unwrap();
    workspaces
        .own_resource_command(&workspace, APP, put("i-1", "First"))
        .await
        .unwrap();

    let (_dispatch, resources) = over_the_link(
        &workspaces,
        &dir.path().join("collab.sock"),
        cert.user_public_key,
    )
    .await;
    let context = ResourceRequestContext::for_tests(
        APP,
        &workspace,
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(&workspace),
    );

    let counted = resources
        .command(
            &context,
            Command::Count {
                kind: KIND.to_owned(),
                filters: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(counted, Reply::Count(1)), "{counted:?}");
    let refused = resources.command(&context, put("i-2", "Mine")).await;
    assert!(
        refused.is_err(),
        "an agent here writes no resource: {refused:?}"
    );

    let mut pages = resources.open(&context, Query::of(KIND, 10)).await.unwrap();
    let first = pages.recv().await.expect("the first page");
    assert_eq!(titles(&first), [Value::Text("First".into())]);
    workspaces
        .own_resource_command(&workspace, APP, put("i-1", "Renamed"))
        .await
        .unwrap();
    let next = tokio::time::timeout(std::time::Duration::from_secs(10), pages.recv())
        .await
        .expect("a write the daemon took sends the page it changed")
        .expect("the query is held open");
    assert_eq!(titles(&next), [Value::Text("Renamed".into())]);
}
