//! Canonical evidence from a committed Store report, without catalogue claims.

/// Digest the report fields the host can independently recover from the
/// original committed document. Catalogue labels are deliberately excluded:
/// the application cannot turn its own trust claims into signed evidence.
///
/// # Errors
/// Returns an encoding error if the canonical evidence cannot be serialized.
pub fn report_evidence_digest(
    app_id: &str,
    status: &str,
    note: &str,
    case_id: Option<&str>,
) -> Result<String, postcard::Error> {
    let bytes = postcard::to_stdvec(&(
        "tilo.store.committed-report.v1",
        app_id,
        status,
        note,
        case_id,
    ))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}
