//! A replica of a workspace another device hosts, from its join's answer.

use super::{DeviceCert, NeighbourMsg, RevocationList, StatePeer, SystemTime, err, peer_handle};
use crate::TransportOrigin;

impl StatePeer {
    /// [`Self::joined`] from a join's answer as it travels (a
    /// [`crate::join::JoinBootstrap`] or its owner form): the workspace, the
    /// `authority`, its `roster` and the `revocations` its governance holds.
    /// The `authority` is the device the join was answered by, no particular
    /// device of the workspace's owner: it is the replica's origin, and the
    /// replica names no owner until the workspace's governance arrives,
    /// which names the owner the replica then holds to. The replica admits
    /// app documents by exact writer; its governance follows from the
    /// answer's signed messages or the workspace's replay.
    ///
    /// # Errors
    /// A revocation does not verify, or [`Self::joined`] refuses the workspace.
    pub fn from_join(
        workspace: &str,
        authority: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: &[tilo_user_identity::RevocationEntry],
        now: SystemTime,
    ) -> Result<Self, String> {
        let mut list = RevocationList::new();
        for entry in revocations {
            list.insert(entry.clone()).map_err(err)?;
        }
        let mut peer = Self::joined(workspace, authority, roster, list, now)?;
        peer.select_exact_admissions().map_err(err)?;
        Ok(peer)
    }

    /// Decide the app document `message`, which the workspace's host relayed
    /// from `origin`, as this replica's governance decides it, without
    /// taking it in: for a device that keeps the workspace's records itself and
    /// needs the workspace's verdict on each write first. `Ok(true)` admits it;
    /// `Ok(false)` holds it until what it is decided against is here (the
    /// workspace's governance, its author's certificate, its app's surface).
    /// A retained write is decided as of when it was signed.
    pub fn decide_relayed_document(
        &self,
        origin: TransportOrigin,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Result<bool, String> {
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return Err("not a document message".into());
        };
        let (source, retained) = match origin {
            TransportOrigin::ServerDelta(source) => (source, false),
            TransportOrigin::RetainedServerDelta(source) => (source, true),
            _ => return Err("a relayed document comes from the workspace's host".into()),
        };
        if *cluster_id != self.core.workspace
            || *peer != peer_handle(&source)
            || envelope.author.as_bytes() != &source
        {
            return Err("transport author or document workspace mismatch".into());
        }
        collab_types::verify_op(envelope).map_err(err)?;
        if !collab_types::names_document(app_id, cluster_id, document_id, envelope) {
            return Err("artifact does not match route".into());
        }
        if self.core.workspace_doc.is_none() || !self.core.certificates.contains_key(&source) {
            return Ok(false);
        }
        let at = if retained { envelope.timestamp } else { now };
        Ok(matches!(
            self.authorize_document(&source, app_id, document_id, envelope, at, retained)?,
            super::authority::Authorized::Admit { .. }
        ))
    }
}

impl StatePeer {
    /// `own`'s copy of `workspace` as `governance` (encoded neighbour
    /// messages, as a join or membership answer carries it) makes it, taken on
    /// its word: what a device the workspace does not list yet writes its own
    /// admission on ([`crate::own_admission`]). Nothing here is trusted, and
    /// nothing is kept: every receiver of what is written on it judges that
    /// against its own copy.
    ///
    /// # Errors
    /// The messages carry no governance of `workspace`, or it does not read.
    pub(crate) fn unanchored(
        workspace: &str,
        own: &DeviceCert,
        governance: &[Vec<u8>],
        now: SystemTime,
    ) -> Result<Self, String> {
        let mut peer = Self::joined(
            workspace,
            own.clone(),
            Vec::new(),
            RevocationList::new(),
            now,
        )?;
        peer.write_as_device(own.device_public_key);
        let mut changes = Vec::new();
        for bytes in governance {
            if let NeighbourMsg::SyncResp {
                changes: carried,
                workspace: named,
            } = NeighbourMsg::decode(bytes).map_err(err)?
            {
                if named != peer.core.workspace {
                    return Err("the governance is another workspace's".into());
                }
                changes.extend(carried);
            }
        }
        let document = match crate::workspace::base::lineage(None, &changes) {
            crate::workspace::base::Lineage::Same { changes, .. } => {
                crate::workspace::candidate(None, &changes)
            }
            crate::workspace::base::Lineage::Newer { base, changes } => {
                crate::workspace::candidate_on(*base, &changes)
            }
            crate::workspace::base::Lineage::Refounded => {
                return Err("the governance starts the workspace over".into());
            }
        }
        .map_err(err)?;
        let document: super::CachedDocument = document.into();
        peer.core.owner = super::receiving::listed_owner(&document, &peer.core.workspace)
            .ok_or("the workspace's governance names no owner")?;
        // The device holds its own certificate only where the workspace lists
        // it: a device it does not list yet writes its own row.
        peer.core.certificates.remove(&own.device_public_key);
        // The workspace's rows are its evidence, as a join's roster is ([`Self::joined`]): a
        // certificate that ran out since its row was written, the device's own included, is
        // held as history, so the device's own row renews it ([`super::certificate_renewal`])
        // and expiry stays the check on what each device does.
        for row in document.identities().values() {
            let Ok(row) = row else { continue };
            let held = crate::verified_certificate::VerifiedCertificate::historical_typed(
                row.device_cert.clone(),
            )
            .map_err(err)?;
            peer.core
                .certificates
                .insert(row.device_cert.device_public_key, held);
        }
        peer.import_device_rows(&document, now)?;
        peer.core.workspace_doc = Some(document);
        Ok(peer)
    }

    /// [`Self::admit_own`] of the admission `redemption`'s device signed,
    /// taken in only when it redeems that very invitation, for that device and
    /// request, and lets the device in; nothing changes otherwise.
    ///
    /// # Errors
    /// The workspace refuses the changes, or they admit anything else.
    pub fn admit_own_redemption(
        &mut self,
        redemption: &crate::join::VerifiedRedemption,
        changes: Vec<Vec<u8>>,
        now: SystemTime,
    ) -> Result<(), String> {
        let cert = redemption.certificate();
        let mut next = self.clone();
        if !next.admit_own(cert, changes, now)? {
            return Err("the admission does not let its device in".into());
        }
        let redeemed = next
            .core
            .workspace_doc
            .as_ref()
            .ok_or("the workspace's governance is missing")?
            .redemptions()
            .map_err(ToOwned::to_owned)?
            .get(&redemption.join().invite_id())
            .is_some_and(|receipts| {
                receipts.iter().any(|receipt| {
                    receipt.device_public_key == cert.device_public_key
                        && receipt.request_id == redemption.request_id()
                })
            });
        if !redeemed {
            return Err("the admission redeems another invitation than the one proven".into());
        }
        *self = next;
        Ok(())
    }

    /// Take in `changes`, `device`'s own admission to the workspace
    /// ([`crate::own_admission::sign_own_admission`],
    /// [`crate::own_admission::sign_further_device_row`]), as every receiver
    /// does: under `device` as their sender, which holds no role here yet.
    /// Whether the workspace now lists `device` under a person holding a role.
    ///
    /// # Errors
    /// The workspace refuses the changes.
    pub fn admit_own(
        &mut self,
        device: &DeviceCert,
        changes: Vec<Vec<u8>>,
        now: SystemTime,
    ) -> Result<bool, String> {
        let workspace = self.core.workspace.clone();
        self.receive_from(
            TransportOrigin::ServerDelta(device.device_public_key),
            NeighbourMsg::SyncResp { changes, workspace },
            now,
        )
        .map_err(|error| error.to_string())?;
        Ok(self
            .certificate(&device.device_public_key, now)
            .is_ok_and(|held| held == device)
            && self.role(&device.device_public_key, now).is_ok())
    }

    /// Take in `changes`, a governance change `device` signed itself
    /// ([`crate::own_admission::sign_command`]), as every receiver does: under
    /// `device` as their sender, judged by its own place in the workspace.
    /// Whether they changed anything.
    ///
    /// # Errors
    /// The workspace refuses any of them, or they build on history this copy lacks: nothing of
    /// them is taken in then.
    pub fn take_signed(
        &mut self,
        device: &DeviceCert,
        changes: Vec<Vec<u8>>,
        now: SystemTime,
    ) -> Result<bool, String> {
        let workspace = self.core.workspace.clone();
        let mut next = self.clone();
        let outcome = next
            .receive_from(
                TransportOrigin::ServerDelta(device.device_public_key),
                NeighbourMsg::SyncResp { changes, workspace },
                now,
            )
            .map_err(|error| error.to_string())?;
        if outcome.rejected > 0 {
            return Err("the workspace refuses the change the device signed".into());
        }
        if outcome.deferred || outcome.lacks_history {
            return Err(
                "the device signed its change on a member list this copy does not hold yet".into(),
            );
        }
        let took = outcome.changed || outcome.metadata_changed;
        *self = next;
        Ok(took)
    }
}
