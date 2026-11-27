//! A site of another operator that keeps a person's workspace keeps the calls an agent asks
//! there for the person's root in that workspace's record of asked calls
//! ([`super::asked_kept`]), and writes nothing else of it.
//!
//! The site is no member of the workspace, so it writes nothing there as itself. As the person's
//! device places the workspace at the site, it grants the site, by a placement grant it signs as
//! one of their devices ([`tilo_user_identity::prepare_placing_grant_by`]), the one record of
//! asked calls ([`asks_records`]). The site certifies a key of its own as a device placed for that
//! record under the grant, again before each certificate ends, and writes the record as that
//! device: every device that keeps the workspace decides its writes by that certificate alone,
//! so any other record it writes is refused there, and once the person revokes the grant, so is
//! that one. What the site keeps is the agent's own signed call: the person's device that answers
//! it checks that signature, so the site carries an ask and never makes one up.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{DeviceCert, MAX_PLACED_DEVICE_LIFETIME_NS, PlacedRecords};

use super::{MemberDevice, Workspaces};

/// How long before a placed device's certificate ends the site certifies it again.
const RENEWED_BEFORE_NS: u64 = 10 * 60 * 1_000_000_000;

/// What a site is granted to place a device for, in `workspace`: its record of asked calls
/// alone.
///
/// # Errors
/// The workspace is the nil one.
pub fn asks_records(workspace: WorkspaceId) -> Result<PlacedRecords, String> {
    PlacedRecords::new(
        workspace.as_uuid().into_bytes(),
        [neighbour_wire::app_document_group(
            tilo_action_services::workspace_invocations::PROVIDER,
            &super::asked_kept::asked_document(),
        )],
    )
    .map_err(|error| error.to_string())
}

/// One workspace's placement at this site: the person's grant, the key the site places, and the
/// device it last certified that key as.
struct Placement {
    grant: DeviceCert,
    records: PlacedRecords,
    key: SigningKey,
    device: Option<Arc<MemberDevice>>,
}

/// Each workspace's placement at this site, by the workspace.
#[derive(Default)]
pub(super) struct AsksPlaced(Mutex<BTreeMap<String, Placement>>);

impl Workspaces {
    /// The grant this device, one of the person's own, gives the site `host` for `workspace`'s
    /// record of asked calls as it hands the workspace over, the record founded first so the
    /// site has one to write into; `None` where this device is no root-signed device of theirs.
    ///
    /// # Errors
    /// The record could not be founded, or the grant not signed.
    pub(crate) async fn grant_asks(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
        host: [u8; 32],
    ) -> Result<Option<DeviceCert>, String> {
        let device = self.signing_device();
        if !device.cert.is_root_signed_device() {
            return Ok(None);
        }
        super::asked_kept::found_record(self, workspace).await?;
        let mut grant = tilo_user_identity::prepare_placing_grant_by(
            &device.cert,
            host,
            asks_records(workspace_id)?,
            "Asked calls",
            tilo_clock::unix_now_ns(),
            None,
        );
        tilo_user_identity::sign_device_cert_async(&mut grant, device.signer.as_ref())
            .await
            .map_err(|error| format!("the site's grant could not be signed: {error}"))?;
        Ok(Some(grant))
    }

    /// Keep `grant`, which the person's device handing `workspace` over gave this site for its
    /// record of asked calls.
    ///
    /// # Errors
    /// The grant is none of this site's, names other records, or does not verify.
    pub(crate) fn keep_asks_grant(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
        grant: DeviceCert,
    ) -> Result<(), String> {
        let records = asks_records(workspace_id)?;
        if grant.device_public_key != self.device_cert().device_public_key
            || grant.capabilities.placing.as_ref() != Some(&records)
        {
            return Err("the grant is for another site or other records".to_owned());
        }
        tilo_user_identity::verify_placing_grant(&grant)
            .map_err(|error| format!("the grant does not verify: {error}"))?;
        self.inner.asks_placed.0.lock_recover().insert(
            workspace.to_owned(),
            Placement {
                grant,
                records,
                key: SigningKey::from_bytes(&rand::random()),
                device: None,
            },
        );
        Ok(())
    }

    /// The device this site writes `workspace`'s record of asked calls as, where it was granted
    /// that record ([`Self::keep_asks_grant`]), certified again when its certificate is about to
    /// end and taken into the workspace's copy here; `None` where it was granted none.
    ///
    /// # Errors
    /// Inside the `Some`: the device could not be certified or taken in.
    pub(crate) async fn asks_device(
        &self,
        workspace: &str,
    ) -> Option<Result<Arc<MemberDevice>, String>> {
        let now = tilo_clock::unix_now_ns();
        let (grant, records, key) = {
            let placed = self.inner.asks_placed.0.lock_recover();
            let placement = placed.get(workspace)?;
            if let Some(device) = &placement.device
                && device
                    .cert
                    .expires_at_ns
                    .is_some_and(|ends| ends.saturating_sub(now) > RENEWED_BEFORE_NS)
            {
                return Some(Ok(Arc::clone(device)));
            }
            let taken = (
                placement.grant.clone(),
                placement.records.clone(),
                placement.key.clone(),
            );
            drop(placed);
            taken
        };
        Some(
            self.certify_asks(workspace, &grant, records, key, now)
                .await,
        )
    }

    /// Certify `key` as the device placed for `records` of `workspace` under `grant`, from `now`,
    /// and take it into the workspace's copy here.
    async fn certify_asks(
        &self,
        workspace: &str,
        grant: &DeviceCert,
        records: PlacedRecords,
        key: SigningKey,
        now: u64,
    ) -> Result<Arc<MemberDevice>, String> {
        let ends = now
            .saturating_add(MAX_PLACED_DEVICE_LIFETIME_NS)
            .min(grant.expires_at_ns.unwrap_or(u64::MAX));
        let mut cert = tilo_user_identity::prepare_placed_device_cert(
            grant,
            key.verifying_key().to_bytes(),
            records,
            &format!("Asked calls at {}", self.device_cert().device_label),
            now.max(grant.issued_at_ns),
            ends,
        );
        tilo_user_identity::sign_device_cert_async(
            &mut cert,
            self.signing_device().signer.as_ref(),
        )
        .await
        .map_err(|error| format!("this site could not certify its asked calls: {error}"))?;
        let held = self
            .inner
            .workspaces
            .lock()
            .await
            .get(workspace)
            .cloned()
            .ok_or("this site does not keep the workspace")?;
        let (state, workspace_id) = {
            let held = held.lock().await;
            (held.peer.clone(), held.workspace_id())
        };
        let hello = neighbour_wire::NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert.clone()),
            revocations: Vec::new(),
        };
        state
            .write(|replica| {
                replica.receive_from(
                    tilo_peer_state::TransportOrigin::ServerDelta(cert.device_public_key),
                    hello.clone(),
                    tilo_clock::wall(),
                )
            })
            .map_err(|error| format!("the workspace refuses this site's asked calls: {error}"))?;
        // Every other device keeping the workspace decides the device's writes by the same
        // certificate, so it goes out ahead of them, and stays for a device that links later.
        self.inner
            .records
            .carry_all(workspace_id, cert.device_public_key, &[hello])?;
        let device = Arc::new(MemberDevice {
            cert,
            signer: Arc::new(key),
        });
        if let Some(placement) = self.inner.asks_placed.0.lock_recover().get_mut(workspace) {
            placement.device = Some(Arc::clone(&device));
        }
        Ok(device)
    }
}
