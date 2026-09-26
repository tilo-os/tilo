//! Per-device envelopes for secrets replicated by the personal fabric.
//!
//! The convergent checkpoint contains only ciphertext. Each certified device
//! gets an independent envelope sealed to its own key, so adding or revoking
//! hardware does not depend on a vendor cloud account or one shared key
//! copied everywhere.
//!
//! Bundle version 2 (written today) seals each copy with HPKE through
//! `tilo-seal` (label [`Label::DeviceFabricSecret`]) to the recipient's
//! Ed25519 device key in its X25519 form, authenticating the public
//! credential metadata as HPKE associated data. Version 1 bundles (raw
//! X25519, a BLAKE3 key derivation and `XChaCha20-Poly1305`) still open, so
//! credentials sealed by earlier builds keep working; refreshing a
//! credential's recipients rewrites it as version 2. The version is the
//! first postcard byte of the bundle.

use std::collections::BTreeSet;

use ed25519_dalek::{SigningKey, VerifyingKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tilo_seal::{Label, PublicKey, SealedBox, SecretKey};

use crate::shell::device_fabric::{
    FabricStamp, MAX_DEVICES, MAX_SEALED_SECRET_BYTES, WifiCredential,
};
use crate::shell::personal_continuity::DeviceId;

const WIFI_ENVELOPE_V1: u8 = 1;
const WIFI_ENVELOPE_V2: u8 = 2;
const WIFI_KEY_DOMAIN: &[u8] = b"tilo-personal-fabric-wifi-envelope-v1\0";
const MAX_WIFI_PASSWORD_BYTES: usize = 4096;
/// HPKE sealed-box overhead: version byte, encapsulated key, AEAD tag.
const SEALED_BOX_OVERHEAD: usize = 1 + 32 + 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WifiRecipientEnvelopeV1 {
    recipient: DeviceId,
    ephemeral_public_key: [u8; 32],
    nonce: [u8; 24],
    ciphertext: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WifiEnvelopeBundleV1 {
    version: u8,
    envelopes: Vec<WifiRecipientEnvelopeV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WifiRecipientEnvelopeV2 {
    recipient: DeviceId,
    /// `tilo_seal::SealedBox` wire bytes.
    sealed: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WifiEnvelopeBundleV2 {
    version: u8,
    envelopes: Vec<WifiRecipientEnvelopeV2>,
}

enum WifiEnvelopeBundle {
    V1(WifiEnvelopeBundleV1),
    V2(WifiEnvelopeBundleV2),
}

impl WifiEnvelopeBundle {
    fn recipients(&self) -> Vec<&DeviceId> {
        match self {
            Self::V1(bundle) => bundle.envelopes.iter().map(|e| &e.recipient).collect(),
            Self::V2(bundle) => bundle.envelopes.iter().map(|e| &e.recipient).collect(),
        }
    }
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
        let recipient_public = recipient_public_key(&recipient)?;
        let aad =
            wifi_envelope_aad_v2(author, &recipient, ssid, security, network_id, bundle_nonce);
        let sealed = tilo_seal::hpke::seal(
            &recipient_public,
            Label::DeviceFabricSecret,
            &aad,
            password.as_bytes(),
        )
        .map_err(|_| WifiSecretError::InvalidCredential)?;
        envelopes.push(WifiRecipientEnvelopeV2 {
            recipient,
            sealed: sealed.to_bytes(),
        });
    }
    let sealed_secret = postcard::to_allocvec(&WifiEnvelopeBundleV2 {
        version: WIFI_ENVELOPE_V2,
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
    let recipient_secret = SecretKey::from_bytes(recipient_key.to_scalar_bytes());
    let plaintext = match &bundle {
        WifiEnvelopeBundle::V2(bundle) => {
            let envelope = bundle
                .envelopes
                .iter()
                .find(|envelope| envelope.recipient == recipient)
                .ok_or(WifiSecretError::NotARecipient)?;
            let sealed = SealedBox::from_bytes(&envelope.sealed)
                .map_err(|_| WifiSecretError::InvalidCredential)?;
            let aad = wifi_envelope_aad_v2(
                &credential.stamp.device,
                &recipient,
                &credential.ssid,
                credential.security,
                credential.network_id,
                credential.nonce,
            );
            tilo_seal::hpke::open(&recipient_secret, Label::DeviceFabricSecret, &aad, &sealed)
                .map_err(|_| WifiSecretError::DecryptionFailed)?
        }
        WifiEnvelopeBundle::V1(bundle) => {
            let envelope = bundle
                .envelopes
                .iter()
                .find(|envelope| envelope.recipient == recipient)
                .ok_or(WifiSecretError::NotARecipient)?;
            let key = tilo_seal::legacy::blake3_x25519_key(
                &recipient_secret,
                &PublicKey::from_bytes(envelope.ephemeral_public_key),
                WIFI_KEY_DOMAIN,
                &[
                    &recipient_bytes,
                    &envelope.ephemeral_public_key,
                    &credential.network_id,
                    &credential.nonce,
                ],
            )
            .map_err(|_| WifiSecretError::DecryptionFailed)?;
            let aad = wifi_envelope_aad_v1(
                &credential.stamp.device,
                &recipient,
                &credential.ssid,
                credential.security,
                credential.network_id,
                credential.nonce,
                envelope.ephemeral_public_key,
            );
            tilo_seal::legacy::open_xchacha20poly1305(
                &key,
                &envelope.nonce,
                &aad,
                &envelope.ciphertext,
            )
            .map_err(|_| WifiSecretError::DecryptionFailed)?
        }
    };
    // `plaintext` is zeroized on drop; only the returned copy survives.
    let password = std::str::from_utf8(&plaintext)
        .map_err(|_| WifiSecretError::DecryptionFailed)?
        .to_owned();
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
        .recipients()
        .into_iter()
        .cloned()
        .collect())
}

pub(crate) fn validate_wifi_envelopes(bytes: &[u8]) -> Result<(), WifiSecretError> {
    let bundle = decode_bundle(bytes)?;
    let listed = bundle.recipients();
    if listed.is_empty() || listed.len() > MAX_DEVICES {
        return Err(WifiSecretError::InvalidCredential);
    }
    let mut recipients = BTreeSet::new();
    if listed
        .into_iter()
        .any(|recipient| !recipients.insert(recipient))
    {
        return Err(WifiSecretError::InvalidCredential);
    }
    let valid = match &bundle {
        WifiEnvelopeBundle::V1(bundle) => bundle.envelopes.iter().all(|envelope| {
            envelope.ephemeral_public_key != [0; 32]
                && envelope.ciphertext.len() >= 16
                && envelope.ciphertext.len() <= MAX_WIFI_PASSWORD_BYTES + 16
        }),
        WifiEnvelopeBundle::V2(bundle) => bundle.envelopes.iter().all(|envelope| {
            envelope.sealed.len() > SEALED_BOX_OVERHEAD
                && envelope.sealed.len() <= MAX_WIFI_PASSWORD_BYTES + SEALED_BOX_OVERHEAD
                && SealedBox::from_bytes(&envelope.sealed).is_ok()
        }),
    };
    if !valid {
        return Err(WifiSecretError::InvalidCredential);
    }
    Ok(())
}

fn decode_bundle(bytes: &[u8]) -> Result<WifiEnvelopeBundle, WifiSecretError> {
    if bytes.is_empty() || bytes.len() > MAX_SEALED_SECRET_BYTES {
        return Err(WifiSecretError::TooLarge);
    }
    // Both layouts start with their one-byte postcard `version` field.
    match bytes.first().copied() {
        Some(WIFI_ENVELOPE_V1) => decode_exact::<WifiEnvelopeBundleV1>(bytes)
            .filter(|bundle| bundle.version == WIFI_ENVELOPE_V1)
            .map(WifiEnvelopeBundle::V1),
        Some(WIFI_ENVELOPE_V2) => decode_exact::<WifiEnvelopeBundleV2>(bytes)
            .filter(|bundle| bundle.version == WIFI_ENVELOPE_V2)
            .map(WifiEnvelopeBundle::V2),
        _ => None,
    }
    .ok_or(WifiSecretError::InvalidCredential)
}

fn decode_exact<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    let (value, remaining) = postcard::take_from_bytes(bytes).ok()?;
    remaining.is_empty().then_some(value)
}

/// The X25519 form of a device's Ed25519 identity key.
fn recipient_public_key(device: &DeviceId) -> Result<PublicKey, WifiSecretError> {
    let bytes: [u8; 32] = hex::decode(device.as_str())
        .map_err(|_| WifiSecretError::InvalidCredential)?
        .try_into()
        .map_err(|_| WifiSecretError::InvalidCredential)?;
    Ok(PublicKey::from_bytes(
        VerifyingKey::from_bytes(&bytes)
            .map_err(|_| WifiSecretError::InvalidCredential)?
            .to_montgomery()
            .to_bytes(),
    ))
}

fn wifi_network_id(ssid: &str, security: os_actions::WifiSecurity) -> [u8; 32] {
    let mut material = WIFI_KEY_DOMAIN.to_vec();
    material.extend_from_slice(b"network\0");
    material.extend_from_slice(ssid.as_bytes());
    material.push(0);
    material.push(security.continuity_tag());
    *blake3::hash(&material).as_bytes()
}

/// Associated data of a version 2 envelope. HPKE already binds the
/// encapsulated key, so unlike version 1 it is not repeated here.
fn wifi_envelope_aad_v2(
    author: &DeviceId,
    recipient: &DeviceId,
    ssid: &str,
    security: os_actions::WifiSecurity,
    network_id: [u8; 32],
    bundle_nonce: [u8; 24],
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
    aad
}

/// Associated data of a version 1 envelope, byte for byte as earlier builds
/// wrote it.
fn wifi_envelope_aad_v1(
    author: &DeviceId,
    recipient: &DeviceId,
    ssid: &str,
    security: os_actions::WifiSecurity,
    network_id: [u8; 32],
    bundle_nonce: [u8; 24],
    ephemeral: [u8; 32],
) -> Vec<u8> {
    let mut aad = wifi_envelope_aad_v2(author, recipient, ssid, security, network_id, bundle_nonce);
    aad.extend_from_slice(&ephemeral);
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(key: &SigningKey) -> DeviceId {
        DeviceId::new(hex::encode(key.verifying_key().to_bytes())).unwrap()
    }

    /// Version 1 bytes written by the build before the `tilo-seal`
    /// migration: author `[1; 32]`, recipients author and laptop `[2; 32]`.
    fn earlier_build_credential() -> WifiCredential {
        let author = device(&SigningKey::from_bytes(&[1; 32]));
        let network_id = wifi_network_id("Home", os_actions::WifiSecurity::Wpa3);
        WifiCredential {
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
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn credential_bundle_versions_stay_openable_rewrap_forward_and_reject_unknown() {
        /// A credential sealed before a crypto-stack upgrade. Devices on
        /// different builds exchange these envelopes, so Ed25519-to-X25519
        /// conversion, X25519 agreement and XChaCha20-Poly1305 must stay
        /// byte-compatible.
        fn credential_sealed_by_an_earlier_build_still_opens() {
            let author_key = SigningKey::from_bytes(&[1; 32]);
            let laptop_key = SigningKey::from_bytes(&[2; 32]);
            assert_eq!(
                hex::encode(wifi_network_id("Home", os_actions::WifiSecurity::Wpa3)),
                "3acb148a49b7b11b0709306da205cc903420497d6fb8e9efb16a3adac3d1e80c"
            );
            let credential = earlier_build_credential();
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
            assert_eq!(
                wifi_envelope_recipients(&credential).unwrap(),
                BTreeSet::from([device(&author_key), device(&laptop_key)])
            );
        }

        fn new_credentials_are_version_two_hpke_boxes() {
            let author_key = SigningKey::from_bytes(&[5; 32]);
            let author = device(&author_key);
            let credential = seal_wifi_credential(
                "Office",
                os_actions::WifiSecurity::Wpa2,
                "v2 passphrase",
                &author,
                [author.clone()],
                FabricStamp {
                    counter: 2,
                    device: author.clone(),
                },
            )
            .unwrap();
            assert_eq!(credential.sealed_secret.first(), Some(&WIFI_ENVELOPE_V2));
            assert_eq!(
                open_wifi_credential(&credential, &author_key).unwrap(),
                "v2 passphrase"
            );
            assert_eq!(
                wifi_envelope_recipients(&credential).unwrap(),
                BTreeSet::from([author])
            );
            // The bundle nonce is authenticated too.
            let mut moved = credential.clone();
            moved.nonce[0] ^= 1;
            assert_eq!(
                open_wifi_credential(&moved, &author_key),
                Err(WifiSecretError::DecryptionFailed)
            );
            // So is the author named in the stamp.
            let mut reauthored = credential;
            reauthored.stamp.device = device(&SigningKey::from_bytes(&[6; 32]));
            assert_eq!(
                open_wifi_credential(&reauthored, &author_key),
                Err(WifiSecretError::DecryptionFailed)
            );
        }

        fn version_one_credential_rewraps_to_version_two() {
            let author_key = SigningKey::from_bytes(&[1; 32]);
            let laptop_key = SigningKey::from_bytes(&[2; 32]);
            let phone_key = SigningKey::from_bytes(&[9; 32]);
            let old = earlier_build_credential();
            assert_eq!(old.sealed_secret.first(), Some(&WIFI_ENVELOPE_V1));
            let password = open_wifi_credential(&old, &laptop_key).unwrap();
            let author = device(&author_key);
            let rewrapped = seal_wifi_credential(
                &old.ssid,
                old.security,
                &password,
                &author,
                [author.clone(), device(&laptop_key), device(&phone_key)],
                FabricStamp {
                    counter: 9,
                    device: author.clone(),
                },
            )
            .unwrap();
            assert_eq!(rewrapped.sealed_secret.first(), Some(&WIFI_ENVELOPE_V2));
            for key in [&author_key, &laptop_key, &phone_key] {
                assert_eq!(
                    open_wifi_credential(&rewrapped, key).unwrap(),
                    "kat wifi passphrase"
                );
            }
        }

        fn unknown_bundle_versions_are_rejected() {
            let mut credential = earlier_build_credential();
            credential.sealed_secret[0] = 3;
            assert_eq!(
                validate_wifi_envelopes(&credential.sealed_secret),
                Err(WifiSecretError::InvalidCredential)
            );
        }

        credential_sealed_by_an_earlier_build_still_opens();
        new_credentials_are_version_two_hpke_boxes();
        version_one_credential_rewraps_to_version_two();
        unknown_bundle_versions_are_rejected();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn sealed_credentials_open_only_for_certified_recipients_with_authenticated_metadata() {
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

        fn swapped_recipient_boxes_do_not_open() {
            let a_key = SigningKey::from_bytes(&[7; 32]);
            let b_key = SigningKey::from_bytes(&[8; 32]);
            let author = device(&a_key);
            let mut credential = seal_wifi_credential(
                "Home",
                os_actions::WifiSecurity::Wpa3,
                "secret",
                &author,
                [author.clone(), device(&b_key)],
                FabricStamp {
                    counter: 1,
                    device: author.clone(),
                },
            )
            .unwrap();
            let WifiEnvelopeBundle::V2(mut bundle) =
                decode_bundle(&credential.sealed_secret).unwrap()
            else {
                panic!("expected a version 2 bundle");
            };
            let first = bundle.envelopes[0].sealed.clone();
            bundle.envelopes[0].sealed = bundle.envelopes[1].sealed.clone();
            bundle.envelopes[1].sealed = first;
            credential.sealed_secret = postcard::to_allocvec(&bundle).unwrap();
            for key in [&a_key, &b_key] {
                assert_eq!(
                    open_wifi_credential(&credential, key),
                    Err(WifiSecretError::DecryptionFailed)
                );
            }
        }

        each_certified_recipient_can_open_but_an_outsider_cannot();
        public_metadata_is_authenticated_as_associated_data();
        security_mode_is_authenticated_and_enterprise_cannot_be_password_only();
        swapped_recipient_boxes_do_not_open();
    }
}
