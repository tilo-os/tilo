//! Existing authenticated relay SyncRequest -> original serialized owner.
use crate::{SessionOwner, committed_authority::CommittedAuthority};
use tilo_relay::peer::sync_refresh::Request;

mod mount;
pub(super) use mount::before_action_mount;

#[cfg(all(test, feature = "test-support"))]
mod aged_mount_tests;
#[cfg(test)]
mod mount_tests;

pub(super) async fn execute(
    owner: &mut SessionOwner,
    committed: &CommittedAuthority,
    request: Request,
    pool: Option<&crate::host_bytes::HostBytes>,
) -> Result<(), String> {
    use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
    request
        .execute(|workspace, certificate| {
            if workspace != owner.workspace_id() || !owner.permits_publication() {
                return Err("checkpoint refresh owner is unavailable".into());
            }
            let peer = owner.peer();
            let now = web_time::SystemTime::now();
            let (role, caps, person) = peer.permissions(certificate.device_public_key, now)?;
            if !peer.certificates().contains(certificate) {
                return Err("checkpoint refresh certificate is no longer current".into());
            }
            let room = peer.room().to_owned();
            let actor = Actor::person(Some(role), &person).in_room(room.clone());
            if decide_capability(
                &actor,
                &ResourceRef::Room(room),
                Verb::Read,
                &caps,
                Some(tilo_relay::clock::now_ns() / 1_000_000_000),
            ) != CapDecision::Allow
            {
                return Err("checkpoint refresh requires current room read".into());
            }
            if let Some(pool) = pool {
                let mut scope = owner
                    .prepare_owner_scope(pool, crate::owner::resources::OwnerOperation::Checkpoint)
                    .map_err(|error| error.to_string())?;
                return owner
                    .with_owner_scope(&mut scope, committed, |owner| owner.refresh_checkpoint())
                    .map_err(|error| error.to_string())?
                    .map(|_| ())
                    .map_err(|error| error.to_string());
            }
            let update = committed.begin(owner).map_err(|error| error.to_string())?;
            let refreshed = owner.refresh_checkpoint();
            // Publish the actual durable result even after ordinary rejected work.
            // A real barrier failure leaves the original authority terminally denied.
            if !super::publish_owner_authority(update, owner).map_err(|error| error.to_string())? {
                return Err("checkpoint refresh durability failed".into());
            }
            refreshed.map(|_| ()).map_err(|error| error.to_string())
        })
        .await
}

/// Test-only dispatcher retaining the original real owner's committed authority.
/// It accepts only requests minted by the live relay subscription path.
#[cfg(feature = "test-support")]
pub struct CheckpointRefreshTestOwner {
    committed: CommittedAuthority,
}
#[cfg(feature = "test-support")]
impl CheckpointRefreshTestOwner {
    /// Retain the same actual owner publication boundary for the test host loop.
    pub fn new(owner: &SessionOwner) -> Self {
        Self {
            committed: CommittedAuthority::new(owner),
        }
    }
    /// Dispatch an opaque authenticated request through the production child.
    /// The fixture owns `owner` directly; no mutex is held across this await.
    pub async fn execute(&self, owner: &mut SessionOwner, request: Request) -> Result<(), String> {
        execute(owner, &self.committed, request, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{LocalHost, publish};
    use std::time::Duration;

    #[tokio::test]
    async fn native_sync_refresh_real_session_commits_new_signed_checkpoint() {
        exchange_case(false, false).await;
    }
    #[tokio::test]
    async fn native_sync_refresh_real_session_barrier_failure_denies() {
        exchange_case(true, false).await;
    }
    #[tokio::test]
    async fn native_sync_refresh_real_session_uses_attached_owner_pool() {
        exchange_case(false, true).await;
    }
    async fn exchange_case(fail_store: bool, attached: bool) {
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let mut owner = SessionOwner::initialize(&directory.path().join("owner")).unwrap();
        let (ingress, _receiver) = crate::https::ingress_channel();
        let pool = attached.then(|| ingress.host_bytes());
        if let Some(pool) = &pool {
            owner.attach_data_resources(pool).unwrap();
        }
        let committed = CommittedAuthority::new(&owner);
        let host = LocalHost::start(&owner).await.unwrap();
        let (mailbox, mut requests) = tilo_relay::peer::sync_refresh::Mailbox::bounded();
        host.relay
            .install_sync_refresh(owner.workspace_id(), mailbox)
            .await
            .unwrap();
        let mut session = host.connect(&owner).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !session.is_ready() {
                session.recv().await.expect("live actual subscription");
            }
        })
        .await
        .unwrap();
        let old_generation = owner.committed_generation();
        let old_checkpoint = owner
            .outgoing()
            .iter()
            .map(|entry| entry.bytes.to_vec())
            .collect::<Vec<_>>();
        session.request_checkpoint_refresh().unwrap();
        let request = tokio::time::timeout(Duration::from_secs(3), requests.recv())
            .await
            .unwrap()
            .unwrap();
        if fail_store {
            std::fs::rename(
                owner.root().join("session.bin"),
                owner.root().join("saved-session.bin"),
            )
            .unwrap();
            std::fs::create_dir(owner.root().join("session.bin")).unwrap();
        }
        let result = execute(&mut owner, &committed, request, pool.as_ref()).await;
        // Close genuine transports even when the intended RED fails below.
        if result.is_ok() {
            publish(&owner, &session).unwrap();
        }
        let changed = owner.committed_generation() > old_generation;
        let fresh = owner
            .outgoing()
            .iter()
            .map(|entry| entry.bytes.to_vec())
            .collect::<Vec<_>>()
            != old_checkpoint;
        session.close().await;
        host.shutdown().await.unwrap();
        if fail_store {
            assert!(result.is_err());
            assert!(
                !owner.durability_healthy(),
                "actual failed barrier must poison owner"
            );
            assert!(matches!(
                committed.observe(),
                crate::committed_authority::Observation::Denied
            ));
            assert!(
                !changed && !fresh,
                "failed barrier must not install signed replacement"
            );
            return;
        }
        assert!(result.is_ok(), "actual native barrier refresh: {result:?}");
        assert!(
            changed && fresh,
            "request must commit new signed publication over original history"
        );
    }
}
