//! Bilateral machine-mesh approvals signed by two enrolled Cloud owner roots.
//! Custody material stays inside the runtime; only the public signed request
//! (challenges, allowed actions and the two root signatures) leaves it.
use super::owner::CloudOwner;
use tilo_user_identity::machine_mesh::{
    BROKER_ACTIONS_SCOPE, MeshChallenge, MeshRequest, sign_request,
};

const MAX_MESH_JSON: usize = 20 * 1024;
const MAX_LINK_ID: usize = 128;
/// Broker actions the target may run for the source after ordinary permission checks.
const ALLOWED_ACTIONS: [&str; 1] = [BROKER_ACTIONS_SCOPE];

/// One side of a mesh approval: the selected machine and its owner custody.
pub(crate) struct MeshSide<'a> {
    pub machine_id: &'a str,
    pub material: &'a [u8],
}

fn bounded<T: serde::de::DeserializeOwned>(text: &str, what: &str) -> Result<T, String> {
    if text.is_empty() || text.len() > MAX_MESH_JSON {
        return Err(format!("Invalid native mesh {what} size"));
    }
    serde_json::from_str(text).map_err(|_| format!("Invalid native mesh {what}"))
}

fn link_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_LINK_ID
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    {
        return Err("Invalid machine link id".into());
    }
    Ok(())
}

/// Restores both sides as enrolled owners and signs with their roots. The
/// roots are dropped (and zeroized by `SigningKey`) before this returns.
fn sign(
    account_id: &str,
    source: &MeshSide<'_>,
    target: &MeshSide<'_>,
    request: &mut MeshRequest,
    now: u64,
) -> Result<String, String> {
    if source.machine_id == target.machine_id {
        return Err("Select two different machines".into());
    }
    if request.source.machine_id != source.machine_id
        || request.target.machine_id != target.machine_id
    {
        return Err("Native mesh challenge belongs to different machines".into());
    }
    let root = |side: &MeshSide<'_>| {
        CloudOwner::restore(side.material, account_id, side.machine_id, now)?
            .mesh_root_key()
            .map_err(|_| "Cloud owner identity is not enrolled for this machine".to_string())
    };
    let (source_root, target_root) = (root(source)?, root(target)?);
    sign_request(request, &source_root, &target_root)?;
    drop((source_root, target_root));
    serde_json::to_string(request).map_err(|error| error.to_string())
}

/// Signs a fresh `link` approval over both machines' native inspect challenges.
pub(crate) fn link(
    account_id: &str,
    source: &MeshSide<'_>,
    target: &MeshSide<'_>,
    source_challenge: &str,
    target_challenge: &str,
    link: &str,
    now: u64,
) -> Result<String, String> {
    link_id(link)?;
    let mut request = MeshRequest {
        version: 1,
        kind: "link".into(),
        link_id: link.into(),
        source: bounded::<MeshChallenge>(source_challenge, "challenge")?,
        target: bounded::<MeshChallenge>(target_challenge, "challenge")?,
        allowed_actions: ALLOWED_ACTIONS
            .iter()
            .map(|&action| action.into())
            .collect(),
        source_signature: Vec::new(),
        target_signature: Vec::new(),
    };
    sign(account_id, source, target, &mut request, now)
}

/// Signs an `unlink` of an existing grant. `source`/`target` follow the
/// grant's own orientation; its challenges are reused verbatim.
pub(crate) fn unlink(
    account_id: &str,
    source: &MeshSide<'_>,
    target: &MeshSide<'_>,
    grant: &str,
    now: u64,
) -> Result<String, String> {
    let grant: MeshRequest = bounded(grant, "link grant")?;
    if grant.version != 1 || grant.kind != "link" {
        return Err("Invalid native mesh link grant".into());
    }
    link_id(&grant.link_id)?;
    let mut request = MeshRequest {
        kind: "unlink".into(),
        source_signature: Vec::new(),
        target_signature: Vec::new(),
        ..grant
    };
    sign(account_id, source, target, &mut request, now)
}

#[cfg(test)]
#[path = "mesh_tests.rs"]
mod tests;
