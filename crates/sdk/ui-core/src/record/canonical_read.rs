//! A record's first read from an admitted remote history: the native canonical snapshot merges
//! into this device's local cache and continues there with a background writer. The exceptional
//! first graph is admitted only by its live creation capture (`validate_created`); existing local
//! bytes always pass the ordinary identity validator (`validate`). Only the previously stored
//! heads are durable; the merged graph stays a pending write.

use tilo_automerge::ApplyIncoming;

pub(super) fn finish_created(
    mut store: app_state::DocStore,
    read: crate::collaboration::CommittedDocument,
    validate: impl Fn(&automerge::AutoCommit) -> Result<(), String>,
    validate_created: impl Fn(&automerge::AutoCommit) -> Result<(), String>,
) -> Result<(automerge::AutoCommit, app_state::DocWriter), String> {
    let incoming_validator = if read.creation_workspace().is_some() {
        &validate_created as &dyn Fn(&automerge::AutoCommit) -> Result<(), String>
    } else {
        &validate as &dyn Fn(&automerge::AutoCommit) -> Result<(), String>
    };
    let mut incoming = read.with_mutation(|mutation| {
        let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
            return Err("native canonical snapshot missing".to_string());
        };
        let doc = automerge::AutoCommit::load(bytes).map_err(|error| error.to_string())?;
        incoming_validator(&doc)
            .map_err(|error| format!("incoming canonical history refused: {error}"))?;
        Ok::<_, String>(doc)
    })??;
    let label = store.document_path().to_path_buf();
    app_state::validate_canonical_schema_of(&incoming, &label)
        .map_err(|error| error.to_string())?;
    let loaded = store.load_complete().map_err(|error| error.to_string())?;
    let mut doc = if let Some(mut existing) = loaded {
        validate(&existing)
            .map_err(|error| format!("existing canonical cache refused: {error}"))?;
        existing
            .merge_incoming(&mut incoming)
            .map_err(|error| error.to_string())?;
        validate(&existing).map_err(|error| format!("merged canonical cache refused: {error}"))?;
        existing
    } else {
        // The absence is solely in this local cache. Its content comes from
        // the already admitted remote Existing read, not a creation grant.
        incoming.set_actor(store.actor().clone());
        incoming
    };
    incoming_validator(&doc)
        .map_err(|error| format!("final canonical history refused: {error}"))?;
    app_state::validate_canonical_schema_of(&doc, &label).map_err(|error| error.to_string())?;
    if let Some(reason) = store.refusal() {
        return Err(reason.to_string());
    }
    let mut writer = store.writer();
    if !writer.is_enabled() {
        return Err(writer
            .last_error()
            .unwrap_or_else(|| "canonical writer unavailable".to_string()));
    }
    writer.request(&mut doc);
    read.applied();
    Ok((doc, writer))
}
