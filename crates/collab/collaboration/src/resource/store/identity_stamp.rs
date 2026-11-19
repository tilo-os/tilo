//! What a checked-out document says it is, kept when it is checked out: a later write that would
//! make it name another record or workspace is refused.

use automerge::{AutoCommit, ObjType, ROOT};

use super::{
    DocumentCheckout, META, ObjectAt, home, object_at, validate_identity, workspace_scope,
};

/// Whether the document carries its record metadata map.
fn record_metadata_present(doc: &AutoCommit) -> Result<bool, String> {
    match object_at(doc, &ROOT, META, ObjType::Map) {
        ObjectAt::Missing => Ok(false),
        ObjectAt::Object(_) => Ok(true),
        ObjectAt::Invalid => Err("conflicting record metadata".into()),
    }
}

#[derive(Clone, Debug)]
pub struct ReplicaIdentityStamp {
    checkout: DocumentCheckout,
    workspace: String,
    metadata: bool,
    home: Option<String>,
    /// The workspace the document names, as [`workspace_scope`] reads it.
    scope: Option<String>,
}

impl ReplicaIdentityStamp {
    pub(super) fn capture(
        doc: &AutoCommit,
        checkout: &DocumentCheckout,
        workspace: &str,
    ) -> Result<Self, String> {
        validate_identity(doc, checkout, workspace).map_err(|error| error.to_string())?;
        let metadata = record_metadata_present(doc)?;
        if !checkout.id.is_package_primary() && !metadata {
            return Err("record metadata is missing".into());
        }
        Ok(Self {
            checkout: checkout.clone(),
            workspace: workspace.to_string(),
            metadata,
            home: home(doc),
            scope: workspace_scope(doc),
        })
    }

    pub fn validate(&self, doc: &AutoCommit) -> Result<(), String> {
        validate_identity(doc, &self.checkout, &self.workspace)
            .map_err(|error| error.to_string())?;
        if record_metadata_present(doc)? != self.metadata
            || home(doc) != self.home
            || workspace_scope(doc) != self.scope
        {
            return Err("conflicting record identity".into());
        }
        Ok(())
    }
}
