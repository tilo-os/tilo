//! When the filesystem daemon's endpoint starts its relays.
//!
//! `fs serve` binds with an *empty* relay map and holds the default relays
//! in a [`Reachability`] until something means another party may dial this
//! device; see [`tilo_endpoint_reachability`] for what the relays cost and
//! how they are held back. Dialling out works while dormant too.
//!
//! Who can dial a mount? Only a party that knows it, and every way of
//! knowing it leaves a trace the daemon sees. Each one wakes the relays:
//!
//! * a peer the content plane learns of — persisted, bootstrapped from a
//!   ticket, met over gossip or authenticated in a session
//!   ([`crate::Hydrator::add_peer`]);
//! * sync progress persisted with a peer: someone synced with this
//!   namespace before;
//! * a capability list that names anyone other than the mount's own actor,
//!   at start or after any change to it;
//! * a live share grant, or any request to the share control socket;
//! * no capability list at all: an unenforced mount is shared by handing
//!   out its ticket, which leaves no trace.
//!
//! Waking is sticky for the life of the process: the endpoint's handle is
//! deferred without a rest delay.

use crate::acl::AclHandle;

pub use tilo_endpoint_reachability::Reachability;

/// Start `reachability`'s held-back relays because of `why`, and say so on
/// stdout, where the daemon's log goes. Returns whether this call started
/// them.
pub fn wake(reachability: &Reachability, why: &'static str) -> bool {
    let started = reachability.wake(why);
    if started {
        println!("Starting relays: this device may be dialled ({why})");
    }
    started
}

/// Wake `reachability` once `acl` names anyone other than the mount's own
/// actor: now, or after any change to it while dormant. Returns the task
/// watching for that change, if one was needed.
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
    use std::time::{Duration, Instant};

    use collab_types::{Access, CapOverride, Ed25519PublicKey, Effect, Principal, Scope, Subject};
    use iroh::endpoint::presets;
    use iroh::{RelayMap, RelayMode, RelayUrl};

    use super::{Reachability, wake_when_acl_admits_others};
    use crate::acl::{AclHandle, WorkspaceAcl};

    /// Two relays nobody listens on: the test observes the relay map, not
    /// a relay connection.
    fn unused_relays() -> RelayMap {
        ["http://127.0.0.1:9/", "http://127.0.0.1:10/"]
            .into_iter()
            .map(|url| url.parse::<RelayUrl>().unwrap())
            .collect()
    }

    /// An endpoint bound the way `fs serve` binds one, and its relay map:
    /// the map is shared, so it shows what the endpoint's net report and
    /// relay transport see.
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
        let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        while relays.len() != count {
            assert!(
                Instant::now() < deadline,
                "the endpoint's relay map never reached {count} relays"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn grant(subject: Subject) -> CapOverride {
        CapOverride::new(
            subject,
            Scope::FsPath("/".into()),
            Effect::Grant,
            Access::Write,
        )
    }

    /// The shipped capability list grants only the mount's own principal:
    /// nobody else can be admitted, so the relays stay asleep. A grant to
    /// another person, made while the mount runs, wakes them.
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
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            reachability.is_dormant(),
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
        tokio::time::timeout(Duration::from_secs(5), watcher)
            .await
            .expect("the watcher ends once the relays are up")
            .unwrap();
        endpoint.close().await;
    }

    /// An ACL that already names someone else wakes the relays at once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_acl_naming_someone_else_wakes_the_relays_at_start() {
        let acl = AclHandle::new(WorkspaceAcl::from_capabilities(
            [1; 32],
            &[grant(Subject::Principal(Principal::Room("team".into())))],
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
        assert!(wake_when_acl_admits_others(&reachability, &acl, "a room grant").is_none());
        until_relays(&relays, 2).await;
        endpoint.close().await;
    }
}
