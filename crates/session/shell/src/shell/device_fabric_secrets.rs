//! Per-device envelopes for secrets replicated by the personal fabric.
//!
//! The convergent checkpoint contains only ciphertext. Each certified device
//! gets an independent X25519/`XChaCha20Poly1305` envelope, so adding or
//! revoking hardware does not depend on a vendor cloud account or one shared
//! key copied everywhere.

use std::collections::BTreeSet;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{SigningKey, VerifyingKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::shell::device_fabric::{
    FabricStamp, MAX_DEVICES, MAX_SEALED_SECRET_BYTES, WifiCredential,
};
use crate::shell::personal_continuity::DeviceId;

const WIFI_ENVELOPE_VERSION: u8 = 1;
const WIFI_KEY_DOMAIN: &[u8] = b"tilo-personal-fabric-wifi-envelope-v1\0";
const MAX_WIFI_PASSWORD_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WifiRecipientEnvelope {
    recipient: DeviceId,
    ephemeral_public_key: [u8; 32],
    nonce: [u8; 24],
    ciphertext: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WifiEnvelopeBundle {
    version: u8,
    envelopes: Vec<WifiRecipientEnvelope>,
}

/// Failure to create, validate, or open a Wi-Fi secret envelope.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WifiSecretError {
    #[error("the Wi-Fi credential metadata is invalid")]
    InvalidCredential,
    #[error("the Wi-Fi credential has no envelope for this device")]
    NotARecipient,
    #[error("the Wi-Fi credential could not be decrypted")]
    DecryptionFailed,
    #[error("the Wi-Fi credential envelope exceeds its fabric bound")]
    TooLarge,
}

/// Seal one passphrase independently to every currently certified device.
///
/// # Errors
///
/// Returns an error for invalid metadata, recipients, an empty/oversized
/// passphrase, encryption failure, or an aggregate exceeding the fabric cap.
pub fn seal_wifi_credential(
    ssid: &str,
    security: os_actions::WifiSecurity,
    password: &str,
    author: &DeviceId,
    recipients: impl IntoIterator<Item = DeviceId>,
    stamp: FabricStamp,
) -> Result<WifiCredential, WifiSecretError> {
    if ssid.is_empty()
        || ssid.len() > 128
        || !security.supports_portable_passphrase()
        || password.is_empty()
        || password.len() > MAX_WIFI_PASSWORD_BYTES
        || stamp.device != *author
    {
        return Err(WifiSecretError::InvalidCredential);
    }
    let recipients = recipients.into_iter().collect::<BTreeSet<_>>();
    if recipients.is_empty() || recipients.len() > MAX_DEVICES {
        return Err(WifiSecretError::InvalidCredential);
    }
    let network_id = wifi_network_id(ssid, security);
    let mut bundle_nonce = [0_u8; 24];
    UnwrapErr(SysRng).fill_bytes(&mut bundle_nonce);
    let mut envelopes = Vec::with_capacity(recipients.len());
    for recipient in recipients {
        let recipient_bytes = decode_device_key(&recipient)?;
        let recipient_public = VerifyingKey::from_bytes(&recipient_bytes)
            .map_err(|_| WifiSecretError::InvalidCredential)?
            .to_montgomery()
            .to_bytes();
        let mut seed = [0_u8; 32];
        UnwrapErr(SysRng).fill_bytes(&mut seed);
        let ephemeral = StaticSecret::from(seed);
        let ephemeral_public_key = PublicKey::from(&ephemeral).to_bytes();
        let shared = ephemeral.diffie_hellman(&PublicKey::from(recipient_public));
        let key = wifi_envelope_key(
            shared.as_bytes(),
            recipient_bytes,
            ephemeral_public_key,
            network_id,
            bundle_nonce,
        );
        let mut nonce = [0_u8; 24];
        UnwrapErr(SysRng).fill_bytes(&mut nonce);
        let aad = wifi_envelope_aad(
            author,
            &recipient,
            ssid,
            security,
            network_id,
            bundle_nonce,
            ephemeral_public_key,
        );
        let ciphertext = XChaCha20Poly1305::new((&key).into())
            .encrypt(
                <&XNonce>::from(&nonce),
                Payload {
                    msg: password.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| WifiSecretError::InvalidCredential)?;
        envelopes.push(WifiRecipientEnvelope {
            recipient,
            ephemeral_public_key,
            nonce,
            ciphertext,
        });
    }
    let sealed_secret = postcard::to_allocvec(&WifiEnvelopeBundle {
        version: WIFI_ENVELOPE_VERSION,
        envelopes,
    })
    .map_err(|_| WifiSecretError::InvalidCredential)?;
    if sealed_secret.len() > MAX_SEALED_SECRET_BYTES {
        return Err(WifiSecretError::TooLarge);
    }
    validate_wifi_envelopes(&sealed_secret)?;
    Ok(WifiCredential {
        network_id,
        ssid: ssid.to_string(),
        security,
        sealed_secret,
        nonce: bundle_nonce,
        stamp,
    })
}

/// Open this device's envelope without exposing any other recipient's copy.
///
/// # Errors
///
/// Returns an error when the envelope is malformed, does not target this
/// device, fails authentication, or does not contain a valid passphrase.
pub fn open_wifi_credential(
    credential: &WifiCredential,
    recipient_key: &SigningKey,
) -> Result<String, WifiSecretError> {
    if !credential.security.supports_portable_passphrase() {
        return Err(WifiSecretError::InvalidCredential);
    }
    let bundle = decode_bundle(&credential.sealed_secret)?;
    let recipient_bytes = recipient_key.verifying_key().to_bytes();
    let recipient = DeviceId::new(hex::encode(recipient_bytes))
        .map_err(|_| WifiSecretError::InvalidCredential)?;
    let envelope = bundle
        .envelopes
        .iter()
        .find(|envelope| envelope.recipient == recipient)
        .ok_or(WifiSecretError::NotARecipient)?;
    let recipient_secret = StaticSecret::from(recipient_key.to_scalar_bytes());
    let shared = recipient_secret.diffie_hellman(&PublicKey::from(envelope.ephemeral_public_key));
    let key = wifi_envelope_key(
        shared.as_bytes(),
        recipient_bytes,
        envelope.ephemeral_public_key,
        credential.network_id,
        credential.nonce,
    );
    let aad = wifi_envelope_aad(
        &credential.stamp.device,
        &recipient,
        &credential.ssid,
        credential.security,
        credential.network_id,
        credential.nonce,
        envelope.ephemeral_public_key,
    );
    let plaintext = XChaCha20Poly1305::new((&key).into())
        .decrypt(
            <&XNonce>::from(&envelope.nonce),
            Payload {
                msg: &envelope.ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| WifiSecretError::DecryptionFailed)?;
    let password = String::from_utf8(plaintext).map_err(|_| WifiSecretError::DecryptionFailed)?;
    if password.is_empty() || password.len() > MAX_WIFI_PASSWORD_BYTES {
        return Err(WifiSecretError::DecryptionFailed);
    }
    Ok(password)
}

/// Certified recipients currently represented by an opaque bundle.
///
/// # Errors
///
/// Returns an error when the bundle is malformed or violates its bounds.
pub fn wifi_envelope_recipients(
    credential: &WifiCredential,
) -> Result<BTreeSet<DeviceId>, WifiSecretError> {
    validate_wifi_envelopes(&credential.sealed_secret)?;
    Ok(decode_bundle(&credential.sealed_secret)?
        .envelopes
        .into_iter()
        .map(|envelope| envelope.recipient)
        .collect())
}

pub(crate) fn validate_wifi_envelopes(bytes: &[u8]) -> Result<(), WifiSecretError> {
    let bundle = decode_bundle(bytes)?;
    if bundle.version != WIFI_ENVELOPE_VERSION
        || bundle.envelopes.is_empty()
        || bundle.envelopes.len() > MAX_DEVICES
    {
        return Err(WifiSecretError::InvalidCredential);
    }
    let mut recipients = BTreeSet::new();
    for envelope in bundle.envelopes {
        if !recipients.insert(envelope.recipient)
            || envelope.ephemeral_public_key == [0; 32]
            || envelope.ciphertext.len() < 16
            || envelope.ciphertext.len() > MAX_WIFI_PASSWORD_BYTES + 16
        {
            return Err(WifiSecretError::InvalidCredential);
        }
    }
    Ok(())
}

fn decode_bundle(bytes: &[u8]) -> Result<WifiEnvelopeBundle, WifiSecretError> {
    if bytes.is_empty() || bytes.len() > MAX_SEALED_SECRET_BYTES {
        return Err(WifiSecretError::TooLarge);
    }
    let (bundle, remaining) =
        postcard::take_from_bytes(bytes).map_err(|_| WifiSecretError::InvalidCredential)?;
    if !remaining.is_empty() {
        return Err(WifiSecretError::InvalidCredential);
    }
    Ok(bundle)
}

fn decode_device_key(device: &DeviceId) -> Result<[u8; 32], WifiSecretError> {
    let bytes = hex::decode(device.as_str()).map_err(|_| WifiSecretError::InvalidCredential)?;
    bytes
        .try_into()
        .map_err(|_| WifiSecretError::InvalidCredential)
}

fn wifi_network_id(ssid: &str, security: os_actions::WifiSecurity) -> [u8; 32] {
    let mut material = WIFI_KEY_DOMAIN.to_vec();
    material.extend_from_slice(b"network\0");
    material.extend_from_slice(ssid.as_bytes());
    material.push(0);
    material.push(security.continuity_tag());
    *blake3::hash(&material).as_bytes()
}

fn wifi_envelope_key(
    shared: &[u8; 32],
    recipient: [u8; 32],
    ephemeral: [u8; 32],
    network_id: [u8; 32],
    bundle_nonce: [u8; 24],
) -> [u8; 32] {
    let mut material = WIFI_KEY_DOMAIN.to_vec();
    material.extend_from_slice(shared);
    material.extend_from_slice(&recipient);
    material.extend_from_slice(&ephemeral);
    material.extend_from_slice(&network_id);
    material.extend_from_slice(&bundle_nonce);
    *blake3::hash(&material).as_bytes()
}

fn wifi_envelope_aad(
    author: &DeviceId,
    recipient: &DeviceId,
    ssid: &str,
    security: os_actions::WifiSecurity,
    network_id: [u8; 32],
    bundle_nonce: [u8; 24],
    ephemeral: [u8; 32],
) -> Vec<u8> {
    let mut aad = WIFI_KEY_DOMAIN.to_vec();
    aad.extend_from_slice(author.as_str().as_bytes());
    aad.push(0);
    aad.extend_from_slice(recipient.as_str().as_bytes());
    aad.push(0);
    aad.extend_from_slice(ssid.as_bytes());
    aad.push(0);
    aad.push(security.continuity_tag());
    aad.extend_from_slice(&network_id);
    aad.extend_from_slice(&bundle_nonce);
    aad.extend_from_slice(&ephemeral);
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(key: &SigningKey) -> DeviceId {
        DeviceId::new(hex::encode(key.verifying_key().to_bytes())).unwrap()
    }

    /// A credential sealed before a crypto-stack upgrade. Devices on
    /// different builds exchange these envelopes, so Ed25519-to-X25519
    /// conversion, X25519 agreement and XChaCha20-Poly1305 must stay
    /// byte-compatible.
    #[test]
    fn credential_sealed_by_an_earlier_build_still_opens() {
        let author_key = SigningKey::from_bytes(&[1; 32]);
        let laptop_key = SigningKey::from_bytes(&[2; 32]);
        let author = device(&author_key);
        let network_id = wifi_network_id("Home", os_actions::WifiSecurity::Wpa3);
        assert_eq!(
            hex::encode(network_id),
            "3acb148a49b7b11b0709306da205cc903420497d6fb8e9efb16a3adac3d1e80c"
        );
        let credential = WifiCredential {
            network_id,
            ssid: "Home".to_string(),
            security: os_actions::WifiSecurity::Wpa3,
            sealed_secret: hex::decode(
                "0102403831333937373065613837643137356635366133353436366333346337\
                6563636362386438613931623465653337613235646636306635623866633962\
                33393428892c4aeb3eaaaa9c33184c2a373f878c6828575dd00a28ceaf913176\
                8c4a4c0a78addaa6728b4c59c67b027ec859022eb9033aec9fe6082314e9979c\
                04eed6b2da816b0999b7b7a8aabe11367cbee8e1d624ce30e78133a9de997640\
                3861383865336464373430396631393566643532646232643363626135643732\
                6361363730396266316439343132316266333734383830316234306636663563\
                20780a8c3f9ce8e0221c752624c03a32c2a68c43edbef27ef7660e8433d7770a\
                2a8c702ed2a4491e05a2644bcf08d0dec0531327a9e2ed7423be26fc02b54021\
                a8dd77f58615d2bbd239baec431831e46d18e9f86b7ce171cc353872",
            )
            .unwrap(),
            nonce: hex::decode("9ce84bea5719e1eb1c14441d827462389f744fa0088d88d9")
                .unwrap()
                .try_into()
                .unwrap(),
            stamp: FabricStamp {
                counter: 8,
                device: author,
            },
        };
        for key in [&author_key, &laptop_key] {
            assert_eq!(
                open_wifi_credential(&credential, key).unwrap(),
                "kat wifi passphrase"
            );
        }
        assert_eq!(
            open_wifi_credential(&credential, &SigningKey::from_bytes(&[3; 32])),
            Err(WifiSecretError::NotARecipient)
        );
    }

    #[test]
    fn each_certified_recipient_can_open_but_an_outsider_cannot() {
        let author_key = SigningKey::from_bytes(&[1; 32]);
        let laptop_key = SigningKey::from_bytes(&[2; 32]);
        let outsider_key = SigningKey::from_bytes(&[3; 32]);
        let author = device(&author_key);
        let credential = seal_wifi_credential(
            "Home",
            os_actions::WifiSecurity::Wpa3,
            "correct horse battery staple",
            &author,
            [author.clone(), device(&laptop_key)],
            FabricStamp {
                counter: 8,
                device: author.clone(),
            },
        )
        .unwrap();

        assert_eq!(
            open_wifi_credential(&credential, &author_key).unwrap(),
            "correct horse battery staple"
        );
        assert_eq!(
            open_wifi_credential(&credential, &laptop_key).unwrap(),
            "correct horse battery staple"
        );
        assert_eq!(
            open_wifi_credential(&credential, &outsider_key),
            Err(WifiSecretError::NotARecipient)
        );
        assert!(
            !credential
                .sealed_secret
                .windows("correct horse battery staple".len())
                .any(|window| window == b"correct horse battery staple")
        );
    }

    #[test]
    fn public_metadata_is_authenticated_as_associated_data() {
        let key = SigningKey::from_bytes(&[4; 32]);
        let author = device(&key);
        let mut credential = seal_wifi_credential(
            "Home",
            os_actions::WifiSecurity::Wpa2,
            "secret",
            &author,
            [author.clone()],
            FabricStamp {
                counter: 1,
                device: author.clone(),
            },
        )
        .unwrap();
        credential.ssid = "Lookalike".into();
        assert_eq!(
            open_wifi_credential(&credential, &key),
            Err(WifiSecretError::DecryptionFailed)
        );
    }

    #[test]
    fn security_mode_is_authenticated_and_enterprise_cannot_be_password_only() {
        let key = SigningKey::from_bytes(&[31; 32]);
        let author = device(&key);
        let stamp = FabricStamp {
            counter: 1,
            device: author.clone(),
        };
        let mut credential = seal_wifi_credential(
            "Home",
            os_actions::WifiSecurity::Wpa2,
            "secret",
            &author,
            [author.clone()],
            stamp.clone(),
        )
        .unwrap();
        credential.security = os_actions::WifiSecurity::Wpa3;
        assert_eq!(
            open_wifi_credential(&credential, &key),
            Err(WifiSecretError::DecryptionFailed)
        );

        for security in [
            os_actions::WifiSecurity::Enterprise,
            os_actions::WifiSecurity::Owe,
            os_actions::WifiSecurity::Open,
            os_actions::WifiSecurity::Unknown,
        ] {
            assert_eq!(
                seal_wifi_credential(
                    "Corp",
                    security,
                    "not-enough-enterprise-context",
                    &author,
                    [author.clone()],
                    stamp.clone(),
                ),
                Err(WifiSecretError::InvalidCredential),
                "{security:?}"
            );
        }
    }
}
