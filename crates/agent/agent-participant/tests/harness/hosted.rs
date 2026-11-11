//! The agent host the scenario tests drive: a `tilo-relay` whose app-serving
//! plane is the agent supervisor, its state under a scratch directory, and
//! the laptop that certifies the agents and answers what they ask.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a fixture: what it cannot set up is the failure it reports"
)]

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_agent_participant::{AgentHost, AppCatalogue, FeedbackReviewer, ModelChoice};
use tilo_relay::certify::{ApproverDecision, ApproverReply, ConfirmationAnswer};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_relay_client::Approver;
use tilo_relay_protocol::certify::PendingConfirmation;
use tilo_user_identity::{CertificationPolicy, RevocationList, issue_device_cert};

use crate::support::Device;

/// A host serving its workspaces, with the agent supervisor as its
/// app-serving plane.
pub struct Hosted {
    pub relay: Relay,
    pub web: WebHandle,
    pub agents: AgentHost,
    /// The host's own device key, whose records the workspace holds too.
    pub device: [u8; 32],
}

/// What a host is set up with: the model its agents plan with, the catalogue
/// they are offered, and the page it serves.
pub struct Options {
    pub model: ModelChoice,
    pub catalogue: Option<Arc<dyn Fn() -> AppCatalogue + Send + Sync>>,
    /// The page's entry point, when the host serves one.
    pub web_entry: Option<String>,
    /// The origins a page may speak from.
    pub allowed_origins: Vec<String>,
    /// The reviewer a host lends its workspaces' reporters.
    pub feedback_reviewer: Option<Arc<dyn FeedbackReviewer>>,
    /// Where the host runs its workspaces' Terminal sessions.
    pub shell: Option<Arc<tilo_relay::agents::HostShells>>,
    /// Serve the web routes as plain HTTP on loopback too, as a client that
    /// trusts no development certificate reaches them.
    pub plain: bool,
    /// The per-person change and content budgets of a host whose workspaces
    /// admit the public; `None` limits nobody.
    pub member_limits: Option<tilo_relay::member_limits::MemberLimitsConfig>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            model: ModelChoice::Scripted,
            catalogue: None,
            web_entry: None,
            allowed_origins: Vec::new(),
            feedback_reviewer: None,
            shell: None,
            plain: false,
            member_limits: None,
        }
    }
}

impl Options {
    /// Agents offered `catalogue` and planning with `model`.
    pub fn offering(
        catalogue: impl Fn() -> AppCatalogue + Send + Sync + 'static,
        model: ModelChoice,
    ) -> Self {
        Self {
            model,
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
    let cfg = tilo_relay::RelayConfig {
        state_root: state.join("relay"),
        web_entry: options.web_entry,
        member_limits: options.member_limits,
        ..Default::default()
    };
    let (relay, agents) = tilo_relay::agents::start(
        cfg,
        RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host.key.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
        tilo_relay::agents::AgentOptions {
            state_dir: state.join("agents"),
            model: options.model,
            catalogue: options.catalogue,
            feedback_reviewer: options.feedback_reviewer,
            shell: options.shell,
            browsers: None,
        },
    )
    .await
    .unwrap();
    let web = relay
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
            door: None,
        })
        .await
        .unwrap();
    Hosted {
        relay,
        web,
        agents,
        device: host.cert.device_public_key,
    }
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

    /// Spawn it against `relay`.
    pub fn spawn(
        self,
        relay: &Relay,
        root: SigningKey,
        laptop: &Device,
    ) -> tokio::task::JoinHandle<()> {
        let addr = relay.node_addr();
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
