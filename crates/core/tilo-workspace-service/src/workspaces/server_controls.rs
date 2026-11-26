//! Who may control a server's machine: a grant in its person's own workspace, naming the person
//! it lets.
//!
//! A device belongs to its person alone: a workspace's ownership or its administrator role never
//! reaches it. What reaches a machine's controls is an ordinary policy grant, written in the
//! server's person's own workspace on the server's settings resource
//! (`devices/<device>/settings/server`), which [`crate::workspaces::server_controls::caller_may`]
//! decides through the one policy decision. [`Workspaces::grant_server_controls`] writes one for
//! each administrator of a workspace the server is given, and the release passes take back those
//! the server or the administrator no longer stands for. Every grant here is written for a member
//! ([`Grantee::person`]), never an agent: an agent acts through the person's grant.

use tilo_action_policy::{
    ActionKind, Answer, Context, Grant, GrantId, Grantee, KindSet, TreePath, WorkspacePolicy,
};
use tilo_actions::remote::Caller;
use tilo_server_protocol::servers::person_workspace_id;

use super::Workspaces;
use super::collaboration::request_digest;

/// The ids of the grants this module writes start with this, so the ones `servers add` wrote are
/// told apart from grants a person wrote themselves beside them.
const GRANT_PREFIX: &str = "servers-v1:";

/// The setting every machine control is decided at: the server's own settings resource.
pub const CONTROL: &str = "server";

/// The node a device's setting `name` is decided at, in `workspace`: the Settings app's
/// `devices/<device>/settings/<name>`, which [`caller_may`] decides.
#[must_use]
pub fn settings_node(workspace: &str, device: &[u8; 32], name: &str) -> TreePath {
    TreePath::workspace(workspace)
        .app(tilo_app_id::apps::SETTINGS)
        .resource(format!("devices/{}/settings/{name}", hex::encode(device)))
}

/// The id of the grant letting `person` (hex root key) control the server `device`.
#[must_use]
pub fn grant_id(device: &[u8; 32], person: &str) -> GrantId {
    GrantId::new(format!("{GRANT_PREFIX}{}:{person}", hex::encode(device)))
}

/// Whether `id` names a grant this module wrote, and for whom: the server's device key and the
/// person it lets.
#[must_use]
pub fn grant_named(id: &GrantId) -> Option<(&str, &str)> {
    let rest = id.as_str().strip_prefix(GRANT_PREFIX)?;
    rest.split_once(':')
}

/// The grant letting `person` (hex root key) control the server `device`, written in the server's
/// person's own `workspace` by that person (`by`, hex root key). It covers what controlling a
/// machine is: reading it, and changing it within its history or outside it (restarting a unit,
/// rebooting it). It answers who may call a control, never whether its person is asked first: a
/// control whose contract asks still asks, on the device it is called from.
#[must_use]
pub fn grant(workspace: &str, device: &[u8; 32], person: &str, by: &str) -> Grant {
    Grant {
        id: grant_id(device, person),
        grantee: Grantee::person(person.to_owned()),
        node: settings_node(workspace, device, CONTROL),
        kinds: KindSet::READ_UNDOABLE.with(ActionKind::Outside),
        answer: Answer::Allow,
        lifetime: tilo_action_policy::Lifetime::UntilRevoked,
        granted_by: Grantee::person(by.to_owned()),
    }
}

/// Whether `caller` may take `kind` at `node` under `policy`: the one policy decision, for a
/// caller as the transport admitted it (a person, or an agent acting for one).
#[must_use]
pub fn caller_may(
    policy: &WorkspacePolicy,
    caller: &Caller,
    node: &TreePath,
    kind: ActionKind,
) -> bool {
    let granted = caller.granted();
    tilo_action_policy::Authority::of_acting(caller.acting(), policy, &granted)
        .decide(node, kind, &Context::at(tilo_clock::unix_now_ms()))
        .answer
        == Answer::Allow
}

impl Workspaces {
    /// Write the grants that let each administrator of `workspace` control the server `device`:
    /// one member grant per administrator, in this device's person's own workspace, on the
    /// server's settings resource ([`CONTROL`]). A grant already there is left as it is.
    ///
    /// # Errors
    /// The workspace's policy or its administrators do not read, or the person's own workspace
    /// does not take the grants.
    pub async fn grant_server_controls(
        &self,
        workspace: &str,
        device: [u8; 32],
    ) -> Result<(), String> {
        let own = person_workspace_id(&self.inner.host.cert.user_public_key);
        let by = hex::encode(self.inner.host.cert.user_public_key);
        if workspace == own {
            // The person's own workspace is nobody's to be granted: its owner already controls
            // every server that holds it.
            return Ok(());
        }
        let Some(policy) = self.policy_of(workspace).await else {
            return Err(format!("{workspace}'s policy does not read"));
        };
        let held = self.policy_of(&own).await;
        let missing: Vec<Grant> = policy
            .administrators()
            .map(|person| grant(&own, &device, person, &by))
            .filter(|grant| {
                !held
                    .as_ref()
                    .is_some_and(|held| held.grants.iter().any(|held| held.id == grant.id))
            })
            .collect();
        self.put_controls(&own, missing).await
    }

    /// Write the grants that let every administrator of `workspace` control each of this
    /// person's servers serving it: what making a new administrator follows.
    ///
    /// # Errors
    /// A server's controls do not read or take the grants.
    pub async fn refresh_server_controls(&self, workspace: &str) -> Result<(), String> {
        let ours = hex::encode(self.inner.host.cert.user_public_key);
        let keepers = self.keeping_devices(workspace).await?;
        for keeper in keepers.into_iter().filter(|keeper| keeper.person == ours) {
            let Some(device) = tilo_failure::ignored!(
                "a keeping device's key is hex, in thirty-two bytes",
                hex::decode(&keeper.device)
                    .ok()
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
            ) else {
                continue;
            };
            self.grant_server_controls(workspace, device).await?;
        }
        Ok(())
    }

    /// Take back the grants letting anyone control the server `device` who no longer
    /// administers a workspace this device serves: the server stopped serving one, or its
    /// administrator left. Grants a person wrote themselves are not this module's and stay.
    ///
    /// # Errors
    /// The person's own workspace does not read or take the change.
    pub async fn release_server_controls(&self, device: [u8; 32]) -> Result<(), String> {
        let led = hex::encode(device);
        self.release_controls(|named, _| named == led).await
    }

    /// Take back the grants letting `person` (hex root key) control any server this device
    /// serves, when they no longer administer a workspace one of them serves.
    ///
    /// # Errors
    /// The person's own workspace does not read or take the change.
    pub async fn release_administrator_controls(&self, person: &str) -> Result<(), String> {
        self.release_controls(|_, held| held == person).await
    }

    /// The grants this module wrote in the person's own workspace that `keep` does not keep, as
    /// the ids to take back: `keep` answers the device and person a grant names, and a grant kept
    /// whose serving still stands for it stays.
    async fn release_controls(&self, keep: impl Fn(&str, &str) -> bool) -> Result<(), String> {
        let own = person_workspace_id(&self.inner.host.cert.user_public_key);
        let Some(policy) = self.policy_of(&own).await else {
            return Ok(());
        };
        let mut revoke = Vec::new();
        for grant in &policy.grants {
            let Some((device, person)) = grant_named(&grant.id) else {
                continue;
            };
            if keep(device, person) && !self.serves(device, person).await {
                revoke.push(grant.id.clone());
            }
        }
        self.remove_controls(&own, revoke).await
    }

    /// Whether `device` (hex) serves a workspace this device holds whose administrators include
    /// `person`: what keeps their grant to control it.
    async fn serves(&self, device: &str, person: &str) -> bool {
        let Some(device) = tilo_failure::ignored!(
            "a keeping device's key is hex, in thirty-two bytes",
            hex::decode(device)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        ) else {
            return false;
        };
        for summary in self.list().await {
            let workspace = summary.workspace;
            if self.names_server(&workspace, device).await
                && self
                    .policy_of(&workspace)
                    .await
                    .is_some_and(|policy| policy.is_administrator(person))
            {
                return true;
            }
        }
        false
    }

    /// Write `grants` into the person's own `workspace`.
    async fn put_controls(&self, workspace: &str, grants: Vec<Grant>) -> Result<(), String> {
        if grants.is_empty() {
            return Ok(());
        }
        let cert = self.inner.host.cert.clone();
        let request = request_digest(&grants).map_err(|error| error.to_string())?;
        self.put_workspace_grants(workspace, &cert, request, grants)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Take back the grants `ids` from the person's own `workspace`.
    async fn remove_controls(&self, workspace: &str, ids: Vec<GrantId>) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let cert = self.inner.host.cert.clone();
        let request = request_digest(&("servers", &ids)).map_err(|error| error.to_string())?;
        // Reached from taking back an administrator's grant, which is what this write is for:
        // boxed so the two paths are one finite future.
        Box::pin(self.remove_workspace_grants(workspace, &cert, request, ids))
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}
