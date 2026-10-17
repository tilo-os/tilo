//! The signed PCR11 prediction an installer checks before offering TPM
//! short-PIN unlock: the authority signed this generation's policy digest.
use base64::Engine as _;

/// Verify a prediction sidecar: generation `generation`, the sha256 bank,
/// exactly PCR 11, and an RSA PKCS#1 v1.5 SHA-256 signature by
/// `public_key_pem` over `approved_policy || be64(generation)`, the message
/// the build's prediction signer signs.
///
/// # Errors
///
/// Returns an error if the sidecar is malformed, describes another generation,
/// bank or PCR set, or its signature does not verify.
pub fn verify_pcr_prediction(
    public_key_pem: &str,
    prediction_json: &[u8],
    generation: u64,
) -> Result<(), String> {
    let prediction: serde_json::Value =
        serde_json::from_slice(prediction_json).map_err(|_| "prediction is not JSON")?;
    if prediction
        .get("generation")
        .and_then(serde_json::Value::as_u64)
        != Some(generation)
    {
        return Err(format!("prediction is not for generation {generation}"));
    }
    if prediction
        .get("pcr_bank")
        .and_then(serde_json::Value::as_str)
        != Some("sha256")
    {
        return Err("prediction is not for the sha256 PCR bank".into());
    }
    if prediction.get("pcrs") != Some(&serde_json::json!([11])) {
        return Err("prediction must cover exactly PCR 11".into());
    }
    let field = |name: &str| {
        let encoded = prediction
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("prediction has no {name}"))?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| format!("prediction {name} is not base64"))
    };
    let mut message = field("approved_policy")?;
    let signature = field("signature")?;
    message.extend_from_slice(&generation.to_be_bytes());
    crate::verify_rsa_sha256(public_key_pem, &message, &signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(generation: u64, edit: impl FnOnce(&mut serde_json::Value)) -> (String, Vec<u8>) {
        let authority = crate::generate_tpm_authority_key().expect("RSA authority");
        let approved: Vec<u8> = (0..32).collect();
        let mut message = approved.clone();
        message.extend_from_slice(&generation.to_be_bytes());
        let signature =
            crate::sign_rsa_sha256(authority.private_key_pem(), &message).expect("sign policy");
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut prediction = serde_json::json!({
            "generation": generation, "pcr_bank": "sha256", "pcrs": [11],
            "approved_policy": encode(&approved), "signature": encode(&signature),
        });
        edit(&mut prediction);
        (
            authority.public_key_pem().to_string(),
            serde_json::to_vec(&prediction).expect("JSON"),
        )
    }

    #[test]
    fn accepts_only_the_signed_generation_bank_and_pcr() {
        let (public, json) = signed(7, |_| {});
        verify_pcr_prediction(&public, &json, 7).expect("exact prediction");
        assert!(verify_pcr_prediction(&public, &json, 8).is_err());
        for edit in [
            |p: &mut serde_json::Value| p["pcrs"] = serde_json::json!([7]),
            |p: &mut serde_json::Value| p["pcrs"] = serde_json::json!([7, 11]),
            |p: &mut serde_json::Value| p["pcr_bank"] = "sha1".into(),
            |p: &mut serde_json::Value| p["generation"] = "7".into(),
            |p: &mut serde_json::Value| p["approved_policy"] = "not base64!".into(),
            |p: &mut serde_json::Value| p["signature"] = serde_json::Value::Null,
        ] {
            let (public, json) = signed(7, edit);
            assert!(verify_pcr_prediction(&public, &json, 7).is_err());
        }
    }

    #[test]
    fn the_generation_is_part_of_the_signed_message() {
        // A policy signed for generation 7, relabelled as generation 8.
        let (public, json) = signed(7, |p| p["generation"] = 8.into());
        let error = verify_pcr_prediction(&public, &json, 8).unwrap_err();
        assert!(error.contains("verification failed"), "{error}");
    }
}
