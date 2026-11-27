//! A session in a workspace's VM: one of the VM daemon's shells, which asks its daemon what was
//! launched for it, opens Browser on the record the machine launched, and tells the daemon the
//! window is up under the provider the workspace's dispatch lists it by
//! (`docs/hosted-browser.md`). The daemon is stood in for here by a provider of the test's own
//! that answers its `environment` actions as the VM's daemon does, so the test reads what the
//! session asked of it; the daemon's own side is tilo-runtime's `device::vm_environment`.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use tilo_action_services::environment_window::{self as wire, Holds, Launch, Report};
use tilo_actions::remote::TestProvider;
use tilo_actions::remote::{CataloguePage, CatalogueProvider, InvokeArgument, InvokeRequest};
use tilo_desktop_model::{Workspace, WorkspaceId};
use tilo_session::apps::Apps;
use tilo_session::hosted_browser::HostedBrowser;
use tilo_session::{AppActions, Options};

/// The app whose window the session holds.
const BROWSER: &str = "so.tilo.browser";
/// The record the machine launches Browser on.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000aa";
/// The provider the workspace's dispatch lists Browser's window under.
const WINDOW: &str = "42";

/// What the session told its daemon, in order.
fn told(daemon: &TestProvider) -> Vec<Report> {
    daemon
        .asked()
        .iter()
        .filter(|request| request.target.address.action == wire::REPORT)
        .filter_map(|request| match &request.argument {
            InvokeArgument::Json(named) => serde_json::from_value(named.clone().into()).ok(),
            _ => None,
        })
        .collect()
}

/// The VM's daemon, stood in for: it answers the session's first ask for launches with Browser on
/// [`RECORD`] in `workspace`, and every later one with none; it lists Browser's window in
/// `workspace` under [`WINDOW`]; and it keeps what it was told.
fn daemon(workspace: &str) -> Arc<TestProvider> {
    let launched = Mutex::new(false);
    let listed = workspace.to_owned();
    let workspace = workspace.to_owned();
    Arc::new(
        TestProvider::new()
            .with_catalogue(move |_, _| {
                Ok(CataloguePage {
                    providers: vec![CatalogueProvider {
                        provider: WINDOW.into(),
                        app_id: BROWSER.into(),
                        workspace: listed.clone(),
                        document_id: RECORD.into(),
                        browsable: false,
                    }],
                    ..CataloguePage::default()
                })
            })
            .completing(move |request: &InvokeRequest| {
                let first = request.target.address.action == wire::NEXT_LAUNCH
                    && !std::mem::replace(&mut *launched.lock().unwrap(), true);
                let requested = if first {
                    vec![Launch {
                        workspace: workspace.clone(),
                        holds: Holds::Browser,
                        record: Some(RECORD.into()),
                        app: None,
                    }]
                } else {
                    Vec::new()
                };
                tilo_actions::ActionOutput {
                    value: Some(serde_json::to_value(requested).unwrap()),
                    ..tilo_actions::ActionOutput::default()
                }
            }),
    )
}

#[test]
fn a_session_the_machine_launches_browser_in_opens_it_and_says_it_is_up() {
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    crate::common::probe_app(&mut home, BROWSER, "Browser");
    let launcher = tilo_test_env::stand_in(home.home().join("tilo-launcher"), "hold\n")
        .expect("the VM's launcher");
    let state = crate::common::device_in(&mut home);
    // The workspace the VM serves, which its daemon is a device of.
    let served = WorkspaceId::new();
    let workspace = served.to_string();

    let daemon = daemon(&workspace);
    let hosted =
        HostedBrowser::new(Holds::Browser, &workspace).asking(Arc::clone(&daemon) as _, "person");
    let mut launch = tilo_app_launch::SessionEnvironment::from_process();
    launch.wayland_socket = Some(home.runtime().join("wayland-7"));
    let apps = Rc::new(Apps::with_launcher(
        launch,
        None,
        Some(launcher.path().to_owned()),
    ));
    let mut session = crate::common::headless(Options {
        collab: false,
        state: Some(state),
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(apps),
        hosted_browser: Some(hosted),
        ..Options::default()
    });

    // The VM's shell comes to hold the workspace as it holds any of its device's: the session
    // opens nothing of it until then.
    let mut layout = session.handle().read(Clone::clone);
    layout
        .workspaces
        .push(Workspace::new(served, "Browser".to_owned()));
    session.handle().replace_tree(layout);
    crate::common::until(&mut session, "the window is told up", |_| {
        !told(&daemon).is_empty()
    });
    assert_eq!(
        told(&daemon),
        [Report::Up {
            holds: Holds::Browser,
            provider: WINDOW.into(),
        }],
        "Browser opened on the record, and its window is up under the provider the dispatch lists"
    );
    let asked = daemon.asked();
    assert!(
        asked
            .iter()
            .any(|request| request.target.address.action == wire::NEXT_LAUNCH),
        "the session asked its daemon what was launched for it"
    );
    assert!(
        daemon
            .callers()
            .iter()
            .all(|caller| caller.workspace() == Some(workspace.as_str())),
        "and asked only in the workspace it serves"
    );
}
