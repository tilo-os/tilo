use automerge::AutoCommit;

use super::{DocumentCheckout, ResourceOpenError, ResourceStore, portable, validate_identity};

impl ResourceStore {
    /// Read a local document for recovery without taking a writer or asking a peer for it.
    pub fn retained_document(
        &self,
        want: &DocumentCheckout,
    ) -> Result<Option<AutoCommit>, ResourceOpenError> {
        if !want.is_valid_local_storage_request() {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        let held = self.read_checked(&want.id, Clone::clone);
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        let doc = match held {
            Ok(doc) => doc,
            Err(ResourceOpenError::MissingResource) => {
                let bytes = if let Some(cache) = &set.portable {
                    let key = portable::key(&set.app_id, &set.home, &want.id);
                    cache.get(&key).map(|value| value.bytes)
                } else {
                    let (backend, key) = set.canonical_key(want)?;
                    backend.read(&key)?.map(|value| value.bytes)
                };
                let Some(bytes) = bytes else {
                    return Ok(None);
                };
                let options =
                    automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Error);
                AutoCommit::load_with_options(&bytes, options).map_err(|error| {
                    ResourceOpenError::Initialization(format!("the retained document: {error}"))
                })?
            }
            Err(error) => return Err(error),
        };
        validate_identity(&doc, want, &set.home)?;
        Ok(Some(doc))
    }
}
