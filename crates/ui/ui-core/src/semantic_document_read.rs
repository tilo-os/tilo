//! App reader registration and the SDK's committed-read admission. An app registers a
//! reader: a closed request decoder, an exact result schema and a projection over its
//! document at given heads. The SDK alone runs a selected reader, and only over a
//! document it can identify as the caller's canonical record: the record store's current
//! loan of that exact document for a hosted invocation (whose route the scope verified
//! against the owner), or the app's mounted primary document on its current primary
//! route.
use automerge::{AutoCommit, transaction::Transactable};
use tilo_actions::{ActionSpec, EncodedAction, ResourceSpec};
use tilo_sort::CompactSort as _;

pub mod retirement;

pub(crate) use tilo_ui_base::semantic_readers::registered_in_tree;
pub use tilo_ui_base::{
    semantic_readers::Declared, semantic_readers::MAX_CURSOR_BYTES,
    semantic_readers::MAX_REQUEST_BYTES, semantic_readers::NoReadParams,
    semantic_readers::ReadName, semantic_readers::ReadPage, semantic_readers::ReadProjection,
    semantic_readers::RegisteredSemanticReader, semantic_readers::SemanticReadError,
    semantic_readers::SemanticReaders, semantic_readers::decode_cursor,
    semantic_readers::decode_request, semantic_readers::encode_cursor,
    semantic_readers::read_action, semantic_readers::registers_readers,
    semantic_readers::result_schema,
};

/// A borrowed selection from the actual app table and exact current resource.
pub(crate) struct SelectedSemanticRead<'a> {
    registration: &'a RegisteredSemanticReader,
    resource: &'a ResourceSpec,
    spec: &'a ActionSpec,
    encoded: &'a EncodedAction,
}

pub(crate) fn select<'a>(
    table: &'a [RegisteredSemanticReader],
    resource: &'a ResourceSpec,
    spec: &'a ActionSpec,
    encoded: &'a EncodedAction,
) -> Result<Option<SelectedSemanticRead<'a>>, SemanticReadError> {
    let mut matching = table.iter().filter(|entry| {
        Some(entry.operation_type) == spec.operation_type.as_deref()
            || entry.operation_type == encoded.type_id
    });
    let Some(registration) = matching.next() else {
        return Ok(None);
    };
    if registration.operation_type.is_empty() || matching.next().is_some() {
        return Err(SemanticReadError::InvalidRegistration);
    }
    let selected = SelectedSemanticRead {
        registration,
        resource,
        spec,
        encoded,
    };
    selected.check_contract()?;
    if encoded.payload.len() > MAX_REQUEST_BYTES {
        return Err(SemanticReadError::Limit);
    }
    (registration.validate)(resource, &encoded.payload)?;
    Ok(Some(selected))
}

impl SelectedSemanticRead<'_> {
    fn check_contract(&self) -> Result<(), SemanticReadError> {
        use tilo_actions::{ActionKind, ExecutionMode, OperationKind, ResultSpec};
        if !self
            .resource
            .actions
            .iter()
            .any(|actual| std::ptr::eq(actual, self.spec))
            || self.spec.kind != OperationKind::Effect
            || self.spec.operation_type.as_deref() != Some(self.registration.operation_type)
            || self.encoded.type_id != self.registration.operation_type
            || self.spec.contract.kind != ActionKind::Read
            || self.spec.contract.execution != ExecutionMode::Immediate
            || !matches!(&self.spec.contract.result, ResultSpec::Json { schema } if schema == (self.registration.schema)())
        {
            return Err(SemanticReadError::InvalidContract);
        }
        Ok(())
    }

    /// Run the reader over the caller's canonical record at its committed heads.
    /// `document` must be the record store's current loan of the record or the app's mounted
    /// primary document.
    pub(crate) fn read(
        &self,
        document: &AutoCommit,
    ) -> Result<tilo_actions::ActionOutput, SemanticReadError> {
        self.check_contract()?;
        let record = crate::invocation_scope::identified_resource(document)
            .ok_or(SemanticReadError::MissingQualifiedOwner)?;
        if document.pending_ops() != 0 {
            return Err(SemanticReadError::Pending);
        }
        let heads = document.base_heads();
        let page = (self.registration.project)(document, &heads, &self.encoded.payload)?;
        let mut hex: Vec<String> = heads.iter().map(ToString::to_string).collect();
        hex.compact_sort();
        let result = serde_json::json!({
            "version": 1,
            "record": {
                "workspaceId": record.workspace.map(|workspace| workspace.to_string()),
                "appId": record.app_id,
                "recordId": record.document_id,
            },
            "basis": { "heads": hex },
            "content": page.content,
            "nextCursor": page.next_cursor,
        });
        Ok(tilo_actions::ActionOutput {
            value: Some(result),
            resources: Vec::new(),
        })
    }
}

/// Inert descriptors for reads registered by the actual retained widget tree.
pub fn registered_operations(
    widget: &dyn tilo_ui_base::widget::Widget,
    resources: &[ResourceSpec],
) -> Vec<tilo_actions::ActionDescriptor> {
    fn collect(
        widget: &dyn tilo_ui_base::widget::Widget,
        resource: &ResourceSpec,
        addresses: &mut std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for spec in &resource.actions {
            let Some(type_id) = spec.operation_type.as_deref() else {
                continue;
            };
            if type_id.is_empty() {
                continue;
            }
            let Some(table) = registered_in_tree(widget, type_id) else {
                continue;
            };
            let mut matching = table
                .iter()
                .filter(|reader| reader.operation_type == type_id);
            let Some(registration) = matching.next() else {
                continue;
            };
            if matching.next().is_some() {
                continue;
            }
            let encoded = EncodedAction {
                type_id: type_id.to_owned(),
                payload: Vec::new(),
            };
            if (SelectedSemanticRead {
                registration,
                resource,
                spec,
                encoded: &encoded,
            })
            .check_contract()
            .is_ok()
            {
                addresses.insert(tilo_actions::ActionAddress::new(
                    resource.id.as_str(),
                    &spec.id,
                ));
            }
        }
        for child in &resource.children {
            collect(widget, child, addresses);
        }
    }
    let mut addresses = std::collections::HashSet::new();
    for resource in resources {
        collect(widget, resource, &mut addresses);
    }
    tilo_actions::api::survey(resources)
        .into_iter()
        .filter(|descriptor| addresses.contains(&descriptor.address))
        .collect()
}

#[cfg(test)]
mod tests;
