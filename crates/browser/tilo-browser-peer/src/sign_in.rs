//! What a browser gives a service that signs its person in.
//!
//! A service that is not one of the person's devices asks the page for two
//! things, and the page builds both here: a
//! [`SignInProof`](tilo_user_identity::SignInProof), in which a device the
//! person's root certified signs the service's fresh challenge, and — when
//! the person lets an agent act for them through that service — a
//! certificate of the key the service generated for the grant
//! ([`tilo_user_identity::DeviceCapabilities::agent_grant`]).
//!
//! The service decodes both as its canonical bytes (`tilo_user_identity::
//! canonical`) in base64url without padding, so that is what the page sends
//! and what builds them here.

use base64::Engine as _;
use tilo_relay_client::DeviceSigner;
use tilo_user_identity::canonical::canonical_bytes;
use tilo_user_identity::certification::MAX_CERTIFICATION_LABEL_BYTES;
use tilo_user_identity::{CanonicalError, DeviceCert, SignInError, SignInProof, sign_in_digest};

/// The device behind `signer`, which `cert` certifies, signs `challenge`
/// from `audience`: the proof with which a service signs its person in.
///
/// # Errors
/// `signer` is not the device `cert` certifies, it refused to sign, or the
/// audience is malformed ([`SignInError`]).
pub async fn sign_in_proof(
    cert: DeviceCert,
    signer: &dyn DeviceSigner,
    audience: &str,
    challenge: [u8; 32],
) -> Result<SignInProof, SignInError> {
    if signer.device_public_key() != cert.device_public_key {
        return Err(SignInError::Signature);
    }
    let digest = sign_in_digest(&cert, audience, &challenge)?;
    let signature = signer
        .sign(digest.to_vec())
        .await
        .map_err(|_| SignInError::Signature)?;
    Ok(SignInProof {
        cert,
        audience: audience.to_owned(),
        challenge,
        signature,
    })
}

/// `value`'s canonical bytes as base64url without padding: the text a
/// service decodes a proof, or a certificate, from.
///
/// # Errors
/// [`CanonicalError`] when `value` has no canonical encoding.
pub fn encode<T: serde::Serialize>(value: &T) -> Result<String, CanonicalError> {
    let bytes = canonical_bytes(value)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// What an agent's grant is called in its certificate: the client that asks,
/// through whose service, in at most
/// [`MAX_CERTIFICATION_LABEL_BYTES`] bytes of UTF-8, from text that is not
/// trusted to be printable. The service is named in full where a name and a
/// service fit together, and both are cut short where they do not, so a
/// label always says who holds the grant.
#[must_use]
pub fn grant_label(client: &str, audience: &str) -> String {
    let printable: String = client
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let who = if printable.trim().is_empty() {
        "An agent".to_owned()
    } else {
        printable
    };
    let host = cut(host(audience), MAX_CERTIFICATION_LABEL_BYTES / 2);
    let suffix = format!(" via {host}");
    let room = MAX_CERTIFICATION_LABEL_BYTES.saturating_sub(suffix.len());
    let who = cut(who.trim(), room);
    if who.is_empty() {
        suffix.trim_start().to_owned()
    } else {
        format!("{who}{suffix}")
    }
}

/// The host of an origin: what a label calls a service. The origin has been
/// checked by whoever asked (the route that builds a label skips what it
/// cannot check), so this only has to read one.
fn host(audience: &str) -> &str {
    let authority = audience
        .split_once("://")
        .map_or(audience, |(_scheme, rest)| rest);
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    authority
        .rsplit_once('@')
        .map_or(authority, |(_userinfo, host)| host)
}

/// `text`, cut to at most `max` bytes without splitting the character at the
/// cut, and without trailing space.
fn cut(text: &str, max: usize) -> String {
    let mut fitted = String::with_capacity(text.len().min(max));
    for character in text.chars() {
        if fitted.len().saturating_add(character.len_utf8()) > max {
            break;
        }
        fitted.push(character);
    }
    fitted.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{DeviceCapabilities, RevocationList, sign_device_cert};

    use super::*;

    const AUDIENCE: &str = "https://door.example";

    fn device(root: &SigningKey, key: &SigningKey, capabilities: DeviceCapabilities) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Tilo on the web".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    #[tokio::test]
    async fn a_device_proves_its_person_to_the_service_that_asked() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let key = SigningKey::from_bytes(&[2; 32]);
        let cert = device(&root, &key, DeviceCapabilities::member());
        let proof = sign_in_proof(cert, &key, AUDIENCE, [7; 32]).await.unwrap();
        assert_eq!(
            proof.verify(AUDIENCE, &[7; 32], &RevocationList::new(), 10),
            Ok(root.verifying_key().to_bytes())
        );
        // The text the service decodes is the proof's canonical bytes.
        let text = encode(&proof).unwrap();
        let decoded: SignInProof = tilo_user_identity::canonical::from_canonical_bytes(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&text)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded, proof);
        // Another device may not sign under this certificate.
        let other = SigningKey::from_bytes(&[3; 32]);
        assert_eq!(
            sign_in_proof(proof.cert.clone(), &other, AUDIENCE, [7; 32]).await,
            Err(SignInError::Signature)
        );
        // Nor does a proof answer another service, or another challenge.
        assert_eq!(
            decoded.verify(
                "https://other.example",
                &[7; 32],
                &RevocationList::new(),
                10
            ),
            Err(SignInError::WrongChallenge)
        );
    }

    #[tokio::test]
    async fn a_certified_device_travels_as_canonical_bytes_in_base64url() {
        let root = SigningKey::from_bytes(&[4; 32]);
        let key = SigningKey::from_bytes(&[5; 32]);
        let cert = device(&root, &key, DeviceCapabilities::member());
        let text = encode(&cert).unwrap();
        assert!(!text.contains('=') && !text.contains('+') && !text.contains('/'));
        let decoded: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&text)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded, cert);
    }

    #[test]
    fn a_grant_is_labelled_by_its_client_and_service() {
        assert_eq!(
            grant_label("Claude Code", AUDIENCE),
            "Claude Code via door.example"
        );
        assert_eq!(
            grant_label("Claude Code", "http://localhost:8787"),
            "Claude Code via localhost:8787"
        );
        assert_eq!(grant_label("  ", AUDIENCE), "An agent via door.example");
        assert_eq!(
            grant_label("two\nlines", AUDIENCE),
            "twolines via door.example"
        );
        let long = grant_label(&"é".repeat(100), AUDIENCE);
        assert!(
            long.len() <= MAX_CERTIFICATION_LABEL_BYTES,
            "{} bytes",
            long.len()
        );
        assert!(long.starts_with(&"é".repeat(10)));
        assert!(long.ends_with(" via door.example"));
        let long_service = grant_label(
            "Claude Code",
            &format!("https://{}.example", "a".repeat(100)),
        );
        assert!(
            long_service.len() <= MAX_CERTIFICATION_LABEL_BYTES,
            "{} bytes",
            long_service.len()
        );
        assert!(long_service.starts_with("Claude Code via aaaa"));
    }
}
