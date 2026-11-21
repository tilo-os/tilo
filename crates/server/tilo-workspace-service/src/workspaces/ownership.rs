//! A workspace this host makes for a person is theirs.
//!
//! The host founds it, as it founds every workspace, and hands it to the
//! person in the same step ([`Workspaces::create_for`]): from then on its
//! governance names them as its owner on every device, and this host stays
//! in it as one of its administrators, the grant every member reads and its
//! owner may withdraw. That grant is what the host serves the workspace with:
//! it admits whom invitations name, names its own address, signs the
//! workspace's checkpoints and relays what its members write.
//!
//! A workspace an earlier build kept for a person while this host owned it
//! is handed to that person when the host next starts, or when another
//! holder hands it to this host
//! ([`Workspaces::hand_kept_workspace_over`]).

use tilo_action_policy::GrantId;

use super::{GovernedWorkspace, Workspaces};

/// What the id of the grant started with that named the person an earlier
/// build kept a workspace for while its host owned it.
const KEPT_FOR: &str = "workspace-keeper-v1:";

impl Workspaces {
    /// Hand `governed`, a workspace this host owns, to `person`: one
    /// governance change, which leaves this host its administrator, and the
    /// name `name` for them when one is given, until they rename themselves.
    /// Nothing when it is theirs already, under that name, or `person` is
    /// this host's own.
    pub(super) async fn give_ownership(
        &self,
        governed: &mut GovernedWorkspace,
        person: [u8; 32],
        name: Option<&str>,
    ) -> Result<(), String> {
        if person == self.inner.host.cert.user_public_key {
            return Ok(());
        }
        let key = &self.inner.host.key;
        let now = tilo_clock::wall();
        let mut candidate = governed.peer().clone();
        let handed = candidate
            .governance_mut()
            .transfer_ownership(person, key, now)?;
        let named = match name {
            Some(name) => candidate
                .governance_mut()
                .name_person(person, name, key, now)?,
            None => false,
        };
        if !(handed || named) {
            return Ok(());
        }
        let state = governed.collaboration().clone();
        governed.commit(candidate, key, now, |_, _| Ok(state))?;
        self.settle(governed).await
    }

    /// Hand `governed` to the person it is kept for, when this host owns it:
    /// the person an earlier build's grant named, else the person whose own
    /// workspace this host records it as. That grant goes first, while this
    /// host may still take it back, and the workspace changes owner after.
    pub(super) async fn hand_kept_workspace_over(
        &self,
        governed: &mut GovernedWorkspace,
    ) -> Result<(), String> {
        let host = self.inner.host.cert.user_public_key;
        if governed.peer().owner() != host {
            return Ok(());
        }
        let policy = governed.peer().governance().governed_policy()?;
        let kept: Vec<GrantId> = policy
            .grants
            .iter()
            .filter(|grant| grant.id.as_str().starts_with(KEPT_FOR))
            .map(|grant| grant.id.clone())
            .collect();
        let named = kept.iter().find_map(|id| {
            let mut person = [0; 32];
            hex::decode_to_slice(id.as_str().strip_prefix(KEPT_FOR)?, &mut person).ok()?;
            Some(person)
        });
        let Some(person) = named
            .or_else(|| {
                self.own()
                    .and_then(|own| own.owner_of(governed.workspace()))
            })
            .filter(|person| *person != host)
        else {
            return Ok(());
        };
        if !kept.is_empty() {
            let key = &self.inner.host.key;
            let now = tilo_clock::wall();
            let mut candidate = governed.peer().clone();
            if candidate
                .governance_mut()
                .remove_governed_grants(&kept, key, now)?
            {
                let state = governed.collaboration().clone();
                governed.commit(candidate, key, now, |_, _| Ok(state))?;
            }
        }
        self.give_ownership(governed, person, None).await?;
        tracing::info!(workspace = %governed.workspace(), owner = %hex::encode(person),
            "a workspace this host kept for a person is theirs now");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::KEPT_FOR;
    use crate::workspaces::tests::{governor, test_host};
    use ed25519_dalek::SigningKey;
    use tilo_action_policy::{Answer, Grant, GrantId, Grantee, KindSet, Lifetime, roles};

    /// The grant an earlier build wrote to name the person a workspace was
    /// kept for while its host owned it, as it wrote it.
    fn kept_for(workspace: &str, person: &str, host: &str) -> Grant {
        Grant {
            id: GrantId(format!("{KEPT_FOR}{person}")),
            grantee: Grantee::person(person.to_owned()),
            node: roles::administrators_node(workspace),
            kinds: KindSet::UNDOABLE.union(KindSet::OUTSIDE),
            answer: Answer::Allow,
            lifetime: Lifetime::UntilRevoked,
            granted_by: Grantee::person(host.to_owned()),
        }
    }

    /// A workspace an earlier build kept for a person while its host owned
    /// it is theirs once the host starts again: its governance names them
    /// its owner, the grant that named them is gone, and the host stays in it
    /// as an administrator. The next start changes nothing.
    #[tokio::test]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "each start binds the server it drives until it shuts it down at the end"
    )]
    async fn a_kept_workspace_is_handed_to_its_person_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let host = governor();
        let host_person = hex::encode(host.1.user_public_key);
        let ada = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
        let server = test_host(dir.path(), &host.0, &host.1).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Kept").await.unwrap().workspace;
        let grant = kept_for(&workspace, &hex::encode(ada), &host_person);
        workspaces
            .put_workspace_grants(&workspace, &host.1, [5; 32], vec![grant.clone()])
            .await
            .unwrap();
        assert!(!workspaces.has_owner(&workspace).await);
        server.shutdown().await;

        for start in ["first", "second"] {
            let server = test_host(dir.path(), &host.0, &host.1).await;
            let workspaces = server.workspaces();
            assert!(
                workspaces.is_owner(&workspace, ada).await,
                "the {start} start leaves the workspace hers"
            );
            let policy = workspaces.policy_of(&workspace).await.unwrap();
            assert!(
                policy.grants.iter().all(|held| held.id != grant.id),
                "the grant that named her is gone"
            );
            assert!(policy.is_administrator(&host_person));
            server.shutdown().await;
        }
    }
}
