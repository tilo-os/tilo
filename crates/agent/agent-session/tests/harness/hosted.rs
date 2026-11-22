//! The agent host the scenario tests drive: a `tilo-server` whose app-serving
//! app action server is the agent supervisor, its state under a scratch directory, and
//! the laptop that certifies the agents and answers what they ask.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_agent_session::{AgentService, AppCatalogue};
use tilo_server::certify::{ApproverDecision, ApproverReply, ConfirmationAnswer};
use tilo_server::daemon::{Server, ServerIdentity};
use tilo_server::web::{TlsSource, WebConfig, WebHandle};
use tilo_server_client::Approver;
use tilo_server_protocol::certify::PendingConfirmation;
use tilo_user_identity::{CertificationPolicy, RevocationList, issue_device_cert};

use crate::support::Device;

/// A host serving its workspaces' documents, its roster agents' grants and its people's events.
pub struct Hosted {
    pub server: Server,
    pub web: WebHandle,
    pub agents: AgentService,
    pub roster: tilo_server::agents::RosterServices,
    /// The host's own device key, whose records the workspace holds too.
    pub device: [u8; 32],
}

/// What a host is set up with: the catalogue its workspace actions open records in, and the
/// page it serves.
#[derive(Default)]
pub struct Options {
    pub catalogue: Option<Arc<dyn Fn() -> AppCatalogue + Send + Sync>>,
    /// The page's entry point, when the host serves one.
    pub web_entry: Option<String>,
    /// The origins a page may speak from.
    pub allowed_origins: Vec<String>,
    /// Where the host runs its workspaces' Terminal sessions.
    pub shell: Option<Arc<tilo_server::agents::HostShells>>,
    /// The Browser this host places for its workspaces.
    pub browsers: Option<Arc<tilo_server::hosted_browser::HostedBrowsers>>,
    /// Serve the web routes as plain HTTP on loopback too, as a client that
    /// trusts no development certificate reaches them.
    pub plain: bool,
}

impl Options {
    /// A host offering `catalogue`.
    pub fn offering(catalogue: impl Fn() -> AppCatalogue + Send + Sync + 'static) -> Self {
        Self {
            catalogue: Some(Arc::new(catalogue)),
            ..Self::default()
        }
    }

    /// A host whose page is served at `origin`.
    pub fn serving(mut self, origin: &str) -> Self {
        self.web_entry = Some(format!("{origin}/"));
        self.allowed_origins = vec![origin.to_owned()];
        self
    }
}

/// Start a host for `root`'s person on `host`'s device.
pub async fn start(root: &SigningKey, host: &Device, state: &Path, options: Options) -> Hosted {
    let cfg = tilo_server::ServerConfig {
        state_root: state.join("server"),
        web_entry: options.web_entry,
        // The host's own packet relay, on this machine: devices reach it here, as
        // the test's devices do, and no test depends on the public relays.
        packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
        ..Default::default()
    };
    let identity = ServerIdentity {
        user_public_key: root.verifying_key().to_bytes(),
        device_signing_key: host.key.clone(),
        device_cert: host.cert.clone(),
        revocations: RevocationList::new(),
    };
    let agents = tilo_server::agents::AgentOptions {
        state_dir: state.join("agents"),
        catalogue: options.catalogue,
        shell: options.shell,
        browsers: options.browsers,
    };
    let (server, agents, roster) =
        tilo_server::agents::start_workspace_documents(cfg, identity, agents)
            .await
            .unwrap();
    assert!(roster.events.is_some());
    assert!(roster.initiative.is_some());
    let web = server
        .serve_web(WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: TlsSource::Development {
                hosts: vec!["localhost".into(), "127.0.0.1".into()],
                store: Some(state.join("web-dev-identity")),
            },
            allowed_origins: options.allowed_origins,
            plain_listen: options.plain.then(|| "127.0.0.1:0".parse().unwrap()),
            public_origin: None,
            fetch_proxy: None,
            wait_list: Arc::default(),
            link: None,
            site: None,
        })
        .await
        .unwrap();
    Hosted {
        server,
        web,
        agents,
        roster,
        device: host.cert.device_public_key,
    }
}

/// [`serving_documents`], with the action service it serves the records with: what the agents the
/// host runs for its workspaces act through.
pub async fn serving_documents_with_actions(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    catalogue: impl Fn() -> AppCatalogue + Send + Sync + 'static,
) -> (
    Server,
    tilo_agent_session::workspace_actions::WorkspaceActions,
) {
    let cfg = tilo_server::ServerConfig {
        state_root: state.join("server"),
        packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
        ..Default::default()
    };
    tilo_server::agents::start_serving_documents(
        cfg,
        ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host.key.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
        Some(Arc::new(catalogue)),
        None,
        None,
    )
    .await
    .unwrap()
}

/// What a server that reaches nothing beyond this machine is set up with: its state under
/// `state`, and devices reaching it on loopback.
pub fn on_this_machine(state: &Path) -> tilo_server::ServerConfig {
    tilo_server::ServerConfig {
        state_root: state.join("server"),
        packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
        ..Default::default()
    }
}

/// Start the agents' service for `root`'s person on `server`'s device, with its documents served
/// to their devices, each action done in the document's own app worker here,
/// whether or not a page of the person's is open. Its apps are
/// `catalogue`'s.
pub async fn serving_documents(
    root: &SigningKey,
    server: &Device,
    state: &Path,
    catalogue: impl Fn() -> AppCatalogue + Send + Sync + 'static,
) -> Server {
    tilo_server::agents::start_serving_documents(
        on_this_machine(state),
        ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server.key.clone(),
            device_cert: server.cert.clone(),
            revocations: RevocationList::new(),
        },
        Some(Arc::new(catalogue)),
        None,
        None,
    )
    .await
    .unwrap()
    .0
}

/// The laptop: a root holder answering every agent request with the agent's
/// certificate and the delegation of exactly what was proposed.
///
/// A question an agent puts to its person is answered with `unlisted` unless
/// the agent is one of `confirming` (a scenario's `may_confirm`), which is
/// allowed once. `Unlisted::Pending` leaves every question for the test to
/// answer, and is what a scenario about the person's own answer uses.
pub struct Laptop {
    confirming: BTreeSet<String>,
    unlisted: Unlisted,
}

/// What the laptop does with a question from an agent it does not know.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Unlisted {
    /// Leave it pending.
    Pending,
    /// Refuse it.
    Deny,
}

impl Laptop {
    /// A laptop allowing the actions of the agents `confirming` names and
    /// treating the rest as `unlisted` says.
    pub fn new(confirming: impl IntoIterator<Item = String>, unlisted: Unlisted) -> Self {
        Self {
            confirming: confirming.into_iter().collect(),
            unlisted,
        }
    }

    /// Spawn it against `server`.
    pub fn spawn(
        self,
        server: &Server,
        root: SigningKey,
        laptop: &Device,
    ) -> tokio::task::JoinHandle<()> {
        let addr = server.node_addr();
        let cert = laptop.cert.clone();
        let key = laptop.key.clone();
        tokio::spawn(async move {
            let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .bind()
                .await
                .unwrap();
            let mut approver =
                Approver::connect(&endpoint, addr, cert, &key, &RevocationList::new())
                    .await
                    .unwrap();
            loop {
                let (Ok(proposals), Ok(pending_list)) = (
                    approver.list_agent_proposals().await,
                    approver.list_pending().await,
                ) else {
                    // The host closed: there is nothing left to answer.
                    return;
                };
                for pending in pending_list {
                    let Some(agent) = proposals
                        .iter()
                        .find(|agent| agent.request_digest == pending.request_digest)
                    else {
                        continue;
                    };
                    let now = tilo_clock::unix_now_ns();
                    let cert = issue_device_cert(
                        &pending.request,
                        &root,
                        &CertificationPolicy::agent(),
                        &RevocationList::new(),
                        now,
                    )
                    .unwrap();
                    let decided = approver
                        .decide(
                            pending.request_digest,
                            ApproverDecision::Issue(cert.clone()),
                        )
                        .await;
                    let Ok(decided) = decided else { return };
                    // A bare certificate is refused: an agent is approved with
                    // its delegation or not at all.
                    assert!(
                        matches!(decided, ApproverReply::Refused { .. }),
                        "an agent's bare certificate is not issued: {decided:?}"
                    );
                    let delegation = agent
                        .proposal
                        .sign(&root, pending.request.device_public_key, now)
                        .unwrap();
                    let decided = approver
                        .decide(
                            pending.request_digest,
                            ApproverDecision::IssueDelegate { cert, delegation },
                        )
                        .await;
                    let Ok(decided) = decided else { return };
                    assert_eq!(decided, ApproverReply::Decided);
                }
                if self.unlisted != Unlisted::Pending {
                    let Ok(questions) = approver.list_confirmations().await else {
                        return;
                    };
                    for question in questions {
                        let answer = self.answer(&question);
                        eprintln!(
                            "{} asked to {}: {answer:?}",
                            question.agent_label, question.action_label
                        );
                        let _ = approver.answer(question.id, answer).await;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    }

    fn answer(&self, question: &PendingConfirmation) -> ConfirmationAnswer {
        if self.confirming.contains(&question.agent_id)
            || self.confirming.contains(&question.agent_label)
        {
            return ConfirmationAnswer::AllowOnce;
        }
        match self.unlisted {
            Unlisted::Deny | Unlisted::Pending => ConfirmationAnswer::Deny,
        }
    }
}
