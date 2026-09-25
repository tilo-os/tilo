//! Client-owned enrollment identity. Opaque persistence bytes never leave the browser.
use ed25519_dalek::SigningKey;
use qos_peer_state::enrollment::{self, ChallengeRequest, ChallengeResponse};
use serde::{Deserialize, Serialize};
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Scope {
    root: [u8; 32],
    room: String,
    workspace: String,
}
#[derive(Serialize, Deserialize)]
struct Material {
    version: u32,
    secret: [u8; 32],
    request_id: [u8; 32],
    scope: Option<Scope>,
}
#[derive(Deserialize)]
struct PublicBootstrap {
    version: u32,
    certificate: tilo_user_identity::DeviceCert,
    root_public_key: [u8; 32],
    bootstrap: Room,
}
#[derive(Deserialize)]
struct Room {
    room: String,
    workspace_id: String,
}
fn read(bytes: &[u8]) -> Result<Material, String> {
    let material: Material =
        serde_json::from_slice(bytes).map_err(|_| "Invalid stored identity material")?;
    if material.version != 2 {
        return Err("Unsupported stored identity version".into());
    }
    Ok(material)
}
pub fn create() -> Vec<u8> {
    let (key, _) = tilo_user_identity::DeviceIdentity::generate("Browser viewer", 0);
    let (request, _) = tilo_user_identity::DeviceIdentity::generate("Enrollment request", 0);
    serde_json::to_vec(&Material {
        version: 2,
        secret: key.to_bytes(),
        request_id: request.to_bytes(),
        scope: None,
    })
    .expect("serializable identity")
}
pub fn request(bytes: &[u8]) -> Result<String, String> {
    let material = read(bytes)?;
    serde_json::to_string(&ChallengeRequest {
        device_public_key: encode(
            &SigningKey::from_bytes(&material.secret)
                .verifying_key()
                .to_bytes(),
        ),
        request_id: encode(&material.request_id),
    })
    .map_err(|e| e.to_string())
}
fn encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn challenge(
    material: &Material,
    response: &str,
) -> Result<enrollment::EnrollmentChallenge, String> {
    let response: ChallengeResponse = serde_json::from_str(response).map_err(|e| e.to_string())?;
    if response.version != 2 {
        return Err("Unsupported enrollment challenge version".into());
    }
    let challenge = enrollment::decode(&response.challenge)?;
    if challenge.device_public_key
        != SigningKey::from_bytes(&material.secret)
            .verifying_key()
            .to_bytes()
        || challenge.request_id != material.request_id
    {
        return Err("Enrollment identity/request mismatch".into());
    }
    if let Some(scope) = &material.scope {
        if scope.root != challenge.root_public_key
            || scope.room != challenge.room
            || scope.workspace != challenge.workspace_id.to_string()
        {
            return Err(
                "Enrollment authority changed; preserved identity requires explicit migration"
                    .into(),
            );
        }
    }
    Ok(challenge)
}
pub fn complete(bytes: &[u8], response: &str) -> Result<String, String> {
    let material = read(bytes)?;
    let proof = enrollment::sign(
        &challenge(&material, response)?,
        &SigningKey::from_bytes(&material.secret),
    )?;
    serde_json::to_string(&proof).map_err(|e| e.to_string())
}
pub fn bind(bytes: &[u8], response: &str, raw: &str) -> Result<Vec<u8>, String> {
    let mut material = read(bytes)?;
    let challenge = challenge(&material, response)?;
    let config: PublicBootstrap = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    if config.version != 2
        || config.root_public_key != challenge.root_public_key
        || config.bootstrap.room != challenge.room
        || config.bootstrap.workspace_id != challenge.workspace_id.to_string()
        || config.certificate.device_public_key != challenge.device_public_key
        || config.certificate.user_public_key != challenge.root_public_key
    {
        return Err("Public enrollment bootstrap binding mismatch".into());
    }
    let now = web_time::SystemTime::now()
        .duration_since(web_time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX);
    tilo_user_identity::verify_device_cert_at(&config.certificate, now)
        .map_err(|e| e.to_string())?;
    material.scope = Some(Scope {
        root: config.root_public_key,
        room: config.bootstrap.room,
        workspace: config.bootstrap.workspace_id,
    });
    serde_json::to_vec(&material).map_err(|e| e.to_string())
}
pub fn key(bytes: &[u8], raw: &str) -> Result<SigningKey, String> {
    let material = read(bytes)?;
    let config: PublicBootstrap = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let expected = Scope {
        root: config.root_public_key,
        room: config.bootstrap.room,
        workspace: config.bootstrap.workspace_id,
    };
    if material.scope.as_ref() != Some(&expected) || config.version != 2 {
        return Err("Identity is not bound to this enrolled workspace".into());
    }
    let key = SigningKey::from_bytes(&material.secret);
    if config.certificate.device_public_key != key.verifying_key().to_bytes() {
        return Err("Enrollment key mismatch".into());
    }
    Ok(key)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_client_keys_and_stable_request_proof() {
        let first = create();
        let second = create();
        assert_ne!(request(&first).unwrap(), request(&second).unwrap());
        assert_eq!(request(&first).unwrap(), request(&first).unwrap());
        let material = read(&first).unwrap();
        let value = enrollment::EnrollmentChallenge {
            version: 2,
            root_public_key: [1; 32],
            room: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            workspace_id: 7,
            device_public_key: SigningKey::from_bytes(&material.secret)
                .verifying_key()
                .to_bytes(),
            request_id: material.request_id,
            nonce: [9; 32],
            expires_at_ns: u64::MAX,
        };
        let response = serde_json::to_string(&ChallengeResponse {
            version: 2,
            challenge: enrollment::encode(&value).unwrap(),
        })
        .unwrap();
        let proof = serde_json::from_str(&complete(&first, &response).unwrap()).unwrap();
        assert_eq!(
            enrollment::verify(&proof, [1; 32], &value.room, 7, 1).unwrap(),
            value
        );
        assert!(complete(&second, &response).is_err());
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    #[test]
    fn public_response_binds_key_scope_and_same_key_refresh_rejects_other_authority() {
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
        let material = create();
        let data = read(&material).unwrap();
        let root = SigningKey::from_bytes(&[1; 32]);
        let device = SigningKey::from_bytes(&data.secret)
            .verifying_key()
            .to_bytes();
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "browser".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let mut challenge = enrollment::EnrollmentChallenge {
            version: 2,
            root_public_key: cert.user_public_key,
            room: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            workspace_id: 7,
            device_public_key: device,
            request_id: data.request_id,
            nonce: [9; 32],
            expires_at_ns: u64::MAX,
        };
        let response = |value: &enrollment::EnrollmentChallenge| {
            serde_json::to_string(&ChallengeResponse {
                version: 2,
                challenge: enrollment::encode(value).unwrap(),
            })
            .unwrap()
        };
        let raw=serde_json::json!({"version":2,"certificate":cert,"root_public_key":cert.user_public_key,"bootstrap":{"room":challenge.room,"workspace_id":"7"}}).to_string();
        assert!(key(&material, &raw).is_err());
        let bound = bind(&material, &response(&challenge), &raw).unwrap();
        assert_eq!(request(&material).unwrap(), request(&bound).unwrap());
        assert_eq!(
            key(&bound, &raw).unwrap().verifying_key().to_bytes(),
            device
        );
        challenge.nonce = [8; 32];
        assert!(complete(&bound, &response(&challenge)).is_ok());
        challenge.root_public_key = [7; 32];
        assert!(
            complete(&bound, &response(&challenge))
                .unwrap_err()
                .contains("authority changed")
        );
        assert!(bind(&material, &response(&challenge), &raw).is_err());
    }
}
