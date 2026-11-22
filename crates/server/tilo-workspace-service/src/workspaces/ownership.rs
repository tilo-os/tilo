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
//! is handed to that person when the host next starts, or, handed over by
//! another holder, when the person first chooses its servers
//! ([`Workspaces::hand_kept_workspace_to`]). Taking ownership from the host's
//! person keeps of their writes only what this copy holds then
//! ([`tilo_peer_state::standing`]); a copy only just handed the workspace
//! holds part of it, and the rest of the holder's history would be decided as
//! written after. By the time the person takes the servers in hand, the
//! holder that handed it over has handed everything over.

use tilo_action_policy::GrantId;
use tilo_user_identity::DeviceCert;

use super::{GovernedWorkspace, Workspaces};

/// What the id of the grant started with that named the person an earlier
/// build kept a workspace for while its host owned it.
const KEPT_FOR: &str = "workspace-keeper-v1:";

/// Whom an earlier build kept a workspace for while this host owned it.
struct Kept {
    /// The grants that named the people it was kept for, by id.
    grants: Vec<(GrantId, [u8; 32])>,
    /// The person it is handed to.
    person: [u8; 32],
}

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

    /// Whether `caller` chooses `governed`'s servers: its owner does, and so
    /// does the person an earlier build kept it for, whose first choice
    /// hands it to them ([`Self::hand_kept_workspace_to`]). What is offered
    /// to them has to say so, or they never reach the choice that makes it
    /// theirs.
    pub(super) fn may_choose_servers(
        &self,
        governed: &GovernedWorkspace,
        caller: &DeviceCert,
    ) -> bool {
        Self::chooses_servers(governed, caller)
            || (Self::speaks_for_person(governed, caller)
                && self.kept_person(governed).ok().flatten() == Some(caller.user_public_key))
    }

    /// [`Self::hand_kept_workspace_over`] when `person` is the one `governed`
    /// is kept for: a person choosing the servers of a workspace an earlier
    /// build kept for them does what only its owner does.
    pub(super) async fn hand_kept_workspace_to(
        &self,
        governed: &mut GovernedWorkspace,
        person: [u8; 32],
    ) {
        if self.kept_person(governed).ok().flatten() != Some(person) {
            return;
        }
        if let Err(error) = self.hand_kept_workspace_over(governed).await {
            tracing::error!(workspace = %governed.workspace(), %error, "a workspace this host keeps for a person was not handed to them");
        }
    }

    /// The grants an earlier build wrote to name the people `governed` was
    /// kept for, and the person it is kept for: the one this host records
    /// it as the own workspace of, else the first one named. `None` when
    /// this host does not own it, or keeps it for no one.
    fn kept(&self, governed: &GovernedWorkspace) -> Result<Option<Kept>, String> {
        let host = self.inner.host.cert.user_public_key;
        if governed.peer().owner() != host {
            return Ok(None);
        }
        let policy = governed.peer().governance().governed_policy()?;
        let mut kept: Vec<(GrantId, [u8; 32])> = policy
            .grants
            .iter()
            .filter_map(|grant| {
                let mut person = [0; 32];
                hex::decode_to_slice(grant.id.as_str().strip_prefix(KEPT_FOR)?, &mut person)
                    .ok()?;
                Some((grant.id.clone(), person))
            })
            .collect();
        kept.sort_by_key(|(id, _)| id.clone());
        let own = self
            .own()
            .and_then(|own| own.owner_of(governed.workspace()));
        Ok(own
            .or_else(|| kept.first().map(|(_, person)| *person))
            .filter(|person| *person != host)
            .map(|person| Kept {
                grants: kept,
                person,
            }))
    }

    /// The person `governed` is kept for ([`Self::kept`]).
    fn kept_person(&self, governed: &GovernedWorkspace) -> Result<Option<[u8; 32]>, String> {
        Ok(self.kept(governed)?.map(|kept| kept.person))
    }

    /// Hand `governed` to the person it is kept for, when this host owns it:
    /// the person this host records it as the own workspace of, else the one
    /// an earlier build's grant named. An earlier build could name several;
    /// each other one it named stays an administrator, as that grant let them
    /// govern. Those grants go first, while this host may still take them
    /// back, and the workspace changes owner after.
    pub(super) async fn hand_kept_workspace_over(
        &self,
        governed: &mut GovernedWorkspace,
    ) -> Result<(), String> {
        let Some(Kept {
            grants: kept,
            person,
        }) = self.kept(governed)?
        else {
            return Ok(());
        };
        if !kept.is_empty() {
            let key = &self.inner.host.key;
            let now = tilo_clock::wall();
            let mut candidate = governed.peer().clone();
            for (_, other) in kept.iter().filter(|(_, kept)| *kept != person) {
                candidate
                    .governance_mut()
                    .set_member_administrator(*other, None, key, now)?;
            }
            let ids: Vec<GrantId> = kept.iter().map(|(id, _)| id.clone()).collect();
            candidate
                .governance_mut()
                .remove_governed_grants(&ids, key, now)?;
            let state = governed.collaboration().clone();
            governed.commit(candidate, key, now, |_, _| Ok(state))?;
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

    /// An earlier build could keep a workspace for more than one person. One
    /// of them is its owner once the host starts again, and each other one
    /// it was kept for still governs it, as an administrator: nobody it was
    /// kept for is left without a say.
    #[tokio::test]
    async fn every_other_person_a_workspace_was_kept_for_stays_its_administrator() {
        let dir = tempfile::tempdir().unwrap();
        let host = governor();
        let host_person = hex::encode(host.1.user_public_key);
        let [ada, bo] = [[9; 32], [10; 32]]
            .map(|seed| hex::encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes()));
        let server = test_host(dir.path(), &host.0, &host.1).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Kept").await.unwrap().workspace;
        let grants = vec![
            kept_for(&workspace, &ada, &host_person),
            kept_for(&workspace, &bo, &host_person),
        ];
        workspaces
            .put_workspace_grants(&workspace, &host.1, [5; 32], grants)
            .await
            .unwrap();
        server.shutdown().await;

        let server = test_host(dir.path(), &host.0, &host.1).await;
        let workspaces = server.workspaces();
        let policy = workspaces.policy_of(&workspace).await.unwrap();
        let owner = policy
            .owner()
            .expect("the workspace has an owner")
            .to_owned();
        assert!(
            owner == ada || owner == bo,
            "{owner} is one it was kept for"
        );
        let other = if owner == ada { &bo } else { &ada };
        assert!(
            policy.is_administrator(other),
            "the other person it was kept for governs it still"
        );
        assert!(
            policy
                .grants
                .iter()
                .all(|grant| !grant.id.as_str().starts_with(super::KEPT_FOR)),
            "no grant of the earlier build's is left"
        );
        server.shutdown().await;
    }
}
