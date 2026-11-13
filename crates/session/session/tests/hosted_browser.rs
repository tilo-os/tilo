//! A session in a browser environment: the relay hands it a workspace, a record and a device, and
//! it joins as that device, opens Browser on the record, and says what it became
//! (`docs/hosted-browser.md`, "What the session in the VM is handed").
//!
//! The environment's home is fresh every boot, so the identity the relay certifies is the only one
//! this session has, and the join is the relay's own: it asks the relay to enter it into the
//! workspace, redeems the invitation the relay answers with, and holds the workspace as any device
//! does. The window is a package's, mapped by a client of this test's own as a toolkit's window
//! maps: the session opens Browser through the launcher the way it opens any app.

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

use tilo_actions::remote::browser::READY;
use tilo_identity_host::provisioning::FILE_SECRETS_ENV;
use tilo_relay_protocol::hosted::Material;
use tilo_session::apps::Apps;
use tilo_session::hosted_browser::HostedBrowser;
use tilo_session::{AppActions, Options, Session};
use tilo_user_identity::IdentityPaths;

use crate::common::people::{Person, Site};
use collab_types::WorkspaceId;

/// The app whose window a browser environment holds.
const BROWSER: &str = "so.tilo.browser";
/// The record the window opens on: the Browser record the relay placed.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000aa";

/// What the session said, in order: every report line it printed.
type Said = Arc<Mutex<Vec<String>>>;

/// The material the relay hands a browser environment's session: the workspace `workspace` of
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
        tilo_user_identity::persist::load_device_key(&paths).expect("the handed device's key");
    let material = Material {
        workspace: workspace.to_owned(),
        workspace_id,
        record: record.to_owned(),
        device,
        secret: key.to_bytes(),
        relay: site.node_addr(),
        user_public_key: person.public,
        account: "acct_ada".to_owned(),
    };
    (
        serde_json::to_string(&material).expect("the material is the relay's"),
        Handed(root),
    )
}

/// The handed device's identity root, kept as long as the material it was read from.
struct Handed(
    #[expect(dead_code, reason = "the directory lives as long as the test")] tempfile::TempDir,
);

/// A session in a browser environment, its apps starting through `launcher`, keeping what it holds
/// under `state`, with the workspace directory it read from `home`.
fn environment(
    home: &tilo_test_env::TestHome,
    state: &Path,
    launcher: &Path,
    material: &str,
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
    let session = crate::common::headless(Options {
        collab: true,
        state: Some(state.to_path_buf()),
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(apps),
        hosted_browser: Some(hosted),
        ..Options::default()
    });
    (session, said)
}

/// Whether the desk holds `workspace`.
fn holds(session: &Session, workspace: &str) -> bool {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .any(|held| held.id().to_string() == workspace)
    })
}

/// The tab `app`'s window takes in `workspace`, once the desk holds one.
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
    let (handed, _root) = material(&site, &site.workspace, workspace_id, RECORD, &person);

    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    crate::common::probe_app(&mut home, BROWSER, "Browser");
    let launcher = tilo_test_env::stand_in(home.home().join("tilo-launcher"), "hold\n")
        .expect("the environment's launcher");
    let identity_root = home.data().join("identity");
    std::fs::create_dir_all(&identity_root).expect("the environment's identity root");
    home.env().set("TILO_IDENTITY_ROOT", &identity_root);
    home.env().set(FILE_SECRETS_ENV, "1");
    let state = tempfile::tempdir().expect("a directory for the environment's desk");

    let (mut session, said) = environment(&home, state.path(), launcher.path(), &handed);
    session.step(Duration::from_millis(1)).expect("a frame");
    assert_eq!(
        reports(&said).first().map(String::as_str),
        Some(READY),
        "the identity it was handed is its own before anything else happens"
    );

    // It asks the relay to enter it, redeems the invitation, and holds the workspace as the desk
    // holds any device's.
    crate::common::until(&mut session, "the workspace is held", |session| {
        holds(session, &site.workspace)
    });

    // Browser opens on the record, and its window publishes under the tab it takes.
    let mut client = crate::common::window::Client::connect(session.compositor());
    crate::common::window::pump(&mut session, &mut [&mut client], |_, clients| {
        let seen = &clients[0].seen;
        seen.compositor.is_some() && seen.wm_base.is_some() && seen.shm.is_some()
    });
    client.map_plain(&mut session, BROWSER, (600, 400));
    let provider = crate::common::wait_for(&mut session, "the window publishes", |session| {
        tab_of(session, &site.workspace, BROWSER)
    });
    crate::common::until(&mut session, "the provider is reported", |_| {
        reports(&said)
            .iter()
            .any(|line| line == &format!("up {provider}"))
    });
    assert_eq!(
        reports(&said),
        vec![READY.to_owned(), format!("up {provider}")],
        "`ready` first, then the window's provider, and nothing else"
    );
}

#[test]
fn a_session_whose_workspace_the_relay_does_not_let_it_into_opens_nothing() {
    let person = Person::new("Ada");
    let site = Site::start(&person, "Browser");
    // Another workspace of the site's person, which this environment is not placed in: the relay
    // refuses the entry, so no window is ever opened on the record the material names.
    let other = "11111111-2222-4333-8444-555555555555";
    let (handed, _root) = material(&site, other, 77, RECORD, &person);

    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    crate::common::probe_app(&mut home, BROWSER, "Browser");
    let launcher = tilo_test_env::stand_in(home.home().join("tilo-launcher"), "hold\n")
        .expect("the environment's launcher");
    let identity_root = home.data().join("identity");
    std::fs::create_dir_all(&identity_root).expect("the environment's identity root");
    home.env().set("TILO_IDENTITY_ROOT", &identity_root);
    home.env().set(FILE_SECRETS_ENV, "1");
    let state = tempfile::tempdir().expect("a directory for the environment's desk");

    let (mut session, said) = environment(&home, state.path(), launcher.path(), &handed);
    let joined = crate::common::steps_within(&mut session, Duration::from_secs(15), |session| {
        holds(session, other).then_some(())
    });
    assert!(
        joined.is_none(),
        "the workspace it was handed is not one the relay lets it into"
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
