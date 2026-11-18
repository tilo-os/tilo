//! An agent filling a notebook over MCP, one `notes#create-page` after
//! another: what each create asks of the workspace's host stays the same
//! however many notes the notebook already holds. The action is found on the
//! notebook the app lists at its top, and the link to the new note is asked
//! for by its address; neither reads the notebook's notes, which would cost
//! more pages of its directory the more notes it holds.

use super::api::{ISSUER, Mcp};
use super::api_on_server::{api_on_server, approved_key};
use super::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::{Layer as _, filter::Targets};

/// The catalogue requests the workspace's host answered: each is one job of
/// kind `catalogue` its workspace actions log as they run it.
pub(super) static CATALOGUES: AtomicUsize = AtomicUsize::new(0);

/// The catalogue pages that listed a resource's items (a job under a parent,
/// naming nothing) rather than answering a name: what a lookup pays when it
/// pages a level to find one item.
pub(super) static LISTINGS: AtomicUsize = AtomicUsize::new(0);

/// Counts the server's `catalogue` jobs as it logs them.
pub(super) struct CatalogueJobs;

/// Count [`CATALOGUES`] and [`LISTINGS`] from here on: every test of this
/// binary installs the same counting, so one already installed serves them
/// all.
pub(super) fn count_jobs() {
    let counting = CatalogueJobs.with_filter(Targets::new().with_target(
        "tilo_agent_session::workspace_actions",
        tracing::Level::DEBUG,
    ));
    let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(counting));
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CatalogueJobs {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        #[derive(Default)]
        struct Kind {
            catalogue: bool,
            parent: String,
            named: String,
        }
        impl tracing::field::Visit for Kind {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                match field.name() {
                    "kind" if value == "catalogue" => self.catalogue = true,
                    "parent" => value.clone_into(&mut self.parent),
                    "named" => value.clone_into(&mut self.named),
                    _ => {}
                }
            }
            fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
        }
        let mut kind = Kind::default();
        event.record(&mut kind);
        if kind.catalogue {
            CATALOGUES.fetch_add(1, Ordering::Relaxed);
            if !kind.parent.is_empty() && kind.named.is_empty() {
                LISTINGS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// One more note, as an agent makes it, which answers a link to show it.
async fn create(agent: &mut Mcp<'_>, made: &mut usize) {
    let answer = agent
        .tilo_do(json!({"path": "notes#create-page",
                        "args": {"title": format!("Note {made}"),
                                 "markdown": "A paragraph of this note."}}))
        .await;
    let answer = &answer["result"]["structuredContent"];
    assert_eq!(answer["status"], "ok", "{answer}");
    assert!(
        answer["link"].is_string(),
        "a create answers a link: {answer}"
    );
    *made = made.saturating_add(1);
}

/// The most catalogue requests the host answered for any one of `creates`
/// more notes.
async fn per_create(agent: &mut Mcp<'_>, made: &mut usize, creates: usize) -> usize {
    let mut most = 0;
    for _ in 0..creates {
        let before = CATALOGUES.load(Ordering::Relaxed);
        create(agent, made).await;
        most = most.max(CATALOGUES.load(Ordering::Relaxed).saturating_sub(before));
    }
    most
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_create_asks_the_host_the_same_however_full_the_notebook() {
    count_jobs();
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_server, &ada, &page, "Notes mover").await;
    let mut backend = Mcp {
        api: &api,
        path: "/mcp".into(),
        token: key,
        session: None,
    };
    assert_eq!(backend.initialize().await.0, 200);
    let made = backend
        .did("account/workspaces#create", json!({"name": "Notes"}))
        .await;
    let workspace = made["workspace"]["id"].as_str().unwrap().to_owned();
    let minted = backend
        .did("account/keys#mint", json!({"workspace": workspace}))
        .await;
    let mut agent = Mcp {
        api: &api,
        path: format!("/w/{workspace}/mcp"),
        token: minted["token"].as_str().unwrap().to_owned(),
        session: None,
    };
    assert_eq!(agent.initialize().await.0, 200);
    agent.did("notes#open", json!({})).await;

    // A notebook of ten notes, then one of more than a directory read takes
    // in its four pages.
    let mut made = 0;
    while made < 10 {
        create(&mut agent, &mut made).await;
    }
    let few = per_create(&mut agent, &mut made, 3).await;
    while made < 100 {
        create(&mut agent, &mut made).await;
    }
    let many = per_create(&mut agent, &mut made, 3).await;
    assert!(
        many <= few,
        "a create in a notebook of 100 notes asked the host for {many} catalogue pages, \
         in one of 10 for {few}: it read the notebook"
    );
}
