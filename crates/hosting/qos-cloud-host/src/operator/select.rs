//! Explicit operator selection of the private agent runtime for an enrolled owner.
//! This is the host migration step that `Store::activate_private_agent_ledger`
//! documents as never reachable from the network: a local filesystem operator
//! runs it while `serve-https` is stopped. The named device must pass the same
//! current structural-owner verification (`agent_caller`) as agent commands.
use anyhow::{Result, anyhow, bail, ensure};
use qos_peer_state::owner_commit::PrivateAgentState;
use std::path::Path;

/// Select private agent schema V1 and then runtime V2 for `device` (hex public
/// key of a current owner browser device). Idempotent: an already selected V2
/// Store is left unchanged. Returns the resulting selection label.
pub fn select_private_agent_runtime(
    root: &Path,
    machine: &str,
    device: &str,
) -> Result<&'static str> {
    ensure!(root.is_absolute(), "absolute operator Store root required");
    let mut key = [0u8; 32];
    ensure!(
        device.len() == 64
            && hex::decode_to_slice(device, &mut key).is_ok()
            && hex::encode(key) == device,
        "canonical lower-hex owner device key required"
    );
    let mut store = match crate::delivery::access::try_open(root, machine) {
        Ok(store) => store,
        Err(crate::delivery::access::AccessError::Busy) => {
            bail!("operator Store busy; stop serve-https before selecting")
        }
        Err(crate::delivery::access::AccessError::Denied(error)) => {
            return Err(error.context("existing delivery Cloud Store required"));
        }
    };
    ensure!(store.state.bound.is_some(), "enrolled Cloud Store required");
    if matches!(
        store.state.private_agent_ledger,
        Some(PrivateAgentState::V2(_))
    ) {
        return Ok("private-runtime-v2");
    }
    let caller = store
        .agent_caller(key)
        .map_err(|error| anyhow!("owner device verification failed: {error}"))?;
    if store.state.private_agent_ledger.is_none() {
        store
            .activate_private_agent_ledger(&caller)
            .map_err(|error| anyhow!("private ledger selection failed: {error:?}"))?;
    }
    let caller = store
        .agent_caller(key)
        .map_err(|error| anyhow!("owner device verification failed: {error}"))?;
    store
        .activate_private_agent_runtime_v2(&caller)
        .map_err(|error| anyhow!("private runtime selection failed: {error:?}"))?;
    Ok("private-runtime-v2")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn delivery_store(dir: &Path, root: &SigningKey, device: &SigningKey) {
        let mut store = crate::Store::open(dir, "operator-select").unwrap();
        let enrollment = crate::tests::approved(store.inspect(10).unwrap(), root, device);
        store.apply(&enrollment, 11).unwrap();
        store
            .activate_delivery_storage(qos_relay::clock::now_ns())
            .unwrap();
    }

    #[test]
    fn operator_selects_private_runtime_for_the_enrolled_owner_only_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[41; 32]);
        let device = SigningKey::from_bytes(&[42; 32]);
        delivery_store(dir.path(), &root, &device);
        let stranger = hex::encode(SigningKey::from_bytes(&[43; 32]).verifying_key().to_bytes());
        assert!(select_private_agent_runtime(dir.path(), "operator-select", &stranger).is_err());
        assert!(select_private_agent_runtime(dir.path(), "operator-select", "not-hex").is_err());
        assert!(select_private_agent_runtime(dir.path(), "other-machine", &stranger).is_err());
        let owner = hex::encode(device.verifying_key().to_bytes());
        assert_eq!(
            select_private_agent_runtime(dir.path(), "operator-select", &owner).unwrap(),
            "private-runtime-v2"
        );
        let before = std::fs::read(dir.path().join("state.json")).ok();
        assert_eq!(
            select_private_agent_runtime(dir.path(), "operator-select", &owner).unwrap(),
            "private-runtime-v2"
        );
        assert_eq!(std::fs::read(dir.path().join("state.json")).ok(), before);
        let store = crate::delivery::access::try_open(dir.path(), "operator-select").unwrap();
        assert!(matches!(
            store.state.private_agent_ledger,
            Some(PrivateAgentState::V2(_))
        ));
    }

    #[test]
    fn operator_selection_refuses_a_held_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[51; 32]);
        let device = SigningKey::from_bytes(&[52; 32]);
        delivery_store(dir.path(), &root, &device);
        let _held = crate::delivery::access::try_open(dir.path(), "operator-select").unwrap();
        let owner = hex::encode(device.verifying_key().to_bytes());
        let error =
            select_private_agent_runtime(dir.path(), "operator-select", &owner).unwrap_err();
        assert!(error.to_string().contains("busy"), "{error}");
    }
}
