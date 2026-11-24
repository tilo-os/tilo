#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

//! This device's server settings as a desktop session's apps read and change them: the daemon's
//! one copy, read, watched and changed through the shell link, as the app asking. No service of
//! the session's publishes the resource.

use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

use tilo_runtime::device::server_settings::{Published, SERVER, Served};
use tilo_test_env::within;
use tilo_workspace_service::apps::collab_broker::FanoutState;
use tilo_workspace_service::apps::services::{
    Caller, ProcessServices, PublishedResources, Reading, ResourceWatch,
};
use tilo_workspace_service::apps::shell::{
    ShellActions, ShellCaller, ShellDispatch, ShellLinks, ShellServices,
};
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, governor_with, test_host_holding,
};

/// The Settings app's window, and the tilolang package of the same app: the two callers the
/// resource is granted to.
const SETTINGS: &str = tilo_app_id::apps::SETTINGS;
const SETTINGS_PACKAGE: &str = "so.tilo.lang.settings";
/// Any other app on this device.
const OTHER: &str = "so.tilo.notes";

fn caller(app: &str) -> Caller {
    Caller {
        app_id: app.to_owned(),
        workspace: String::new(),
    }
}

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

/// A session's app wire, the daemon it reaches, and the configuration the daemon keeps.
struct Device {
    _dir: tempfile::TempDir,
    _host: tilo_workspace_service::workspaces::fixtures::TestHost,
    _daemon: tilo_workspace_service::apps::collab_broker::BrokerHandle,
    link: std::sync::Arc<crate::app_wire::Link>,
    services: tilo_service::Services,
    config: std::path::PathBuf,
}

/// The device, its daemon publishing `device:server` and the session reaching it on the shell
/// link it handed the session.
async fn device() -> Device {
    use tokio::io::AsyncWriteExt as _;
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
    let config = dir.path().join("server.toml");
    let published: Arc<dyn PublishedResources> =
        Arc::new(Published::new(Served::at(config.clone())));
    let socket = dir.path().join("daemon.sock");
    let links = Arc::new(ShellLinks::default());
    let workspaces = host.workspaces().clone();
    let daemon = tilo_workspace_service::apps::collab_broker::spawn_with_services(
        socket.clone(),
        None,
        tilo_workspace_service::apps::collab_outbound::PeerPublisher::disabled(),
        None,
        ProcessServices {
            shell: Some(Arc::new(ShellServices {
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
                published: Some(published),
                workspaces,
            })),
            ..ProcessServices::default()
        },
        Arc::default(),
        Arc::new(FanoutState::new()),
    )
    .unwrap();
    let secret = links.make(ShellCaller {
        person: cert.user_public_key,
    });
    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let mut opening = runtime_collab::wire::shell::SHELL_MAGIC.to_vec();
    opening.extend_from_slice(&secret);
    stream.write_all(&opening).await.unwrap();
    let (link, _) = runtime_collab::ShellClient::open(stream);
    let shell = tilo_session_client::daemon_documents::DaemonShell::new(
        Ok(link),
        tokio::runtime::Handle::current(),
    );
    let session = crate::app_wire::Link::for_test();
    session.reaching(shell);
    Device {
        _dir: dir,
        _host: host,
        _daemon: daemon,
        link: session,
        services: tilo_service::Services::default(),
        config,
    }
}

impl Device {
    /// Answer what the apps asked, as the session's loop does a pass.
    fn serve(&mut self) {
        self.link.serve(&mut self.services);
    }
}

/// The reading an answer of the session's services carried, decoded.
fn reading(answer: Result<Reading, String>) -> serde_json::Value {
    let (_, reading) = answer.unwrap();
    serde_json::from_slice(&reading.expect("the resource has a reading")).unwrap()
}

/// The next reading a watch sent, decoded.
async fn next(
    readings: &mut tokio::sync::mpsc::UnboundedReceiver<runtime_collab::wire::Event>,
) -> serde_json::Value {
    let event = within("a reading of the resource", readings.recv())
        .await
        .expect("the watch is held open");
    let runtime_collab::wire::Event::Reading { reading, .. } = event else {
        panic!("a watch sends readings");
    };
    serde_json::from_slice(&reading.expect("the resource has a reading")).unwrap()
}

/// The work a reading says this device takes.
fn takes(reading: &serde_json::Value) -> &str {
    reading["work"]["from"].as_str().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_reads_and_changes_this_device_s_server_at_its_daemon() {
    let mut device = device().await;

    // Settings reads the reading from the daemon: no service of the session publishes it.
    let answered = device.link.snapshot(&caller(SETTINGS), SERVER);
    device.serve();
    assert_eq!(
        takes(&reading(within("the reading", answered).await.unwrap())),
        "nobody"
    );

    // Settings' tilolang package shows the same settings, granted as the same app.
    let answered = device.link.snapshot(&caller(SETTINGS_PACKAGE), SERVER);
    device.serve();
    assert_eq!(
        takes(&reading(within("the reading", answered).await.unwrap())),
        "nobody"
    );

    // A watch follows the daemon's reading, and the change an action makes.
    let (sink, mut readings) = tokio::sync::mpsc::unbounded_channel();
    let watch = device.link.watch(&caller(SETTINGS), SERVER, sink).unwrap();
    device.serve();
    assert_eq!(takes(&next(&mut readings).await), "nobody");

    let arguments = serde_json::to_vec(&serde_json::json!({ "work": "own-devices" })).unwrap();
    let answered = device
        .link
        .invoke(&caller(SETTINGS), SERVER, "set-work", arguments);
    device.serve();
    assert!(matches!(
        within("the action's answer", answered).await.unwrap(),
        runtime_collab::wire::Receipt::Done(_)
    ));
    assert_eq!(takes(&next(&mut readings).await), "own-devices");
    device.link.unwatch(watch);

    // The daemon applied it to the configuration it keeps.
    let config = tilo_server_protocol::config::ServerConfig::load(&device.config).unwrap();
    assert_eq!(config.work.from, tilo_server_protocol::WorkFrom::OwnDevices);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_app_reads_nothing_and_changes_nothing() {
    let mut device = device().await;

    let answered = device.link.snapshot(&caller(OTHER), SERVER);
    device.serve();
    let refused = within("the refusal", answered).await.unwrap();
    assert!(refused.is_err(), "another app reads nothing: {refused:?}");

    let arguments = serde_json::to_vec(&serde_json::json!({ "work": "anyone" })).unwrap();
    let answered = device
        .link
        .invoke(&caller(OTHER), SERVER, "set-work", arguments);
    device.serve();
    assert!(matches!(
        within("the refusal", answered).await.unwrap(),
        runtime_collab::wire::Receipt::Refused { .. }
    ));
    assert!(
        !device.config.exists(),
        "no configuration is made for an app the resource is not granted to"
    );
}
