//! Inert continuity promises supplied by an actual owning surface or provider.

/// Comparison-only coverage of an owning surface's effective invocation domain.
/// This never authorizes invocation, disclosure, or document publication.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActionDirectoryInvocationDomain {
    /// The provider has not established complete continuity evidence.
    #[default]
    Unsupported,
    /// All addresses, handlers and target semantics owned by this surface are
    /// represented by the newly materialized resources and their effective
    /// invocation evidence. There is no omitted lazy dispatch domain.
    MaterializedOnly,
    /// Versions the complete omitted domain: addresses/order/cursors, targets,
    /// encodings, argument resolution, handlers and captured state. A recreated
    /// search row count or an unrelated metadata generation is insufficient.
    Versioned([u8; 32]),
}

impl ActionDirectoryInvocationDomain {
    /// Derive an inert domain revision from explicit semantic evidence without
    /// copying its data or requiring a hashing dependency in the application.
    /// The caller must truthfully cover the complete domain described above.
    pub fn versioned(evidence: &tilo_actions::ContinuationEvidence) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.directory-invocation-domain\0v1");
        if evidence.write_canonical_identity(&mut hasher).is_err() {
            return Self::Unsupported;
        }
        Self::Versioned(*hasher.finalize().as_bytes())
    }

    pub(crate) fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}
