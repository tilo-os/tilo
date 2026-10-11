//! Inert contribution claims. Only the authenticated broker can resolve their
//! correlations to retained originals; the held owner decides actual coverage.
use super::{
    Arc, BTreeSet, ChangeHash, Contribution, DocumentAuthorization, DocumentAuthorizationInner,
    NativeInvocationReference, PublishedInvocationLease, RouteSpec,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeContributionClaim {
    pub wire_id: u64,
    pub invocation: [u8; 16],
    pub changes: Vec<[u8; 32]>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePublicationClaims {
    pub body: [u8; 32],
    pub from: Vec<[u8; 32]>,
    pub to: Vec<[u8; 32]>,
    pub exported: Vec<[u8; 32]>,
    pub contributors: Vec<NativeContributionClaim>,
}

/// Projection of the actual validated durable document receipt. The broker
/// constructs this only after its owner receipt binds the exact signed write.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NativeDocumentReceipt {
    pub room: String,
    pub app: String,
    pub record: String,
    pub basis: [u8; 32],
    pub request_digest: [u8; 32],
    pub generation: u64,
    pub aggregate_hash: [u8; 32],
}

impl NativePublicationClaims {
    pub const MAX_BYTES: usize = 1024 * 1024;
    /// Check the bounded inert claim shape, without granting authority.
    ///
    /// # Errors
    /// Refuses absent, repeated or oversized contribution groups and ranges.
    pub fn validate_bounds(&self) -> Result<(), String> {
        if self.contributors.is_empty()
            || self.contributors.len() > 64
            || self.from.len() > 8192
            || self.to.len() > 8192
            || self.exported.len() > 8192
            || self
                .contributors
                .iter()
                .any(|group| group.changes.is_empty() || group.changes.len() > 8192)
            || self
                .contributors
                .iter()
                .map(|group| group.changes.len())
                .sum::<usize>()
                > 8192
        {
            return Err("native publication lacks bounded original contributors".into());
        }
        let mut originals = BTreeSet::new();
        let mut changes = BTreeSet::new();
        for group in &self.contributors {
            if !originals.insert((group.wire_id, group.invocation))
                || group.changes.iter().any(|hash| !changes.insert(*hash))
            {
                return Err(
                    "native publication repeats original custody or contributed changes".into(),
                );
            }
        }
        Ok(())
    }
    /// Encode bounded claims for authenticated native publication.
    ///
    /// # Errors
    /// Refuses invalid claims, allocation failure or an oversized encoding.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate_bounds()?;
        super::encode_native_json(self, Self::MAX_BYTES)
    }
    /// Decode bounded inert claims; the broker must still retain originals.
    ///
    /// # Errors
    /// Refuses oversized, malformed or structurally invalid claims.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > Self::MAX_BYTES {
            return Err("native publication claims exceed bound".into());
        }
        let claims: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        claims.validate_bounds()?;
        Ok(claims)
    }
}

impl DocumentAuthorization {
    #[cfg(feature = "broker-transport")]
    pub(crate) fn native_publication_claims(&self) -> Result<NativePublicationClaims, String> {
        let contributors = self
            .0
            .contributions
            .iter()
            .map(|group| {
                let reference = group
                    .lease
                    .owner_token
                    .as_ref()
                    .and_then(|token| token.downcast_ref::<NativeInvocationReference>())
                    .ok_or("document contributor has no authenticated native reference")?;
                if reference.wire_id != group.lease.request.wire_id
                    || reference.invocation != *group.lease.request.invocation.0.as_bytes()
                {
                    return Err(
                        "native original reference differs from retained contributor".into(),
                    );
                }
                Ok(NativeContributionClaim {
                    wire_id: reference.wire_id,
                    invocation: reference.invocation,
                    changes: group.hashes.iter().map(|hash| hash.0).collect(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let claims = NativePublicationClaims {
            body: self.0.body,
            from: self.0.from.iter().map(|hash| hash.0).collect(),
            to: self.0.to.iter().map(|hash| hash.0).collect(),
            exported: self.0.exported.iter().map(|hash| hash.0).collect(),
            contributors,
        };
        claims.validate_bounds()?;
        Ok(claims)
    }

    /// Recover host-local evidence from actual retained original leases. Wire
    /// claims supply only body/range correlations; final-owner checks remain
    /// mandatory and use the broker's private original tokens, not this JSON.
    ///
    /// # Errors
    /// Refuses malformed claims, unavailable original leases or mismatched
    /// original delivery identities and document routes.
    pub fn from_native_publication(
        route: RouteSpec,
        claims: NativePublicationClaims,
        mut retain: impl FnMut(&NativeContributionClaim) -> Result<PublishedInvocationLease, String>,
    ) -> Result<Self, String> {
        claims.validate_bounds()?;
        let contributions = claims
            .contributors
            .iter()
            .map(|group| {
                let lease = retain(group)?;
                if lease.route != route
                    || lease.request.wire_id != group.wire_id
                    || lease.request.invocation.0.as_bytes() != &group.invocation
                {
                    return Err(
                        "retained native contributor differs from publication correlation".into(),
                    );
                }
                Ok(Contribution {
                    lease,
                    hashes: group.changes.iter().copied().map(ChangeHash).collect(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self(Arc::new(DocumentAuthorizationInner {
            route,
            body: claims.body,
            from: claims.from.into_iter().map(ChangeHash).collect(),
            to: claims.to.into_iter().map(ChangeHash).collect(),
            exported: claims.exported.into_iter().map(ChangeHash).collect(),
            contributions,
            #[cfg(feature = "broker-transport")]
            acknowledgements: Vec::new(),
        })))
    }

    #[cfg(feature = "broker-transport")]
    pub(crate) fn acknowledge_native_commit(&self) {
        for acknowledgement in &self.0.acknowledgements {
            acknowledgement.store(true, std::sync::atomic::Ordering::Release);
        }
    }
}
