//! A resource's genesis change: the one every device creates alike.
//!
//! Two members who each create the same resource before they sync (a
//! workspace's default resource, the resource of one file) must write one
//! change, not two competing roots, or its history forks the first time they
//! meet. So the genesis is made at time zero, under an actor derived from the
//! resource's identity alone ([`actor`]), by code that depends on nothing else
//! — not the creator, the device or the clock.

use automerge::ActorId;

/// The actor a resource's genesis change is made under: `id` (its document
/// id), `kind` (its resource kind) and `home` (the workspace that minted it),
/// and nothing else.
#[must_use]
pub fn actor(id: &str, kind: &str, home: &str) -> ActorId {
    use sha2::Digest as _;
    let digest = sha2::Sha256::new()
        .chain_update(b"tilo.record.genesis\0")
        .chain_update(id.as_bytes())
        .chain_update(b"\0")
        .chain_update(kind.as_bytes())
        .chain_update(b"\0")
        .chain_update(home.as_bytes())
        .finalize();
    ActorId::from(digest.get(..16).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_resources_genesis_actor_is_derived_from_its_identity_alone() {
        // The bytes this derivation wrote before it lived here: a change to
        // the derivation forks every resource created afterwards, so it is
        // pinned.
        let actor = super::actor(
            "@tilo:package-primary",
            "tasks.tracker",
            "6f0c3a52-8e1b-4d6a-9c2f-3b7e5d1a4c90",
        );
        assert_eq!(
            hex::encode(actor.to_bytes()),
            "528fb54d990094231398c433832ba7af"
        );
        // The workspace the resource is minted in is part of it.
        assert_ne!(
            actor,
            super::actor(
                "@tilo:package-primary",
                "tasks.tracker",
                "another-workspace"
            )
        );
    }
}
