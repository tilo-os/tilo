//! Browser in a workspace the relay hosts: the workspace lists it like any
//! other app, an agent's key opens it through the workspace's own actions,
//! and opening it places the environment whose session holds the window
//! (`docs/hosted-browser.md`).
//!
//! The relay mounts nothing of Browser: its window runs in the workspace's
//! VM. What the open makes is the workspace's Browser record, which the
//! session in the VM opens and draws.
use super::*;
use serde_json::json;
use std::sync::Mutex;
use tilo_actions::remote::host_account::Refusal;
use tilo_actions::remote::{CataloguePage, CatalogueQuery};
use tilo_relay::hosted_browser::{
    APP, BrowserMeter, BrowserSessions, HostedBrowsers, TITLE, Window,
};
use tilo_relay_protocol::hosted::Material;
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_session_core::remote::SessionActions;

/// The provider a fake environment's window publishes under.
const WINDOW: &str = "browser@7";

/// Places environments in-process: each starts with the material the relay
/// hands it, and answers the window the relay routes page actions to.
#[derive(Clone, Default)]
struct Environments(Arc<Mutex<Vec<Material>>>);

impl Environments {
    fn started(&self) -> Vec<Material> {
        self.0.lock().unwrap().clone()
    }
}

impl BrowserSessions for Environments {
    fn start(&self, material: &Material) -> Result<Window, String> {
        self.0.lock().unwrap().push(material.clone());
        Ok(Window {
            device: material.device.device_public_key,
            provider: WINDOW.into(),
        })
    }

    fn stop(&self, _: &str) {}
}

/// Counts the page actions a window ran, and admits them all.
#[derive(Default)]
struct Meter {
    counted: Mutex<usize>,
}

impl BrowserMeter for Meter {
    fn admit(&self, _: &str) -> Result<(), Refusal> {
        Ok(())
    }

    fn record(&self, _: &str, _: &str) {
        let mut counted = self.counted.lock().unwrap();
        *counted = counted.saturating_add(1);
    }
}

/// The workspace's row for the app `app`, as an agent's session lists it:
/// the name people read it by, and the label of the action it offers.
fn app_row(listing: &CataloguePage, app: &str) -> (String, Option<String>) {
    let row = listing
        .rows
        .iter()
        .find(|row| row.resource.id == format!("app.{app}"))
        .unwrap_or_else(|| panic!("the workspace lists {app}: {:?}", listing.rows));
    (
        row.resource.label.clone(),
        row.action
            .as_ref()
            .map(|action| action.action_label.clone()),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_key_opens_browser_and_the_workspace_lists_it_like_any_app() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo.sh", DeviceCapabilities::host());
    let computer = enrol(&root, "visa agent", DeviceCapabilities::member());
    let state = support::scratch_dir();
    // The relay makes itself the person's stand-in from the root it keeps, as
    // a host that places browser sessions does at start.
    tilo_relay::hosted_browser::keep_stand_in(&state.path().join("relay"), &root)
        .expect("the root signs the host's stand-in");
    let environments = Environments::default();
    let browsers = Arc::new(HostedBrowsers::new(
        Box::new(environments.clone()),
        Box::new(Meter::default()),
        "acct_ada".into(),
    ));
    let hosted = {
        // A session in the workspace's VM reaches the relay at this address.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(
            tilo_relay::hosted_browser::RELAY_ENV,
            format!(
                "{}@127.0.0.1:7704",
                hex::encode(host.cert.device_public_key)
            ),
        );
        harness::hosted::start(
            &root,
            &host,
            state.path(),
            Options {
                browsers: Some(Arc::clone(&browsers)),
                ..Options::offering(common::catalogue, ModelChoice::Scripted)
            },
        )
        .await
    };
    let _laptop = participant::Participant::start(&hosted.relay, &root).await;
    let mut agent = Browser::open(&hosted, &computer, &host).await;
    let _ =
        tilo_session_client::device::run_as_agent(&agent.peer, hosted_documents::RUN_LABEL).await;
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_surface(Surface::Agent)
    .with_agent_run(hosted_documents::RUN, hosted_documents::RUN_LABEL);

    // The workspace lists Browser like any app: an agent names it `browser`.
    let listing = agent
        .catalogue(CatalogueQuery::default())
        .await
        .expect("the workspace lists its apps");
    let (label, action) = app_row(&listing, APP);
    assert_eq!(label, TITLE, "under the name people read it by");
    assert_eq!(action.as_deref(), Some("Open Browser"));
    assert_eq!(
        APP.rsplit('.').next(),
        Some("browser"),
        "the name an agent addresses it by"
    );

    // `browser#open`, as the SDK asks it of the workspace: the open makes the
    // workspace's Browser record and places the environment on it, once.
    let (answer, _) = hosted_documents::act(&server, "browser#open", json!({})).await;
    eprintln!("browser#open answered {answer}");
    let started = environments.started();
    assert_eq!(started.len(), 1, "the window's environment was opened");
    let record = started[0].record.clone();
    assert!(
        ui::record::RecordId::parse(&record).is_some(),
        "the open made the record it hands the session ({record})"
    );
    assert_eq!(started[0].workspace, WORKSPACE);
    assert_eq!(
        browsers
            .window(tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap())
            .map(|window| window.provider),
        Some(WINDOW.into()),
        "the workspace has the window"
    );

    // A second open attaches to the window already up, on the same record.
    let (again, _) = hosted_documents::act(&server, "browser#open", json!({})).await;
    eprintln!("a second browser#open answered {again}");
    assert_eq!(
        environments.started()[0].record,
        record,
        "the workspace's one Browser record"
    );
    assert_eq!(environments.started().len(), 1, "no second environment");
}
