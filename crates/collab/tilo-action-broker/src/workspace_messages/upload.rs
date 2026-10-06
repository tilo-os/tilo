//! One charge follows actual retained allocations, including references held by
//! an in-flight owner call after its registry entry has been retired.
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(super) struct UploadBudget(Mutex<Usage>);

#[derive(Default)]
struct Usage {
    bytes: usize,
    uploads: usize,
    failed: bool,
}

pub(super) struct UploadCharge {
    budget: Arc<UploadBudget>,
    bytes: usize,
    upload: bool,
}

impl UploadBudget {
    /// Reserve before allocating, cloning, or decoding. Completed uploads keep
    /// their slot and bytes; temporary processing has an additional byte charge.
    pub(super) fn reserve(
        self: &Arc<Self>,
        bytes: usize,
        upload: bool,
    ) -> Result<UploadCharge, String> {
        let mut usage = self
            .0
            .lock()
            .map_err(|_| "Message upload budget unavailable")?;
        if usage.failed {
            return Err("Message upload budget is fenced".into());
        }
        let next = usage
            .bytes
            .checked_add(bytes)
            .filter(|next| *next <= runtime_collab::document_contribution::MAX_UPLOAD_BYTES)
            .ok_or("Message upload byte capacity exhausted")?;
        if bytes == 0
            || (upload && usage.uploads >= runtime_collab::document_contribution::MAX_REFERENCES)
        {
            return Err("Message upload capacity exhausted".into());
        }
        let uploads = usage
            .uploads
            .checked_add(usize::from(upload))
            .ok_or("Message upload count overflow")?;
        usage.bytes = next;
        usage.uploads = uploads;
        drop(usage);
        Ok(UploadCharge {
            budget: self.clone(),
            bytes,
            upload,
        })
    }
}

impl Drop for UploadCharge {
    fn drop(&mut self) {
        // Poison is not permission to release another upload's reservation.
        let mut usage = self
            .budget
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if usage.failed {
            return;
        }
        match (
            usage.bytes.checked_sub(self.bytes),
            usage.uploads.checked_sub(usize::from(self.upload)),
        ) {
            (Some(bytes), Some(uploads)) => {
                usage.bytes = bytes;
                usage.uploads = uploads;
            }
            _ => usage.failed = true,
        }
    }
}

pub(super) struct RetainedUpload {
    pub(super) source: Arc<super::source::MessageSource>,
    pub(super) total: usize,
    pub(super) bytes: Mutex<Vec<u8>>,
    _charge: UploadCharge,
}

impl RetainedUpload {
    pub(super) fn reserve(
        source: Arc<super::source::MessageSource>,
        total: usize,
        budget: &Arc<UploadBudget>,
    ) -> Result<Arc<Self>, String> {
        let charge = budget.reserve(total, true)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(|_| "Message upload allocation refused")?;
        Ok(Arc::new(Self {
            source,
            total,
            bytes: Mutex::new(bytes),
            _charge: charge,
        }))
    }

    pub(super) fn append(&self, offset: usize, chunk: &[u8]) -> Result<(), String> {
        let mut bytes = self
            .bytes
            .lock()
            .map_err(|_| "Message upload unavailable")?;
        if chunk.is_empty()
            || chunk.len() > runtime_collab::document_contribution::MAX_CHUNK_BYTES
            || offset != bytes.len()
            || offset
                .checked_add(chunk.len())
                .is_none_or(|next| next > self.total)
        {
            return Err("Message upload chunk is not contiguous or bounded".into());
        }
        bytes.extend_from_slice(chunk);
        drop(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_message_upload_charges_follow_in_flight_references() {
        let budget = Arc::new(UploadBudget::default());
        let bytes = runtime_collab::document_contribution::MAX_UPLOAD_BYTES;
        let retained = Arc::new(budget.reserve(bytes, true).unwrap());
        let processing = retained.clone();
        drop(retained);
        assert!(
            budget.reserve(1, true).is_err(),
            "retired registry reference must not release in-flight bytes"
        );
        assert!(
            budget.reserve(1, false).is_err(),
            "decoding must charge alongside retained bytes"
        );
        drop(processing);
        let replacement = budget.reserve(bytes, true).unwrap();
        assert_eq!(budget.0.lock().unwrap().bytes, bytes);
        drop(replacement);
        assert_eq!(budget.0.lock().unwrap().bytes, 0);
    }

    #[test]
    fn completed_message_reference_slots_remain_charged_until_release() {
        let budget = Arc::new(UploadBudget::default());
        let mut retained = Vec::new();
        for _ in 0..runtime_collab::document_contribution::MAX_REFERENCES {
            retained.push(budget.reserve(1, true).unwrap());
        }
        assert!(budget.reserve(1, true).is_err());
        let processing = budget.reserve(10, false).unwrap();
        assert_eq!(budget.0.lock().unwrap().bytes, retained.len() + 10);
        retained.pop();
        let replacement = budget.reserve(1, true).unwrap();
        drop(processing);
        drop(replacement);
        drop(retained);
        assert_eq!(budget.0.lock().unwrap().uploads, 0);
        assert_eq!(budget.0.lock().unwrap().bytes, 0);
    }
}
