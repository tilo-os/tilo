//! Explicit content copy between independent records; never merges source history.
use super::{
    AutoCommit, ChangeHash, HistoryRestoreSchema, PreparedContentRestore, ReadDoc, RestoreError,
    committed_heads, refuse_tables, schema, selection, sorted_heads, validated_content,
    write_and_check,
};
use tilo_automerge::ApplyIncoming;

/// Prepare a content-only import from an independently owned document.
/// Both exact frontiers must still match the reviewed values. Authorization,
/// identity discontinuity, archive verification and the final commit belong to the caller.
/// Source operations are used only in a disposable read model; the destination
/// receives newly authored values through the ordinary checked restore writer.
pub fn prepare_external_content_import(
    destination: &AutoCommit,
    source: &AutoCommit,
    schema: &'static HistoryRestoreSchema,
    destination_heads: &[ChangeHash],
    source_heads: &[ChangeHash],
) -> Result<PreparedContentRestore, RestoreError> {
    schema::validate(schema)?;
    let current_heads = sorted_heads(destination_heads);
    let target_heads = sorted_heads(source_heads);
    if committed_heads(destination)? != current_heads || committed_heads(source)? != target_heads {
        return Err(RestoreError::StaleReview);
    }
    refuse_tables(&mut destination.clone())?;
    let mut target_doc = source.clone();
    refuse_tables(&mut target_doc)?;
    if source.text_encoding() != destination.text_encoding() {
        let mut encoded = AutoCommit::new_with_encoding(destination.text_encoding());
        encoded
            .apply_incoming(target_doc.get_changes(&[]))
            .map_err(|error| RestoreError::read("external source", &error))?;
        target_doc = encoded;
    }
    let target = validated_content(&target_doc, schema)?;
    let current = validated_content(destination, schema)?;
    (schema.validate_result)(destination).map_err(RestoreError::SchemaInvalid)?;
    let (protected, protected_identities) =
        selection::protected(destination, schema, &target, None)?;
    let prepared = PreparedContentRestore {
        schema,
        current_heads,
        target_heads,
        current,
        target,
        protected,
        protected_identities,
        encoding: destination.text_encoding(),
    };
    if prepared.current != prepared.target {
        let mut trial = destination.clone();
        write_and_check(&mut trial, &prepared)?;
    }
    Ok(prepared)
}
