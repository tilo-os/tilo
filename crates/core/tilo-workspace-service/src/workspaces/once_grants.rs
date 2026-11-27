//! A grant given once ([`tilo_action_types::Lifetime::Once`]) admits exactly one call. Two
//! calls decided at the same moment both find it in the workspace's grants; the one that claims
//! it here goes on, and the other is refused. The grant is then taken back from the workspace,
//! and the claim holds until that change is committed, after which the workspace no longer has
//! the grant to find.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tilo_action_policy::{Change, GrantId};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

use super::Workspaces;

/// The grants given once that a call admitted here has spent and whose taking back is not yet
/// committed, by workspace and grant id.
#[derive(Default)]
pub(super) struct OnceClaims(Arc<Mutex<HashSet<(String, GrantId)>>>);

/// One call's claim on a grant given once: no other call here is admitted by it while it holds.
struct Claim {
    claims: Arc<Mutex<HashSet<(String, GrantId)>>>,
    key: (String, GrantId),
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Ok(mut claims) = self.claims.lock() {
            claims.remove(&self.key);
        }
    }
}

impl Workspaces {
    fn claim_once(&self, workspace: &str, grant: &GrantId) -> Option<Claim> {
        let claims = &self.inner.once_claims.0;
        let key = (workspace.to_owned(), grant.clone());
        // A poisoned set claims nothing: no call is admitted by a grant given once.
        claims.lock().ok()?.insert(key.clone()).then(|| Claim {
            claims: Arc::clone(claims),
            key,
        })
    }

    async fn take_back(&self, workspace: &str, grant: GrantId) -> Result<(), String> {
        self.commit_own_change(
            workspace,
            WorkspaceMembershipCommand::Member {
                change: Change::Revoke(grant),
            },
            None,
        )
        .await
    }

    /// Spend `grant`, given once in `workspace`, for the call being admitted now: true for
    /// exactly one call, which may go on; false when another call spent it first. The grant is
    /// taken back from the workspace behind it.
    #[must_use]
    pub fn spend_once_now(&self, workspace: &str, grant: &GrantId) -> bool {
        let Some(claim) = self.claim_once(workspace, grant) else {
            return false;
        };
        let workspaces = self.clone();
        let workspace = workspace.to_owned();
        let grant = grant.clone();
        tilo_rt::app().spawn(async move {
            if let Err(error) = workspaces.take_back(&workspace, grant.clone()).await {
                tracing::warn!(%grant, %error, "a grant given once could not be taken back");
            }
            drop(claim);
        });
        true
    }

    /// Spend `grant`, given once in `workspace`, for the call being admitted now, and take it
    /// back before answering.
    ///
    /// # Errors
    /// Another call spent it first, or it could not be taken back: either way, it admits
    /// nothing.
    pub async fn spend_once(&self, workspace: &str, grant: &GrantId) -> Result<(), String> {
        let _claim = self
            .claim_once(workspace, grant)
            .ok_or_else(|| format!("the grant {grant} given once is already spent"))?;
        self.take_back(workspace, grant.clone()).await
    }
}
