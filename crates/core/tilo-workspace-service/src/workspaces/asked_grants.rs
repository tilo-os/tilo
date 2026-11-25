//! The grant a person gives with an approve of an asked record ("approve and grant"): an
//! ordinary grant of the workspace's policy, written as this host's change, shown and taken
//! back like every other grant.
//!
//! This host writes it only for its own person, whose key it signs with, and only within what
//! that person may do themselves: a grant never reaches beyond the one who gives it.

use std::sync::Arc;

use tilo_action_policy::{Answer, Context, Grant};
use tilo_actions::remote::asked::AskedGrants;
use tilo_actions::remote::{Pending, pending};

use super::{GovernedWorkspace, Workspaces};

/// What refuses a grant asked of this host for someone other than its own person.
const NOT_OURS: &str = "this server writes grants only for the person it belongs to";

impl Workspaces {
    /// Write `grant` in `workspace`, given by `person` (hex root key): refused unless `person`
    /// is this host's own, this host governs the workspace, and `person` may take every kind
    /// of action it grants at its node themselves.
    ///
    /// # Errors
    /// Why it was not written.
    pub async fn give_asked_grant(
        &self,
        workspace: &str,
        person: &str,
        grant: Grant,
    ) -> Result<(), String> {
        if person != hex::encode(self.inner.host.cert.user_public_key)
            || grant.granted_by != tilo_action_policy::Grantee::person(person.to_owned())
        {
            return Err(NOT_OURS.to_owned());
        }
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = governed.lock().await;
        let at = tilo_clock::wall();
        let access = governed
            .peer()
            .permissions(self.inner.host.cert.device_public_key, at)
            .map_err(|error| error.to_string())?;
        let now = Context::at(tilo_clock::unix_now_ms());
        if access.person_id() != Some(person)
            || grant
                .kinds
                .iter()
                .any(|kind| access.answer(&grant.node, kind, &now) != Answer::Allow)
        {
            return Err("it would allow more than you may do yourself".to_owned());
        }
        drop(access);
        self.put_grants(&mut governed, at, std::slice::from_ref(&grant))
            .await
    }

    /// Write `grants` in `workspace`, as the host's change to it.
    async fn put_grants(
        &self,
        workspace: &mut GovernedWorkspace,
        at: std::time::SystemTime,
        grants: &[Grant],
    ) -> Result<(), String> {
        if !self.administers(workspace) {
            return Err("this server's person does not govern the workspace".to_owned());
        }
        let key = &self.inner.host.key;
        let mut candidate = workspace.peer().clone();
        if candidate
            .governance_mut()
            .put_governed_grants(grants, key, at)?
        {
            let state = workspace.collaboration().clone();
            workspace.keep(candidate, key, at, |_, _| Ok(state))?;
            self.settle(workspace).await?;
        }
        Ok(())
    }

    /// What writes the grants this host's people give with an approve, on `runtime`.
    #[must_use]
    pub fn asked_grants(&self, runtime: tokio::runtime::Handle) -> Arc<dyn AskedGrants> {
        Arc::new(HostGrants {
            workspaces: self.clone(),
            runtime,
        })
    }
}

struct HostGrants {
    workspaces: Workspaces,
    runtime: tokio::runtime::Handle,
}

impl AskedGrants for HostGrants {
    fn grant(&self, workspace: &str, person: &str, grant: Grant) -> Pending<Result<(), String>> {
        let (done, answer) = pending();
        let (workspaces, workspace, person) = (
            self.workspaces.clone(),
            workspace.to_owned(),
            person.to_owned(),
        );
        self.runtime.spawn(async move {
            done.complete(
                workspaces
                    .give_asked_grant(&workspace, &person, grant)
                    .await,
            );
        });
        answer
    }
}
