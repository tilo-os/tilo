//! Literal pre-adapter policy algorithm, isolated test oracle; no shared qps helpers.
use super::{PlanningCapacity, PlanningFailure};
use anyhow::{Context, Result, bail};
use qos_peer_state::StatePeer;
use tilo_relay::delivery_v3::commit::Digest;
use tilo_user_identity::DeviceCert;
use ui::automerge::{ROOT, ReadDoc, ScalarValue, Value};
use web_time::SystemTime;
const POLICY_BYTES: usize = 1024 * 1024;
fn policy_pressure(error: impl Into<anyhow::Error>) -> anyhow::Error {
    {
        let _ = error.into();
        PlanningFailure::Capacity(PlanningCapacity::Bytes).into()
    }
}
fn policy_add(total: &mut usize, bytes: usize) -> Result<()> {
    *total = total
        .checked_add(bytes)
        .filter(|total| *total <= POLICY_BYTES)
        .ok_or_else(|| {
            policy_pressure(anyhow::anyhow!(
                "serialized policy exceeds source allowance"
            ))
        })?;
    Ok(())
}
fn policy_encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    encode_bounded(value, POLICY_BYTES).map_err(policy_pressure)
}
pub(super) fn encoded(
    peer: &StatePeer,
    workspace: u64,
    device: &DeviceCert,
    host: &DeviceCert,
    now: SystemTime,
) -> Result<Vec<u8>> {
    let room = peer
        .admitted_room()
        .context("delivery governance unavailable")?;
    let mut total = 0usize;
    let governor = match room.get(ROOT, "owner")? {
        Some((Value::Scalar(value), _)) => {
            let value = value.to_str().context("invalid governed owner")?;
            policy_add(&mut total, value.len())?;
            value.to_owned()
        }
        _ => bail!("invalid governed owner"),
    };
    // Consumer-scoped view (v2): other people's grants, certificates and
    // device revocations are excluded.
    let person = collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey(device.user_public_key),
    );
    let roots = [
        device.user_public_key,
        peer.authority().user_public_key,
        host.user_public_key,
    ];
    let devices = [
        device.device_public_key,
        peer.authority().device_public_key,
        host.device_public_key,
    ];
    // Do not use extract_governed_fields: it skips malformed capability rows.
    let mut caps = vec![];
    if let Some((Value::Object(ui::automerge::ObjType::Map), map)) =
        room.get(ROOT, "capabilities")?
    {
        for key in room.keys(&map) {
            policy_add(&mut total, key.len())?;
            let Some((Value::Scalar(value), _)) = room.get(&map, &key)? else {
                bail!("invalid governed capability")
            };
            let ScalarValue::Bytes(bytes) = value.as_ref() else {
                bail!("invalid governed capability")
            };
            if bytes.len() > POLICY_BYTES {
                return Err(policy_pressure(anyhow::anyhow!(
                    "policy row exceeds source allowance"
                )));
            }
            let cap: collab_types::caps::CapOverride = serde_json::from_slice(bytes)?;
            match &cap.scope {
                collab_types::caps::Scope::Workspace => (),
                collab_types::caps::Scope::Room(room) if room == peer.room() => (),
                _ => {
                    return Err(PlanningFailure::UnsupportedPolicy.into());
                }
            }
            let addressed = match &cap.subject {
                collab_types::caps::Subject::User(key) => {
                    collab_types::caps::Principal::person_from_key(*key) == person
                }
                collab_types::caps::Subject::Principal(
                    principal @ collab_types::caps::Principal::Person(_),
                ) => *principal == person,
                _ => true,
            };
            if !addressed {
                continue;
            }
            policy_add(
                &mut total,
                counted(&cap, POLICY_BYTES).map_err(policy_pressure)?,
            )?;
            caps.push((key, policy_encode(&cap)?));
        }
    } else if room.get(ROOT, "capabilities")?.is_some() {
        bail!("invalid governed capabilities")
    }
    caps.sort_unstable();
    let native_certificates: Vec<_> = peer
        .certificates()
        .into_iter()
        .filter(|cert| roots.contains(&cert.user_public_key))
        .collect();
    for certificate in &native_certificates {
        policy_add(
            &mut total,
            counted(certificate, POLICY_BYTES).map_err(policy_pressure)?,
        )?;
    }
    let mut certificates = native_certificates
        .iter()
        .map(policy_encode)
        .collect::<Result<Vec<_>>>()?;
    certificates.sort_unstable();
    let native_revocations: Vec<_> = peer
        .revocations()
        .into_iter()
        .filter(|entry| devices.contains(&entry.revoked_device_public_key))
        .collect();
    for revocation in &native_revocations {
        policy_add(
            &mut total,
            counted(revocation, POLICY_BYTES).map_err(policy_pressure)?,
        )?;
    }
    let mut revocations = native_revocations
        .iter()
        .map(policy_encode)
        .collect::<Result<Vec<_>>>()?;
    revocations.sort_unstable();
    // This slice has no partial visibility filter. Deny before revealing any group.
    for row in peer.rows() {
        peer.check_document_read(device.device_public_key, &row.app_id, &row.record_id, now)
            .map_err(|e| {
                let _ = e;
                anyhow::Error::from(PlanningFailure::CurrentDenied)
            })?;
    }
    let bytes = policy_encode(&(
        2_u8,
        peer.room(),
        workspace,
        peer.authority(),
        host,
        device,
        governor,
        certificates,
        revocations,
        caps,
    ))?;
    Ok(bytes)
}

fn counted<T: serde::Serialize>(value: &T, limit: usize) -> Result<usize> {
    Ok(postcard::serialize_with_flavor(
        value,
        Count { bytes: 0, limit },
    )?)
}
struct Count {
    bytes: usize,
    limit: usize,
}
impl postcard::ser_flavors::Flavor for Count {
    type Output = usize;
    fn try_push(&mut self, _: u8) -> postcard::Result<()> {
        self.try_extend(&[0])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= self.limit)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.bytes)
    }
}
fn encode_bounded<T: serde::Serialize>(value: &T, limit: usize) -> Result<Vec<u8>> {
    let len = counted(value, limit)?;
    let mut bytes = vec![0; len];
    let written = postcard::to_slice(value, &mut bytes)?.len();
    anyhow::ensure!(
        written == len && bytes.capacity() == len,
        "exact serialization size changed"
    );
    Ok(bytes)
}

pub(super) fn inspect(
    peer: &StatePeer,
    workspace: u64,
    device: &DeviceCert,
    host: &DeviceCert,
    now: SystemTime,
) -> Result<Digest> {
    let bytes = encoded(peer, workspace, device, host, now)?;
    Ok(*blake3::Hasher::new()
        .update(b"tilo-delivery-v3-room-policy-v2\0")
        .update(&bytes)
        .finalize()
        .as_bytes())
}
