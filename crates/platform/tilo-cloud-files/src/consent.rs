//! Host-memory admission. Neither the issuer nor its permits enter app documents.
use std::collections::HashMap;

use crate::{DestinationIdentity, cloud::ImportEntry};

#[derive(PartialEq, Eq)]
pub(crate) struct ImportPayload {
    pub account: String,
    pub entries: Vec<ImportEntry>,
    pub folder: String,
    pub destination: DestinationIdentity,
    device: String,
}

impl ImportPayload {
    fn new(
        account: &str,
        entries: &[ImportEntry],
        folder: &str,
        destination: &DestinationIdentity,
        device: &str,
    ) -> Self {
        Self {
            account: account.into(),
            entries: entries.to_vec(),
            folder: folder.into(),
            destination: destination.clone(),
            device: device.into(),
        }
    }
}

/// A single-use, exact-payload grant for the import engine.
///
/// Only a host-owned [`ImportAuthority`] can construct this value. It is neither
/// cloneable nor serializable, and executing an import consumes it by value.
/// The engine reads its payload directly, so callers cannot substitute a source
/// or destination after admission.
pub struct ImportConsent {
    payload: ImportPayload,
}

impl ImportConsent {
    pub(crate) fn into_payload(self) -> ImportPayload {
        self.payload
    }
}

/// A host-private registry of local import approvals.
///
/// Keep this issuer in the host callback state, never in a replicated document.
/// Call `approve` only after local destination and device admission. Untrusted
/// job records are not approvals. Consumed job IDs remain reserved for this
/// issuer's lifetime, including after a mismatched consumption attempt.
#[derive(Default)]
pub struct ImportAuthority {
    approvals: HashMap<String, Option<ImportPayload>>,
}

impl ImportAuthority {
    /// Record the exact locally approved selection and destination.
    ///
    /// # Errors
    /// Rejects a job ID already approved or consumed by this issuer.
    pub fn approve(
        &mut self,
        job_id: &str,
        account: &str,
        entries: &[ImportEntry],
        folder: &str,
        destination: &DestinationIdentity,
        device: &str,
    ) -> Result<(), String> {
        use std::collections::hash_map::Entry;
        match self.approvals.entry(job_id.into()) {
            Entry::Occupied(_) => Err("cloud import job already admitted".into()),
            Entry::Vacant(slot) => {
                slot.insert(Some(ImportPayload::new(
                    account,
                    entries,
                    folder,
                    destination,
                    device,
                )));
                Ok(())
            }
        }
    }

    /// Consume a local approval, checking every field before minting a permit.
    ///
    /// # Errors
    /// Rejects missing, replayed or changed jobs. A mismatch burns the approval
    /// so a later retry cannot use an older local decision.
    pub fn consume(
        &mut self,
        job_id: &str,
        account: &str,
        entries: &[ImportEntry],
        folder: &str,
        destination: &DestinationIdentity,
        device: &str,
    ) -> Result<ImportConsent, String> {
        let payload = self
            .approvals
            .get_mut(job_id)
            .and_then(Option::take)
            .ok_or("cloud import has no unused local approval")?;
        if payload != ImportPayload::new(account, entries, folder, destination, device) {
            return Err("cloud import differs from its local approval".into());
        }
        Ok(ImportConsent { payload })
    }
}
