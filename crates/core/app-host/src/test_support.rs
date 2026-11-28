//! Test-only mutable policy fixture. Production hosts implement `WorkspaceAuthority`.
use crate::services::WorkspaceAuthority;
use std::sync::Arc;
pub use tilo_action_policy::{Answer, GrantId, Grantee, Level, Role, TreePath};
pub use tilo_action_types::KindSet;
pub use tilo_desktop_model::{Workspace, WorkspaceId};

/// The grants fixtures give, in the workspace's own terms.
pub trait Grants {
    /// Let `subject` take actions of `kinds` at `relative` (a path below the
    /// workspace). Returns the grant's id.
    fn allow(&mut self, subject: Grantee, relative: &TreePath, kinds: KindSet) -> GrantId {
        self.answer(subject, relative, kinds, Answer::Allow)
    }
    /// Refuse `subject` actions of `kinds` at `relative`.
    fn deny(&mut self, subject: Grantee, relative: &TreePath, kinds: KindSet) -> GrantId {
        self.answer(subject, relative, kinds, Answer::Deny)
    }
    /// Give `subject` `answer` for actions of `kinds` at `relative`.
    fn answer(
        &mut self,
        subject: Grantee,
        relative: &TreePath,
        kinds: KindSet,
        answer: Answer,
    ) -> GrantId;
    /// Let `subject` take actions of `kinds` at the workspace `workspace`.
    fn allow_in_workspace(&mut self, subject: Grantee, workspace: &str, kinds: KindSet) -> GrantId;
    /// Let the agent `agent` take actions of `kinds` in the workspace.
    fn allow_agent(&mut self, agent: &str, kinds: KindSet) -> GrantId {
        self.allow(Grantee::agent(agent, "ada"), &TreePath::root(), kinds)
    }
    /// Let `subject` take actions of `kinds` anywhere in the app `app`.
    fn allow_app(&mut self, subject: Grantee, app: &str, kinds: KindSet) -> GrantId {
        self.allow(subject, &TreePath::root().app(app), kinds)
    }
}

impl Grants for Workspace {
    fn answer(
        &mut self,
        subject: Grantee,
        relative: &TreePath,
        kinds: KindSet,
        answer: Answer,
    ) -> GrantId {
        let by = giver(&self.policy, &subject);
        self.policy.grant(tilo_action_policy::Grant::new(
            format!("fixture:{}", uuid::Uuid::new_v4()),
            subject,
            self.policy.node().join(relative),
            kinds,
            answer,
            by,
        ))
    }

    fn allow_in_workspace(&mut self, subject: Grantee, workspace: &str, kinds: KindSet) -> GrantId {
        let by = giver(&self.policy, &subject);
        self.policy.grant(tilo_action_policy::Grant::new(
            format!("fixture:{}", uuid::Uuid::new_v4()),
            subject,
            TreePath::workspace(workspace),
            kinds,
            Answer::Allow,
            by,
        ))
    }
}
/// Who gives a fixture's grant: an agent holds only what its own person gave it, anyone else
/// what the workspace's owner gave.
fn giver(policy: &tilo_action_policy::WorkspacePolicy, subject: &Grantee) -> Grantee {
    let person = subject
        .as_agent()
        .map_or_else(|| policy.owner().unwrap_or_default(), |(_, of)| of.as_str());
    Grantee::person(person.to_owned())
}
#[derive(Clone, Default)]
pub struct WorkspaceTree {
    pub desktop: tilo_desktop_model::Layout,
    pub persist_path: Option<std::path::PathBuf>,
    /// The device revocations every workspace of the tree records.
    pub revoked: tilo_user_identity::RevocationList,
}
impl std::ops::Deref for WorkspaceTree {
    type Target = tilo_desktop_model::Layout;
    fn deref(&self) -> &Self::Target {
        &self.desktop
    }
}
impl std::ops::DerefMut for WorkspaceTree {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.desktop
    }
}
#[derive(Clone)]
pub struct WorkspaceTreeHandle(Arc<parking_lot::RwLock<WorkspaceTree>>);
impl std::ops::Deref for WorkspaceTreeHandle {
    type Target = parking_lot::RwLock<WorkspaceTree>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl WorkspaceTree {
    pub fn shared(self) -> WorkspaceTreeHandle {
        WorkspaceTreeHandle(Arc::new(parking_lot::RwLock::new(self)))
    }
}
/// Name what signs the evidence this process's fixtures keep — one synthetic device, served once
/// for the whole process — and open `workspaces`, the ones the fixture's host serves: a host
/// signs the receipts of the workspaces it opens, and no others.
#[allow(clippy::unwrap_used, reason = "a fixture failing is its test failing")]
pub fn serve_the_test_signer(workspaces: impl IntoIterator<Item = String>) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tilo_identity::sources::test_trust::install();
        let root = ed25519_dalek::SigningKey::from_bytes(&[0x91; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[0x92; 32]);
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Synthetic test device".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
        tilo_invocation_journal::serve_as(certificate, Arc::new(device));
    });
    for workspace in workspaces {
        tilo_invocation_journal::opened(&workspace);
    }
}

/// A fixture's own home: the config and identity directories its daemon reads, named in the
/// environment, and the device key its daemon took, let go of with it.
pub struct ProvisionedHome {
    /// The identity root and secrets file, on top of the home's own environment.
    _env: tilo_test_env::ScopedEnv,
    home: tilo_test_env::TestHome,
    _key: tilo_device_identity::persist::DeviceKeyLock,
}

impl ProvisionedHome {
    /// Provision this process a daemon's home and take its device key.
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a fixture failing is its test failing"
    )]
    pub fn new() -> Self {
        let home = tilo_test_env::TestHome::new().unwrap();
        let identity = home.root().join("identity");
        let config = home.config().to_owned();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_IDENTITY_ROOT", &identity)
            .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
        tilo_identity::sources::test_trust::install();
        let key = tilo_identity::provisioning::provisioned_as_its_daemon(
            &tilo_identity::store::account_identity_dir(&config.join("tilo/identity")),
            &tilo_device_identity::IdentityPaths::under(&identity),
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .expect("provision real host receipt signer");
        Self {
            _env: env,
            home,
            _key: key,
        }
    }

    /// The home's root, under which its identity directory lives.
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        self.home.root()
    }
}

impl Default for ProvisionedHome {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceTreeHandle {
    /// The workspaces this tree describes, which the fixture's host serves.
    fn opened(&self) -> Vec<String> {
        self.read()
            .workspaces
            .iter()
            .map(|workspace| workspace.id().to_string())
            .collect()
    }
    pub fn authority(&self) -> crate::services::WorkspaceAuthorityHandle {
        serve_the_test_signer(self.opened());
        Arc::new(self.clone())
    }
    pub fn services(&self) -> crate::services::ProcessServices {
        serve_the_test_signer(self.opened());
        self.services_unserved()
    }
    /// [`Self::services`] for a fixture whose host has opened nothing and names no signer: the
    /// same services, without serving this tree's workspaces.
    pub fn services_unserved(&self) -> crate::services::ProcessServices {
        crate::services::ProcessServices {
            journal_root: self.journal_root(),
            // The workspaces' `chat.agent` records, as these fixtures keep
            // them: every agent a workspace grants anything works there,
            // kept by the workspace's owner.
            agents: Some(Arc::new(TreeAgents(self.clone()))),
            ..Default::default()
        }
    }
    /// The fixture's journal, beside the profile it persists to, as a host
    /// keeping state keeps it.
    fn journal_root(&self) -> Option<std::path::PathBuf> {
        self.read().persist_path.as_ref().map(|p| {
            let mut n = p.clone().into_os_string();
            n.push(".invocations");
            n.into()
        })
    }
}
impl WorkspaceAuthority for WorkspaceTreeHandle {
    /// A test tree keeps no workspace documents: its workspaces revoke the devices the tree
    /// says they do.
    fn workspace_revocations(&self, workspace: &str) -> Option<tilo_user_identity::RevocationList> {
        let tree = self.read();
        crate::services::workspace_in(&tree.desktop, workspace).map(|_| tree.revoked.clone())
    }
    /// A test tree keeps no workspace document: its registry list is the one the test writes
    /// as `registries.toml` under `TILO_STORE_CONFIG_DIR`.
    fn package_registries(&self, _workspace: &str) -> Option<serde_json::Value> {
        let listed = app_cli::config::load_registries().ok()?;
        Some(serde_json::json!({
            "registries": listed.registries.iter().map(|registry| serde_json::json!({
                "name": registry.name,
                "base_url": registry.base_url,
                "owner_pubkey_hex": registry.owner_pubkey_hex,
                "enabled": registry.enabled,
                "channel": registry.channel,
            })).collect::<Vec<_>>(),
        }))
    }
    fn with_tree(&self, read: &mut dyn FnMut(&tilo_desktop_model::Layout)) -> Result<(), String> {
        read(&self.read().desktop);
        Ok(())
    }
}

/// A workspace's record of its agents as a test holds it: which agents it
/// names, by workspace.
#[derive(Clone, Default)]
pub struct KeptAgents {
    listed: Vec<(String, String, String)>,
}

impl KeptAgents {
    /// The record of `workspace` naming the agent `agent` of `person`.
    #[must_use]
    pub fn listing(mut self, workspace: &str, (agent, person): (&str, &str)) -> Self {
        self.listed
            .push((workspace.to_owned(), agent.to_owned(), person.to_owned()));
        self
    }
}

impl crate::services::AgentRoster for KeptAgents {
    fn listed(&self, workspace: &str) -> Vec<crate::services::ListedAgent> {
        self.listed
            .iter()
            .filter(|(held, ..)| held == workspace)
            .map(|(_, agent, person)| crate::services::ListedAgent {
                avatar: None,
                id: agent.clone(),
                name: agent.clone(),
                acts_for: person.clone(),
            })
            .collect()
    }
}

/// The workspaces' agents, read off the grants a fixture gave them: an agent
/// works in a workspace whose grants name it.
struct TreeAgents(crate::test_support::WorkspaceTreeHandle);

impl crate::services::AgentRoster for TreeAgents {
    fn listed(&self, _: &str) -> Vec<crate::services::ListedAgent> {
        Vec::new()
    }

    fn lists(&self, workspace: &str, agent: &tilo_member_types::MemberId) -> bool {
        let tree = self.0.read();
        crate::services::workspace_in(&tree.desktop, workspace).is_some_and(|held| {
            let node = held.policy.node();
            held.policy.grants.iter().any(|grant| {
                grant.node == node
                    && grant.answer == Answer::Allow
                    && grant.grantee.as_member() == Some(agent)
            })
        })
    }
}

/// The runs a test's broker was asked to start, each of which ran to its end at once.
#[cfg(feature = "agents")]
#[derive(Default)]
pub struct Started(pub std::sync::Mutex<Vec<crate::triggers::Firing>>);

#[cfg(feature = "agents")]
#[allow(clippy::unwrap_used, reason = "a fixture failing is its test failing")]
impl crate::triggers::StartRun for Started {
    fn start(
        &self,
        firing: &crate::triggers::Firing,
        _: tilo_automation::AutomationInvocationId,
    ) -> Result<u64, tilo_actions::remote::ActionCallError> {
        let mut started = self.0.lock().unwrap();
        started.push(firing.clone());
        Ok(u64::try_from(started.len()).unwrap())
    }

    fn ended(
        &self,
        firing: &crate::triggers::Firing,
        run: tilo_automation::AutomationInvocationId,
    ) -> Option<tilo_automation::InvocationReport> {
        Some(tilo_automation::InvocationReport {
            invocation: run,
            shortcut: firing.shortcut.clone(),
            version: None,
            trigger: firing.trigger.kind().into(),
            mode: tilo_automation::InvocationMode::Unattended,
            steps: Vec::new(),
            output: None,
            degraded: Vec::new(),
            outcome: tilo_automation::InvocationOutcome::Completed,
        })
    }
}

/// Decide who reads `workspaces`' records as the daemon does, through a real broker.
pub fn decide_record_reads(workspaces: &tilo_workspace_service::workspaces::Workspaces) {
    let broker = crate::collab_app_broker::AppBrokerState::test_broker_with_idle_directory_queue();
    tilo_workspace_service::workspaces::fixtures::decide_record_reads(
        workspaces,
        Arc::clone(broker.state()),
    );
}
