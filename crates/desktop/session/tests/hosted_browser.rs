//! A session in a browser environment: the server hands it a workspace, a record and a device, and
//! it joins as that device, opens Browser on the record, and says what it became
//! (`docs/hosted-browser.md`, "What the session in the VM is handed").
//!
//! The environment's home is fresh every boot, so the identity the server certifies is the only one
//! this session has, and the join is the server's own: it asks the server to enter it into the
//! workspace, redeems the invitation the server answers with, and holds the workspace as any device
//! does. The window is a package's, mapped by a client of this test's own as a toolkit's window
//! maps: the session opens Browser through the launcher the way it opens any app. The app's side
//! of that window is this test's too: a connection to the session's app wire as the Browser the
//! session started, joined as the handed device and publishing its page. That is what the
//! session's broker lists the window by, and the provider the session reports is the one the
//! broker lists, never the tab the shell shows the window in: a caller reaches a window by what the
//! broker lists, so a window nothing is listed for is reported as nothing.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use runtime_collab::{CollabClient, ExportedAction, ExportedInput};
use tilo_actions::remote::browser::READY;
use tilo_actions::remote::{CatalogueQuery, RemoteActionFrame, RemoteActionMessage};
use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::FILE_SECRETS_ENV;
use tilo_server_protocol::hosted::Material;
use tilo_session::apps::Apps;
use tilo_session::hosted_browser::HostedBrowser;
use tilo_session::{AppActions, AppSocket, Launches, Options, Session};
use tilo_user_identity::DeviceCert;

use crate::common::people::{Person, Site};
use collab_types::WorkspaceId;

/// The app whose window a browser environment holds.
const BROWSER: &str = "so.tilo.browser";
/// The record the window opens on: the Browser record the server placed.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000aa";
/// The page a Browser window publishes its actions on, as the server names it.
const PAGE: &str = "browser.page";

/// What the session said, in order: every report line it printed.
type Said = Arc<Mutex<Vec<String>>>;
/// The provider the workspace's dispatch lists Browser's window under, once it lists one.
type Named = Arc<Mutex<Option<u64>>>;

/// The material the server hands a browser environment's session: the workspace `workspace` of
/// `site`, the record to open, and the device `device` certifies.
fn material(
    site: &Site,
    workspace: &str,
    workspace_id: WorkspaceId,
    record: &str,
    person: &Person,
) -> (String, Handed) {
    let root = tempfile::tempdir().expect("a directory for the handed device's identity");
    std::fs::create_dir_all(root.path()).expect("the handed device's identity root");
    let paths = IdentityPaths::under(root.path());
    let device = person.certify(&paths, "Browser");
    let key =
        tilo_device_identity::persist::load_device_key(&paths).expect("the handed device's key");
    let opens = record.to_owned();
    let material = Material {
        workspace: workspace.to_owned(),
        workspace_id,
        record: opens.clone(),
        device: device.clone(),
        secret: key.to_bytes(),
        server: site.node_addr(),
        user_public_key: person.public,
        account: "acct_ada".to_owned(),
    };
    (
        serde_json::to_string(&material).expect("the material is the server's"),
        Handed {
            root,
            key,
            cert: device,
            opens,
        },
    )
}

/// The handed device: its identity root, kept as long as the material it was read from, the key
/// and certificate an app the session starts presents as that device's, and what its window
/// opens on.
struct Handed {
    #[expect(dead_code, reason = "the directory lives as long as the test")]
    root: tempfile::TempDir,
    key: SigningKey,
    cert: DeviceCert,
    opens: String,
}

/// A session in a browser environment, its apps starting through `launcher`, keeping what it holds
/// under `state`, with the workspace directory it read from `home`. It serves its apps on
/// `app_wire`, as the session in a workspace's machine serves the Browser it starts, and the
/// Browser that speaks there in `workspace` is this test's own process.
fn environment(
    home: &tilo_test_env::TestHome,
    state: &Path,
    launcher: &Path,
    material: &str,
    (app_wire, workspace): (&Path, &str),
) -> (Session, Said) {
    let said: Said = Arc::default();
    let reports = Arc::clone(&said);
    let hosted = HostedBrowser::take(
        material,
        Box::new(move |report| {
            reports
                .lock()
                .expect("the reports are this test's")
                .push(report.line());
        }),
    )
    .expect("the environment's session");
    let mut launch = tilo_app_launch::SessionEnvironment::from_process();
    launch.wayland_socket = Some(home.runtime().join("wayland-7"));
    let apps = Rc::new(Apps::with_launcher(launch, None, Some(launcher.to_owned())));
    // Browser's side of the window speaks from this test's own process, so the launch the session
    // keeps for the Browser it starts names that process too: what its broker admits the
    // connection by.
    let begun = Arc::new(Launches::default());
    begun.started(std::process::id(), BROWSER, workspace, true);
    let session = crate::common::headless(Options {
        collab: true,
        state: Some(state.to_path_buf()),
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(apps),
        hosted_browser: Some(hosted),
        app_socket: AppSocket::At(app_wire.to_path_buf()),
        launches: begun,
        ..Options::default()
    });
    (session, said)
}

/// The one action Browser's window publishes of its page here: what the session's broker lists
/// the window by.
fn page_action() -> ExportedAction {
    use tilo_actions::{ActionContract, ActionKind, OperationKind};
    ExportedAction {
        resource: PAGE.into(),
        semantic_identity: None,
        action: "navigate".into(),
        resource_kind: PAGE.into(),
        resource_label: "Page".into(),
        action_label: "Navigate".into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        state: tilo_actions::ActionState::default(),
        operation_kind: OperationKind::Effect,
        params: Vec::new(),
        contract: ActionContract {
            kind: ActionKind::Outside,
            ..ActionContract::default()
        },
        view: None,
        input: ExportedInput::None,
        did: None,
    }
}

/// Browser's side of its window: the connection the launcher hands the app the session started,
/// joined on `document` as the device the session is, publishing its page. It then lists the
/// workspace's actions through the dispatch as any app does, and puts in `named` the provider the
/// listing names the window under: what a caller names the window by. The connection stays for as long as
/// the session serves it, as an open window's does.
fn browser(
    app_wire: &Path,
    (workspace, document): (&str, &str),
    named: &Named,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let mut stream =
            std::os::unix::net::UnixStream::connect(app_wire).expect("the app wire's socket");
        launcher::services::write_collab_handoff(&mut stream, BROWSER, workspace)
            .expect("the launcher names the app on the connection");
        stream
            .set_nonblocking(true)
            .expect("the connection is ready for the runtime");
        let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
        let (reader, writer) = stream.into_split();
        let (client, carrying) = CollabClient::over(reader, writer);
        tokio::spawn(carrying);
        let route = tilo_workspace_service::apps::route::CrdtRoute {
            app_id: BROWSER.into(),
            cluster_id: workspace.into(),
            document_id: document.into(),
        };
        let window = crate::common::held::opened(&client, &route.channel_key()).await
            .expect("the Browser the session started joins where its window opens");
        window
            .publish_actions(&[page_action()])
            .await
            .expect("Browser publishes its page");
        let listing = RemoteActionFrame::new(RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: CatalogueQuery {
                app_id: Some(BROWSER.into()),
                ..CatalogueQuery::default()
            },
        })
        .encode()
        .expect("a listing encodes");
        loop {
            let answer = window
                .remote_action(&listing)
                .await
                .expect("the dispatch lists the workspace's actions");
            let answer = RemoteActionFrame::decode(&answer).expect("a listing's answer decodes");
            if let RemoteActionMessage::CatalogueResponse {
                result: Ok(page), ..
            } = answer.message
                && let Some(provider) = page
                    .rows
                    .iter()
                    .find(|row| {
                        row.app_id == BROWSER && row.resource.id == PAGE && row.action.is_some()
                    })
                    .and_then(|row| row.provider.parse::<u64>().ok())
            {
                let mut first = named.lock().expect("the provider is this test's");
                if first.is_none() {
                    *first = Some(provider);
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
}

/// Whether the shell holds `workspace`.
fn holds(session: &Session, workspace: &str) -> bool {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .any(|held| held.id().to_string() == workspace)
    })
}

/// The tab `app`'s window takes in `workspace`, once the shell holds one.
fn tab_of(session: &Session, workspace: &str, app: &str) -> Option<u64> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .find(|held| held.id().to_string() == workspace)
            .and_then(|held| {
                held.strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .find(|tab| {
                        tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                            .is_some_and(|shown| shown == app)
                    })
            })
            .map(|tab| tab.id)
    })
}

/// What the session said so far.
fn reports(said: &Said) -> Vec<String> {
    said.lock().expect("the reports are this test's").clone()
}

#[test]
fn a_session_in_a_browser_environment_joins_its_workspace_and_opens_browser() {
    let person = Person::new("Ada");
    let site = Site::start(&person, "Browser");
    let workspace_id = site
        .workspace_id(&site.workspace)
        .expect("the site serves the workspace the material names");
    let (handed, device) = material(&site, &site.workspace, workspace_id, RECORD, &person);

    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    crate::common::probe_app(&mut home, BROWSER, "Browser");
    let launcher = tilo_test_env::stand_in(home.home().join("tilo-launcher"), "hold\n")
        .expect("the environment's launcher");
    let identity_root = home.data().join("identity");
    std::fs::create_dir_all(&identity_root).expect("the environment's identity root");
    home.env().set("TILO_IDENTITY_ROOT", &identity_root);
    home.env().set(FILE_SECRETS_ENV, "1");
    let state = tempfile::tempdir().expect("a directory for the environment's shell");
    let private = crate::common::private_home();
    let app_wire = private.path().join("app_wire.sock");

    let (mut session, said) = environment(
        &home,
        state.path(),
        launcher.path(),
        &handed,
        (&app_wire, &site.workspace),
    );
    session.step(Duration::from_millis(1)).expect("a frame");
    assert_eq!(
        reports(&said).first().map(String::as_str),
        Some(READY),
        "the identity it was handed is its own before anything else happens"
    );

    // It asks the server to enter it, redeems the invitation, and holds the workspace as the shell
    // holds any device's.
    crate::common::until(&mut session, "the workspace is held", |session| {
        holds(session, &site.workspace)
    });

    // Browser opens on the record, and its window takes a tab in the workspace.
    let mut client = crate::common::window::Client::connect(session.compositor());
    crate::common::window::pump(&mut session, &mut [&mut client], |_, clients| {
        let seen = &clients[0].seen;
        seen.compositor.is_some() && seen.wm_base.is_some() && seen.shm.is_some()
    });
    client.map_plain(&mut session, BROWSER, (600, 400));
    crate::common::until(&mut session, "the window takes its tab", |session| {
        tab_of(session, &site.workspace, BROWSER).is_some()
    });
    // A window on the screen is not yet one a caller reaches: the session's broker lists nothing
    // under it until the app publishes there, so the session has no provider to report.
    assert_eq!(
        reports(&said),
        vec![READY.to_owned()],
        "a window its broker lists nothing for is not reported as up"
    );

    // The Browser the session started joins as the handed device and publishes its page: the
    // session's broker lists the window, and the workspace's dispatch lists its provider.
    let named = Named::default();
    {
        let (workspace, document, named) =
            (site.workspace.clone(), device.opens, Arc::clone(&named));
        std::thread::spawn(move || {
            browser(&app_wire, (&workspace, &document), &named);
        });
    }
    let provider = crate::common::wait_for(
        &mut session,
        "the session's broker lists the window",
        |_| *named.lock().expect("the provider is this test's"),
    );
    crate::common::until(&mut session, "the provider is reported", |_| {
        reports(&said)
            .iter()
            .any(|line| line == &format!("up {provider}"))
    });
    assert_eq!(
        reports(&said),
        vec![READY.to_owned(), format!("up {provider}")],
        "`ready` first, then the provider the broker lists the window under, and nothing else"
    );
}

#[test]
fn a_session_whose_workspace_the_server_does_not_let_it_into_opens_nothing() {
    let person = Person::new("Ada");
    let site = Site::start(&person, "Browser");
    // Another workspace of the site's person, which this environment is not placed in: the server
    // refuses the entry, so no window is ever opened on the record the material names.
    let other = "11111111-2222-4333-8444-555555555555";
    let (handed, _root) = material(
        &site,
        other,
        tilo_desktop_model::WorkspaceId::from_u128(77),
        RECORD,
        &person,
    );

    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    crate::common::probe_app(&mut home, BROWSER, "Browser");
    let launcher = tilo_test_env::stand_in(home.home().join("tilo-launcher"), "hold\n")
        .expect("the environment's launcher");
    let identity_root = home.data().join("identity");
    std::fs::create_dir_all(&identity_root).expect("the environment's identity root");
    home.env().set("TILO_IDENTITY_ROOT", &identity_root);
    home.env().set(FILE_SECRETS_ENV, "1");
    let state = tempfile::tempdir().expect("a directory for the environment's shell");
    let private = crate::common::private_home();
    let app_wire = private.path().join("app_wire.sock");

    let (mut session, said) = environment(
        &home,
        state.path(),
        launcher.path(),
        &handed,
        (&app_wire, other),
    );
    let joined = crate::common::steps_within(&mut session, Duration::from_secs(15), |session| {
        holds(session, other).then_some(())
    });
    assert!(
        joined.is_none(),
        "the workspace it was handed is not one the server lets it into"
    );
    assert!(
        crate::common::steps_within(&mut session, Duration::from_secs(15), |_| {
            reports(&said)
                .iter()
                .any(|line| line.starts_with("up "))
                .then_some(())
        })
        .is_none(),
        "no window was opened on a record of a workspace it does not hold: {:?}",
        reports(&said)
    );
}
