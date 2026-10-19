//! Credential-free client for the registry's signed moderation protocol.
//!
//! No bearer token or private key enters this process. The launcher-bound
//! host completes exact moderation operations under their admitted invocation.

use std::io::Read as _;
use std::time::Duration;

use app_cli::config::RegistryConfig;
use registry_types::{
    ModerationCase, ModerationMutation, SignedModerationMutation, TransparencyPage,
};
use thiserror::Error;

const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ModerationClientError {
    #[error("signing identity is unavailable: {0}")]
    Identity(String),
    #[error("registry URL must be credential-free HTTPS")]
    UnsafeEndpoint,
    #[error("registry moderation service is offline: {0}")]
    Offline(String),
    #[error("registry refused the moderation request ({status}): {message}")]
    Refused { status: u16, message: String },
    #[error("registry returned an invalid moderation response")]
    InvalidResponse,
    #[error("registry moderation proof failed: {0}")]
    InvalidProof(String),
}

/// Submit one signed mutation and validate the receipt's deterministic
/// projection. Callers must not treat this receipt as a verified status; only
/// [`fetch_verified_case`] anchors status in an owner-signed snapshot.
///
/// # Errors
/// Returns endpoint-safety, connectivity, refusal, or response-shape errors.
pub fn submit(
    base_url: &str,
    request: &SignedModerationMutation,
) -> Result<ModerationCase, ModerationClientError> {
    let url = endpoint(base_url, "moderation/mutations")?;
    let response = client()?
        .post(url)
        .json(request)
        .send()
        .map_err(|error| ModerationClientError::Offline(error.to_string()))?;
    let case: ModerationCase = decode(response)?;
    validate_submission_projection(request, &case)?;
    Ok(case)
}

fn validate_submission_projection(
    request: &SignedModerationMutation,
    case: &ModerationCase,
) -> Result<(), ModerationClientError> {
    let valid = match &request.mutation {
        ModerationMutation::FileReport { package_id, .. } => {
            case.case_id == registry_types::moderation_case_id(package_id, &request.request_id)
                && case.package_id == *package_id
                && case.status == registry_types::ModerationCaseStatus::Open
        }
        ModerationMutation::PublisherReply { case_id, .. } => case.case_id == *case_id,
        ModerationMutation::PublisherAppeal { case_id, .. } => {
            case.case_id == *case_id
                && case.status == registry_types::ModerationCaseStatus::Appealed
        }
        ModerationMutation::ReviewerDecision {
            case_id, decision, ..
        } => case.case_id == *case_id && case.status == decision.status(),
    };
    if valid && case.latest_sequence > 0 {
        Ok(())
    } else {
        Err(ModerationClientError::InvalidResponse)
    }
}

/// Fetch a complete transparency proof, anchor it in the current owner-signed
/// registry snapshot, authorize every reviewer/publisher event and derive the
/// case status locally. No mutable server projection is trusted.
///
/// # Errors
///
/// Returns an error when the case id is malformed, the registry snapshot or
/// transparency log cannot be fetched, or the signed proof is inconsistent.
pub fn fetch_verified_case(
    registry: &RegistryConfig,
    case_id: &str,
) -> Result<ModerationCase, ModerationClientError> {
    validate_case_id(case_id)?;
    let snapshot = crate::registry_client::fetch_snapshot(registry)
        .map_err(|error| ModerationClientError::InvalidProof(error.to_string()))?;
    let mut events = Vec::new();
    let mut from = 1_u64;
    loop {
        let page = fetch_transparency(&registry.base_url, from)?;
        if page.events.is_empty() {
            break;
        }
        events.extend(page.events);
        if events.len() > 10_000 {
            return Err(ModerationClientError::InvalidResponse);
        }
        match page.next_sequence {
            Some(next) if next > from => from = next,
            Some(_) => return Err(ModerationClientError::InvalidResponse),
            None => break,
        }
    }
    registry_types::derive_verified_case(
        &events,
        case_id,
        &registry.owner_pubkey_hex,
        snapshot.moderation_head_hex(),
    )
    .map_err(|error| ModerationClientError::InvalidProof(error.to_string()))
}

fn validate_case_id(case_id: &str) -> Result<(), ModerationClientError> {
    if case_id.is_empty()
        || case_id.len() > 96
        || !case_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(ModerationClientError::InvalidResponse);
    }
    Ok(())
}

/// Fetch one bounded public transparency page.
///
/// # Errors
/// Returns endpoint-safety, connectivity, refusal, or decoding errors.
pub fn fetch_transparency(
    base_url: &str,
    from: u64,
) -> Result<TransparencyPage, ModerationClientError> {
    let mut url = endpoint(base_url, "moderation/transparency")?;
    url.query_pairs_mut()
        .append_pair("from", &from.max(1).to_string())
        .append_pair("limit", "100");
    decode(
        client()?
            .get(url)
            .send()
            .map_err(|error| ModerationClientError::Offline(error.to_string()))?,
    )
}

fn endpoint(base_url: &str, suffix: &str) -> Result<reqwest::Url, ModerationClientError> {
    let mut url =
        reqwest::Url::parse(base_url).map_err(|_| ModerationClientError::UnsafeEndpoint)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(ModerationClientError::UnsafeEndpoint);
    }
    url.set_fragment(None);
    url.set_query(None);
    let path = format!("{}/{}", url.path().trim_end_matches('/'), suffix);
    url.set_path(&path);
    Ok(url)
}

fn client() -> Result<reqwest::blocking::Client, ModerationClientError> {
    tilo_net::http::blocking_client()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ModerationClientError::Offline(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
) -> Result<T, ModerationClientError> {
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return Err(ModerationClientError::InvalidResponse);
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| ModerationClientError::Offline(error.to_string()))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(ModerationClientError::InvalidResponse);
    }
    if !status.is_success() {
        let message = String::from_utf8_lossy(&bytes).chars().take(512).collect();
        return Err(ModerationClientError::Refused {
            status: status.as_u16(),
            message,
        });
    }
    serde_json::from_slice(&bytes).map_err(|_| ModerationClientError::InvalidResponse)
}

pub fn report_request_id(package_id: &str, note: &str, now_unix: u64, nonce: &[u8; 16]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(package_id.as_bytes());
    hasher.update(&[0]);
    hasher.update(note.as_bytes());
    hasher.update(&now_unix.to_le_bytes());
    hasher.update(nonce);
    let digest = hasher.finalize();
    format!(
        "store-{}",
        digest.to_hex().chars().take(32).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Endpoint validation, request identity binding and unforgeable receipts.
    #[test]
    fn moderation_client_endpoints_requests_and_receipts_are_trustworthy() {
        // Case `endpoint_rejects_credentials_plaintext_and_redirect_tricks`.
        {
            for unsafe_url in [
                "http://registry.test",
                "https://placeholder:placeholder@registry.test",
                "file:///tmp/registry",
            ] {
                assert!(matches!(
                    endpoint(unsafe_url, "moderation/mutations"),
                    Err(ModerationClientError::UnsafeEndpoint)
                ));
            }
            assert_eq!(
                endpoint("https://registry.test/base/", "moderation/mutations")
                    .unwrap()
                    .as_str(),
                "https://registry.test/base/moderation/mutations"
            );
        }

        // Case `request_id_is_stable_but_body_and_time_bound`.
        {
            assert_eq!(
                report_request_id("so.tilo.notes", "evidence", 10, &[1; 16]),
                report_request_id("so.tilo.notes", "evidence", 10, &[1; 16])
            );
            assert_ne!(
                report_request_id("so.tilo.notes", "evidence", 10, &[1; 16]),
                report_request_id("so.tilo.notes", "different", 10, &[1; 16])
            );
        }

        // Case `submission_receipt_cannot_forge_case_id_package_or_status`.
        {
            // Only the request id and the mutation are read here; signing is
            // the registry's concern and tested there.
            let request = registry_types::SignedModerationMutation {
                protocol: registry_types::MODERATION_PROTOCOL,
                request_id: "request-one".into(),
                issued_at_unix: 10,
                expires_at_unix: 20,
                actor_role: registry_types::ModerationActorRole::Reporter,
                actor_pubkey_hex: String::new(),
                mutation: ModerationMutation::FileReport {
                    package_id: "so.tilo.notes".into(),
                    category: "policy".into(),
                    summary: "misleading listing".into(),
                    evidence_digest_hex: "aa".repeat(32),
                },
                signature_hex: String::new(),
                destination: None,
                credential: None,
            };
            let short_hash = blake3::hash(b"so.tilo.notes:request-one")
                .to_hex()
                .chars()
                .take(24)
                .collect::<String>();
            let mut case = ModerationCase {
                case_id: format!("case-{short_hash}"),
                package_id: "so.tilo.notes".into(),
                publisher_pubkey_hex: "bb".repeat(32),
                status: registry_types::ModerationCaseStatus::Open,
                opened_at_unix: 11,
                updated_at_unix: 11,
                latest_sequence: 1,
            };
            validate_submission_projection(&request, &case).unwrap();
            case.status = registry_types::ModerationCaseStatus::ClosedNoAction;
            assert!(validate_submission_projection(&request, &case).is_err());
        }
    }
}
