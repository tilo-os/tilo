//! A record's first read from an admitted remote history: the native canonical snapshot merges
//! into this device's local cache and continues there with a background writer. Every graph it
//! merges and writes passes the ordinary identity validator (`validate`). Only the previously
//! stored heads are durable; the merged graph stays a pending write.

use tilo_automerge::ApplyIncoming;

pub(super) fn finish(
    mut store: app_state::DocStore,
    read: crate::collaboration::CommittedDocument,
    validate: impl Fn(&automerge::AutoCommit) -> Result<(), String>,
) -> Result<(automerge::AutoCommit, app_state::DocWriter), String> {
    let mut incoming = read.with_mutation(|mutation| {
        let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
            return Err("native canonical snapshot missing".to_string());
        };
        let doc = automerge::AutoCommit::load(bytes).map_err(|error| error.to_string())?;
        validate(&doc).map_err(|error| format!("incoming canonical history refused: {error}"))?;
        Ok::<_, String>(doc)
    })??;
    let loaded = store.load_complete().map_err(|error| error.to_string())?;
    let doc = if let Some(mut existing) = loaded {
        validate(&existing)
            .map_err(|error| format!("existing canonical cache refused: {error}"))?;
        existing
            .merge_incoming(&mut incoming)
            .map_err(|error| error.to_string())?;
        validate(&existing).map_err(|error| format!("merged canonical cache refused: {error}"))?;
        existing
    } else {
        // The absence is solely in this local cache. Its content comes from
        // the already admitted remote read, not from this device.
        incoming.set_actor(store.actor());
        incoming
    };
    validate(&doc).map_err(|error| format!("final canonical history refused: {error}"))?;
    let mut writer = store.writer();
    if !writer.is_enabled() {
        return Err(writer
            .last_error()
            .unwrap_or_else(|| "canonical writer unavailable".to_string()));
    }
    writer.request(&doc);
    read.applied();
    Ok((doc, writer))
}
