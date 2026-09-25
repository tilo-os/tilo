//! Temporary immutable roots; byte custody, never snapshot authority.
use super::*;
use crate::publication_custody::{PublicationCustody, PublicationCustodyFailure};
use tilo_relay::delivery_v3::commit::{MAX_REFERENCES, PublicationRef};
/// Native generation remains an owner fact; the neutral holder conveys bytes only.
pub(crate) struct OwnedPublicationPin {
    custody: PublicationCustody,
    generation: u64,
}
impl OwnedPublicationPin {
    pub(crate) fn len(&self) -> usize {
        self.custody.len()
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn same_pool(&self, pool: &HostBytes) -> bool {
        self.custody.same_pool(pool)
    }
    pub(crate) fn reference(&self, index: usize) -> Option<&PublicationRef> {
        self.custody.reference(index)
    }
    pub(crate) fn get(&self, index: usize) -> Option<PublicationView<'_>> {
        self.custody.get(index)
    }
    pub(crate) fn custody(&self) -> &PublicationCustody {
        &self.custody
    }
    pub(crate) fn into_custody(self) -> PublicationCustody {
        self.custody
    }
}
impl SessionOwner {
    pub(crate) fn pin_publication(&self, host: &HostBytes) -> DataResult<OwnedPublicationPin> {
        let PublicationStorage::V5 { roots, .. } = &self.aggregate.publication else {
            return Err(OwnerDataFailure::new(
                Failure::Unavailable,
                anyhow::anyhow!("V5 publication roots required"),
            ));
        };
        let count = add(roots.baseline.len(), roots.tail.len())?;
        self.pin_references(
            host,
            (0..count).map(|i| {
                if i < roots.baseline.len() {
                    &roots.baseline[i]
                } else {
                    &roots.tail[i - roots.baseline.len()]
                }
            }),
            4096,
        )
    }
    pub(in crate::owner) fn pin_snapshot_publications<'a>(
        &self,
        host: &HostBytes,
        references: impl ExactSizeIterator<Item = &'a PublicationRef>,
    ) -> DataResult<OwnedPublicationPin> {
        self.pin_references(host, references, MAX_REFERENCES)
    }
    fn pin_references<'a>(
        &self,
        host: &HostBytes,
        references: impl ExactSizeIterator<Item = &'a PublicationRef>,
        limit: usize,
    ) -> DataResult<OwnedPublicationPin> {
        self.require_owner_resource_scope()
            .map_err(|e| self.classify_data_error(e))?;
        self.require_delivery()
            .map_err(|e| self.classify_data_error(e))?;
        let resources = self
            .resources
            .as_ref()
            .filter(|r| r.budget.same_pool(host))
            .ok_or_else(|| {
                OwnerDataFailure::new(
                    Failure::Unavailable,
                    anyhow::anyhow!("publication pin requires original attached host pool"),
                )
            })?;
        let PublicationStorage::V5 { pool, .. } = &self.aggregate.publication else {
            unreachable!()
        };
        let count = references.len();
        if count == 0 || count > limit {
            return Err(capacity());
        }
        let custody =
            PublicationCustody::from_native(pool, references, host, resources.retained.clone())
                .map_err(|error| match error {
                    PublicationCustodyFailure::Capacity => capacity(),
                    PublicationCustodyFailure::InvalidPayload => {
                        self.classify_data_error(error.into())
                    }
                })?;
        Ok(OwnedPublicationPin {
            custody,
            generation: self.committed_generation(),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // Byte custody only: repeated references deliberately prove no native
    // publication selection or snapshot-authority claim.
    #[test]
    fn publication_snapshot_backing_pins_bound_reference_metadata_without_payload_copy() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        owner.activate_delivery().unwrap();
        let (ingress, _rx) = crate::https::ingress_channel();
        let pool = ingress.host_bytes();
        owner.attach_data_resources(&pool).unwrap();
        let current = crate::committed_authority::CommittedAuthority::new(&owner);
        let mut scope = owner
            .prepare_owner_scope(&pool, OwnerOperation::Export)
            .unwrap();
        let pin = owner
            .with_owner_scope(&mut scope, &current, |o| {
                let PublicationStorage::V5 { roots, .. } = &o.aggregate.publication else {
                    unreachable!()
                };
                let reference = &roots.baseline[0];
                assert!(
                    o.pin_snapshot_publications(
                        &pool,
                        std::iter::repeat_n(reference, MAX_REFERENCES + 1)
                    )
                    .is_err()
                );
                o.pin_snapshot_publications(&pool, std::iter::repeat_n(reference, MAX_REFERENCES))
            })
            .unwrap()
            .unwrap();
        assert_eq!(pin.len(), MAX_REFERENCES);
        assert_eq!(
            pin.get(0).unwrap().bytes.as_ptr(),
            pin.get(MAX_REFERENCES - 1).unwrap().bytes.as_ptr()
        );
    }
    #[test]
    fn publication_pin_keeps_original_payload_and_credit_after_owner_and_current_drop() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        owner.activate_delivery().unwrap();
        let (ingress, _rx) = crate::https::ingress_channel();
        let pool = ingress.host_bytes();
        let initial = pool.available_bytes();
        owner.attach_data_resources(&pool).unwrap();
        let current = crate::committed_authority::CommittedAuthority::new(&owner);
        let expected: Vec<_> = owner
            .outgoing()
            .iter()
            .map(|p| (p.bytes.to_vec(), p.kind, p.bytes.as_ptr()))
            .collect();
        let generation = owner.committed_generation();
        assert!(
            owner.pin_publication(&pool).is_err(),
            "unscoped root export must deny"
        );
        let mut scope = owner
            .prepare_owner_scope(&pool, OwnerOperation::Export)
            .unwrap();
        let pin = owner
            .with_owner_scope(&mut scope, &current, |o| o.pin_publication(&pool))
            .unwrap()
            .unwrap();
        drop(scope);
        drop(current);
        drop(owner);
        assert!(
            pool.available_bytes() < initial,
            "original payload credit must remain held"
        );
        assert_eq!(pin.generation(), generation);
        assert_eq!(pin.custody().get(0).unwrap().bytes.as_ptr(), expected[0].2);
        let before_transfer = pool.available_bytes();
        let pin = pin.into_custody();
        assert_eq!(
            pool.available_bytes(),
            before_transfer,
            "consuming native handoff must not release or recharge original backing"
        );
        assert_eq!(pin.len(), expected.len());
        for (i, (bytes, kind, pointer)) in expected.iter().enumerate() {
            let p = pin.get(i).unwrap();
            assert_eq!(p.bytes, bytes);
            assert_eq!(&p.kind, kind);
            assert_eq!(p.bytes.as_ptr(), *pointer);
        }
        assert!(pin.get(pin.len()).is_none());
        drop(pin);
        assert_eq!(pool.available_bytes(), initial);
    }
    #[test]
    fn publication_pin_wrong_pool_and_legacy_storage_deny_without_allocation() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let (ingress, _rx) = crate::https::ingress_channel();
        let (foreign, _foreign_rx) = crate::https::ingress_channel();
        let pool = ingress.host_bytes();
        owner.attach_data_resources(&pool).unwrap();
        let current = crate::committed_authority::CommittedAuthority::new(&owner);
        let mut scope = owner
            .prepare_owner_scope(&pool, OwnerOperation::Export)
            .unwrap();
        let before = pool.available_bytes();
        owner
            .with_owner_scope(&mut scope, &current, |o| {
                assert!(o.pin_publication(&foreign.host_bytes()).is_err());
                assert!(o.pin_publication(&pool).is_err());
            })
            .unwrap();
        assert_eq!(pool.available_bytes(), before);
    }
}
