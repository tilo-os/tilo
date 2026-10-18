//! Owner-private migration data and public opaque text bindings.
/// Previously extracted direction references, indexed by task and revision.
/// Only references backed by retained private text may be included.
pub type NativeTaskPrivateReferences =
    std::collections::BTreeMap<String, std::collections::BTreeMap<u64, String>>;

/// Text extracted from an older room document by its exact signing governor.
#[derive(Clone, Debug, Default)]
pub struct NativeTaskPrivateText {
    /// Full model/account snapshot recovered before replacing room history.
    pub model_authority: Option<tilo_desktop_model::task::WorkspaceTaskModelAuthority>,
    pub instruction: Option<String>,
    pub agent_request: Option<tilo_desktop_model::WorkspaceAgentRequest>,
    pub directions: std::collections::BTreeMap<u64, String>,
    /// Original automatic audit events recovered from current and retired task
    /// values. Owner-private archival data, not authenticated transcript messages:
    /// old events need not carry an invocation or an authenticated source device.
    pub historical_reports: Vec<serde_json::Value>,
    /// Exact current private confirmation, to pair with its sanitized metadata.
    pub confirmation: Option<tilo_desktop_model::WorkspaceTaskConfirmation>,
    /// Exact historical versions, including overwritten and deleted controls.
    /// Archive only: these values cannot authorize a current invocation.
    pub historical_confirmations: Vec<tilo_desktop_model::WorkspaceTaskConfirmation>,
    /// Original confirmation audit events, including old draft labels/reasons.
    pub historical_confirmation_audit: Vec<serde_json::Value>,
}
/// An opaque binding to a private model/account snapshot. Never return the
/// original object in a room projection, journal, or sanitized baseline.
///
/// # Errors
/// Returns an error if the contract cannot be serialized for its digest.
pub fn private_model_contract_reference(
    contract: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    if is_private_model_contract_reference(contract) {
        return Ok(contract.clone());
    }
    let bytes = serde_json::to_vec(contract).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "private_model_contract_digest": blake3::hash(&bytes).to_hex().to_string()
    }))
}

#[must_use]
pub fn is_private_model_contract_reference(contract: &serde_json::Value) -> bool {
    contract.as_object().is_some_and(|object| {
        object.len() == 1
            && object
                .get("private_model_contract_digest")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|digest| {
                    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
    })
}

/// Public replacement for private direction text, shared with command admission.
#[must_use]
pub fn private_instruction_reference(text: &str) -> String {
    format!(
        "private-instruction:{}",
        blake3::hash(text.as_bytes()).to_hex()
    )
}

/// Whether a label exactly matches the former automatically derived title.
/// Explicit labels, including shorter prefixes, are not private excerpts.
#[must_use]
pub fn legacy_private_task_title(title: &str, instruction: &str) -> bool {
    let derived: String = instruction
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .scan(0usize, |bytes, character| {
            *bytes = bytes.saturating_add(character.len_utf8());
            (*bytes <= 120).then_some(character)
        })
        .collect();
    !derived.is_empty() && title == derived
}
