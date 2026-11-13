//! When the daemon's endpoint starts its relays. `fs serve` binds with an empty
//! relay map and holds the default relays back until something shows another party
//! may dial this device: a peer the content plane learns of, sync progress with a
//! peer, a capability list naming anyone other than the mount's own actor, a live
//! share grant, or no capability list at all. Waking is sticky for the life of the
//! process. Dialling out works while dormant.

use crate::acl::AclHandle;

pub use tilo_endpoint_reachability::Reachability;

/// Start the held-back relays because of `why`, and say so on stdout; returns
/// whether this call started them.
pub fn wake(reachability: &Reachability, why: &'static str) -> bool {
    let started = reachability.wake(why);
    if started {
        println!("Starting relays: this device may be dialled ({why})");
    }
    started
}

/// Wake `reachability` once `acl` names anyone other than the mount's own actor:
/// now, or after any change to it while dormant. Returns the task watching for that
/// change, if one was needed.
pub fn wake_when_acl_admits_others(
    reachability: &Reachability,
    acl: &AclHandle,
    what: &'static str,
) -> Option<tokio::task::JoinHandle<()>> {
    if !reachability.is_dormant() {
        return None;
    }
    if acl.snapshot().names_others() {
        wake(reachability, what);
        return None;
    }
    let mut revisions = acl.subscribe_revision();
    let acl = acl.clone();
    let reachability = reachability.clone();
    Some(tokio::spawn(async move {
        while revisions.changed().await.is_ok() {
            if !reachability.is_dormant() {
                return;
            }
            if acl.snapshot().names_others() {
                wake(&reachability, what);
                return;
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use collab_types::{Access, CapOverride, Ed25519PublicKey, Effect, Principal, Scope, Subject};
    use iroh::endpoint::presets;
    use iroh::{RelayMap, RelayMode, RelayUrl};

    use super::{Reachability, wake_when_acl_admits_others};
    use crate::acl::{AclHandle, WorkspaceAcl};

    fn unused_relays() -> RelayMap {
        ["http://127.0.0.1:9/", "http://127.0.0.1:10/"]
            .into_iter()
            .map(|url| url.parse::<RelayUrl>().unwrap())
            .collect()
    }

    async fn endpoint_without_relays() -> (iroh::Endpoint, RelayMap) {
        let relays = RelayMap::empty();
        let endpoint = iroh::Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Custom(relays.clone()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        (endpoint, relays)
    }

    async fn until_relays(relays: &RelayMap, count: usize) {
        tilo_test_env::within(
            format!("the endpoint's relay map to reach {count} relays"),
            async {
                while relays.len() != count {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            },
        )
        .await;
    }

    fn grant(subject: Subject) -> CapOverride {
        CapOverride::new(
            subject,
            Scope::FsPath("/".into()),
            Effect::Grant,
            Access::Write,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_grant_to_someone_else_wakes_the_relays() {
        let me = [1; 32];
        let local = Principal::Person("local".into());
        let own = vec![grant(Subject::Principal(local.clone()))];
        let acl = AclHandle::new(WorkspaceAcl::from_capabilities(
            me,
            &own,
            std::slice::from_ref(&local),
            None,
        ));
        let (endpoint, relays) = endpoint_without_relays().await;
        let reachability = Reachability::defer(
            &endpoint,
            &unused_relays(),
            tokio::runtime::Handle::current(),
            None,
        );
        let watcher = wake_when_acl_admits_others(&reachability, &acl, "a test grant")
            .expect("an ACL naming only this mount leaves the relays asleep");
        acl.replace(WorkspaceAcl::from_capabilities(
            me,
            &own,
            std::slice::from_ref(&local),
            None,
        ));
        assert!(
            !acl.snapshot().names_others() && reachability.is_dormant(),
            "a change that still names only this mount must not wake them"
        );
        let mut shared = own.clone();
        shared.push(grant(Subject::User(Ed25519PublicKey([2; 32]))));
        acl.replace(WorkspaceAcl::from_capabilities(
            me,
            &shared,
            std::slice::from_ref(&local),
            None,
        ));
        until_relays(&relays, 2).await;
        assert!(!reachability.is_dormant());
        tilo_test_env::within("the watcher to end once the relays are up", watcher)
            .await
            .unwrap();
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_acl_naming_someone_else_wakes_the_relays_at_start() {
        let acl = AclHandle::new(WorkspaceAcl::from_capabilities(
            [1; 32],
            &[grant(Subject::Principal(Principal::Workspace(
                "team".into(),
            )))],
            &[],
            None,
        ));
        let (endpoint, relays) = endpoint_without_relays().await;
        let reachability = Reachability::defer(
            &endpoint,
            &unused_relays(),
            tokio::runtime::Handle::current(),
            None,
        );
        assert!(wake_when_acl_admits_others(&reachability, &acl, "a workspace grant").is_none());
        until_relays(&relays, 2).await;
        endpoint.close().await;
    }
}
