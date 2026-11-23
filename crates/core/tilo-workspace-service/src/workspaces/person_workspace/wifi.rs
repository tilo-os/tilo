//! The Wi-Fi passphrases the person's devices share, as their own workspace keeps them
//! ([`super::devices::Devices::keep_wifi`]): one per network, sealed to each of their devices.
//!
//! The record holds only ciphertext. Each device gets its own envelope sealed to its key, so
//! adding or revoking a device does not depend on a vendor cloud account or one shared key copied
//! everywhere. Each copy is sealed with HPKE through `tilo-seal` (label
//! [`Label::DeviceFabricSecret`]) to the recipient's Ed25519 device key in its X25519 form, with
//! the credential's public facts (who sealed it, for whom, which network) as associated data.
//! The bundle's version is its first postcard byte; any other version is refused.

use std::collections::BTreeSet;

use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use tilo_seal::{Label, PublicKey, SealedBox, SecretKey};
pub use zeroize::Zeroizing;

const ENVELOPE_VERSION: u8 = 2;
const KEY_DOMAIN: &[u8] = b"tilo-personal-fabric-wifi-envelope-v1\0";
const MAX_PASSWORD_BYTES: usize = 4096;
const MAX_SSID_BYTES: usize = 128;
/// The most devices one passphrase is sealed to.
pub const MAX_RECIPIENTS: usize = 64;
/// The most bytes the envelopes of one passphrase take.
const MAX_SEALED_BYTES: usize = 16 * 1024;
/// HPKE sealed-box overhead: version byte, encapsulated key, AEAD tag.
const SEALED_BOX_OVERHEAD: usize = 1 + 32 + 16;

/// How a network a passphrase joins is secured: the two kinds a passphrase alone joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Security {
    Wpa2,
    Wpa3,
}

impl Security {
    /// The byte the network's id and the associated data name it by.
    const fn tag(self) -> u8 {
        match self {
            Self::Wpa2 => 1,
            Self::Wpa3 => 2,
        }
    }

    /// Its name in an action's arguments.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Wpa2 => "wpa2",
            Self::Wpa3 => "wpa3",
        }
    }

    /// The security `name` names ([`Self::name`]).
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "wpa2" => Some(Self::Wpa2),
            "wpa3" => Some(Self::Wpa3),
            _ => None,
        }
    }
}

/// One network's passphrase, sealed to the devices that may open it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub network_id: [u8; 32],
    pub ssid: String,
    pub security: Security,
    /// The envelopes, one per recipient.
    pub sealed: Vec<u8>,
    pub nonce: [u8; 24],
    /// The device that sealed it.
    pub author: [u8; 32],
    /// When it was sealed, ms since the epoch: the newer of two for one network wins.
    pub sealed_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Envelope {
    recipient: [u8; 32],
    /// `tilo_seal::SealedBox` wire bytes.
    sealed: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Bundle {
    version: u8,
    envelopes: Vec<Envelope>,
}

/// Why a passphrase was not sealed or opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("the Wi-Fi network or its passphrase is not one a passphrase alone joins")]
    Invalid,
    #[error("the Wi-Fi passphrase was not shared with this device")]
    NotARecipient,
    #[error("the Wi-Fi passphrase could not be opened")]
    DecryptionFailed,
    #[error("the Wi-Fi passphrase is sealed to more devices than it can be")]
    TooLarge,
}

impl Credential {
    /// The postcard bytes of it in hex: what the record keeps.
    ///
    /// # Panics
    /// Never: a credential always encodes.
    #[must_use]
    pub fn to_hex(&self) -> String {
        #[allow(
            clippy::expect_used,
            reason = "a struct of plain fields always encodes"
        )]
        hex::encode(postcard::to_allocvec(self).expect("a credential encodes"))
    }

    /// The credential `hex` holds, when it reads and is well formed.
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        let bytes = hex::decode(hex).ok()?;
        let (credential, rest) = postcard::take_from_bytes::<Self>(&bytes).ok()?;
        (rest.is_empty() && credential.validate().is_ok()).then_some(credential)
    }

    /// The record key it is kept under: its network's id, hex.
    #[must_use]
    pub fn network(&self) -> String {
        hex::encode(self.network_id)
    }

    /// Whether this is a sealed passphrase of a network one could join.
    ///
    /// # Errors
    /// [`Refused::Invalid`] for anything else.
    pub fn validate(&self) -> Result<(), Refused> {
        if self.ssid.is_empty()
            || self.ssid.len() > MAX_SSID_BYTES
            || self.network_id != network_id(&self.ssid, self.security)
        {
            return Err(Refused::Invalid);
        }
        validate_envelopes(&self.sealed)
    }

    /// Seal `password` of the network `ssid` to each of `recipients`, as `author` (whose key is
    /// among them or not) at `sealed_unix_ms`.
    ///
    /// # Errors
    /// The network, the passphrase or the recipients are not ones to seal.
    pub fn seal(
        ssid: &str,
        security: Security,
        password: &str,
        author: [u8; 32],
        recipients: impl IntoIterator<Item = [u8; 32]>,
        sealed_unix_ms: u64,
    ) -> Result<Self, Refused> {
        if ssid.is_empty()
            || ssid.len() > MAX_SSID_BYTES
            || password.is_empty()
            || password.len() > MAX_PASSWORD_BYTES
        {
            return Err(Refused::Invalid);
        }
        let recipients = recipients.into_iter().collect::<BTreeSet<_>>();
        if recipients.is_empty() || recipients.len() > MAX_RECIPIENTS {
            return Err(Refused::Invalid);
        }
        let network_id = network_id(ssid, security);
        let mut nonce = [0; 24];
        getrandom::fill(&mut nonce).map_err(|_| Refused::Invalid)?;
        let mut envelopes = Vec::with_capacity(recipients.len());
        for recipient in recipients {
            let aad = aad(&author, &recipient, ssid, security, network_id, nonce);
            let sealed = tilo_seal::hpke::seal(
                &public_key(&recipient)?,
                Label::DeviceFabricSecret,
                &aad,
                password.as_bytes(),
            )
            .map_err(|_| Refused::Invalid)?;
            envelopes.push(Envelope {
                recipient,
                sealed: sealed.to_bytes(),
            });
        }
        let sealed = postcard::to_allocvec(&Bundle {
            version: ENVELOPE_VERSION,
            envelopes,
        })
        .map_err(|_| Refused::Invalid)?;
        if sealed.len() > MAX_SEALED_BYTES {
            return Err(Refused::TooLarge);
        }
        let credential = Self {
            network_id,
            ssid: ssid.to_owned(),
            security,
            sealed,
            nonce,
            author,
            sealed_unix_ms,
        };
        credential.validate()?;
        Ok(credential)
    }

    /// Open the envelope sealed to `key`'s device, without touching any other recipient's.
    ///
    /// # Errors
    /// It was not sealed to that device, or what it says of itself was changed.
    pub fn open(&self, key: &SigningKey) -> Result<Zeroizing<String>, Refused> {
        let bundle = decode(&self.sealed)?;
        let recipient = key.verifying_key().to_bytes();
        let envelope = bundle
            .envelopes
            .iter()
            .find(|envelope| envelope.recipient == recipient)
            .ok_or(Refused::NotARecipient)?;
        let sealed = SealedBox::from_bytes(&envelope.sealed).map_err(|_| Refused::Invalid)?;
        let aad = aad(
            &self.author,
            &recipient,
            &self.ssid,
            self.security,
            self.network_id,
            self.nonce,
        );
        let plaintext = tilo_seal::hpke::open(
            &SecretKey::from_bytes(key.to_scalar_bytes()),
            Label::DeviceFabricSecret,
            &aad,
            &sealed,
        )
        .map_err(|_| Refused::DecryptionFailed)?;
        // `plaintext` is zeroized on drop; only the returned copy survives, zeroized in turn.
        let password = Zeroizing::new(
            std::str::from_utf8(&plaintext)
                .map_err(|_| Refused::DecryptionFailed)?
                .to_owned(),
        );
        if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
            return Err(Refused::DecryptionFailed);
        }
        Ok(password)
    }
}

fn validate_envelopes(bytes: &[u8]) -> Result<(), Refused> {
    let bundle = decode(bytes)?;
    if bundle.envelopes.is_empty() || bundle.envelopes.len() > MAX_RECIPIENTS {
        return Err(Refused::Invalid);
    }
    let mut recipients = BTreeSet::new();
    let valid = bundle.envelopes.iter().all(|envelope| {
        recipients.insert(envelope.recipient)
            && envelope.sealed.len() > SEALED_BOX_OVERHEAD
            && envelope.sealed.len() <= MAX_PASSWORD_BYTES + SEALED_BOX_OVERHEAD
            && SealedBox::from_bytes(&envelope.sealed).is_ok()
    });
    if valid { Ok(()) } else { Err(Refused::Invalid) }
}

fn decode(bytes: &[u8]) -> Result<Bundle, Refused> {
    if bytes.is_empty() || bytes.len() > MAX_SEALED_BYTES {
        return Err(Refused::TooLarge);
    }
    postcard::take_from_bytes::<Bundle>(bytes)
        .ok()
        .filter(|(bundle, rest)| rest.is_empty() && bundle.version == ENVELOPE_VERSION)
        .map(|(bundle, _)| bundle)
        .ok_or(Refused::Invalid)
}

/// The X25519 form of a device's Ed25519 key.
fn public_key(device: &[u8; 32]) -> Result<PublicKey, Refused> {
    Ok(PublicKey::from_bytes(
        VerifyingKey::from_bytes(device)
            .map_err(|_| Refused::Invalid)?
            .to_montgomery()
            .to_bytes(),
    ))
}

/// The id of the network `ssid` secured by `security`: what its passphrase is kept under.
#[must_use]
pub fn network_id(ssid: &str, security: Security) -> [u8; 32] {
    let mut material = KEY_DOMAIN.to_vec();
    material.extend_from_slice(b"network\0");
    material.extend_from_slice(ssid.as_bytes());
    material.push(0);
    material.push(security.tag());
    *blake3::hash(&material).as_bytes()
}

/// Associated data of an envelope. HPKE already binds the encapsulated key, so it is not repeated
/// here.
fn aad(
    author: &[u8; 32],
    recipient: &[u8; 32],
    ssid: &str,
    security: Security,
    network_id: [u8; 32],
    nonce: [u8; 24],
) -> Vec<u8> {
    let mut aad = KEY_DOMAIN.to_vec();
    aad.extend_from_slice(hex::encode(author).as_bytes());
    aad.push(0);
    aad.extend_from_slice(hex::encode(recipient).as_bytes());
    aad.push(0);
    aad.extend_from_slice(ssid.as_bytes());
    aad.push(0);
    aad.push(security.tag());
    aad.extend_from_slice(&network_id);
    aad.extend_from_slice(&nonce);
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(key: &SigningKey) -> [u8; 32] {
        key.verifying_key().to_bytes()
    }

    fn sealed(key: &SigningKey, recipients: &[&SigningKey]) -> Credential {
        Credential::seal(
            "Home",
            Security::Wpa3,
            "correct horse battery staple",
            device(key),
            recipients.iter().map(|key| device(key)),
            2,
        )
        .expect("a passphrase seals")
    }

    #[test]
    fn a_passphrase_opens_only_for_its_recipients_and_only_as_it_was_sealed() {
        let author = SigningKey::from_bytes(&[1; 32]);
        let laptop = SigningKey::from_bytes(&[2; 32]);
        let outsider = SigningKey::from_bytes(&[3; 32]);
        let credential = sealed(&author, &[&author, &laptop]);
        assert_eq!(
            hex::encode(network_id("Home", Security::Wpa3)),
            "3acb148a49b7b11b0709306da205cc903420497d6fb8e9efb16a3adac3d1e80c",
            "a network keeps the id its passphrase was kept under"
        );
        for key in [&author, &laptop] {
            assert_eq!(
                credential.open(key).expect("a recipient opens it").as_str(),
                "correct horse battery staple"
            );
        }
        assert_eq!(credential.open(&outsider), Err(Refused::NotARecipient));
        assert!(
            !credential
                .sealed
                .windows(b"correct horse".len())
                .any(|window| window == b"correct horse"),
            "the record holds no passphrase in the clear"
        );
        assert_eq!(
            Credential::from_hex(&credential.to_hex()),
            Some(credential.clone())
        );

        // What it says of itself is sealed with it.
        let mut renamed = credential.clone();
        renamed.ssid = "Lookalike".into();
        renamed.network_id = network_id("Lookalike", Security::Wpa3);
        assert_eq!(renamed.open(&author), Err(Refused::DecryptionFailed));
        let mut reauthored = credential.clone();
        reauthored.author = device(&outsider);
        assert_eq!(reauthored.open(&author), Err(Refused::DecryptionFailed));
        let mut renonced = credential.clone();
        renonced.nonce[0] ^= 1;
        assert_eq!(renonced.open(&author), Err(Refused::DecryptionFailed));
        let mut downgraded = credential.clone();
        downgraded.security = Security::Wpa2;
        assert_eq!(downgraded.validate(), Err(Refused::Invalid));

        // Two recipients' boxes swapped open for neither.
        let mut bundle = decode(&credential.sealed).expect("its envelopes read");
        let first = bundle.envelopes[0].sealed.clone();
        bundle.envelopes[0].sealed = bundle.envelopes[1].sealed.clone();
        bundle.envelopes[1].sealed = first;
        let mut swapped = credential.clone();
        swapped.sealed = postcard::to_allocvec(&bundle).expect("a bundle encodes");
        for key in [&author, &laptop] {
            assert_eq!(swapped.open(key), Err(Refused::DecryptionFailed));
        }

        // A bundle of another version is no credential.
        let mut versioned = credential;
        versioned.sealed[0] = 3;
        assert_eq!(versioned.validate(), Err(Refused::Invalid));
        assert_eq!(Credential::from_hex(&versioned.to_hex()), None);
    }

    #[test]
    fn nothing_is_sealed_that_a_passphrase_alone_does_not_join() {
        let key = SigningKey::from_bytes(&[4; 32]);
        for (ssid, password) in [("", "secret"), ("Home", ""), (&*"x".repeat(129), "secret")] {
            assert_eq!(
                Credential::seal(
                    ssid,
                    Security::Wpa2,
                    password,
                    device(&key),
                    [device(&key)],
                    1
                ),
                Err(Refused::Invalid)
            );
        }
        assert_eq!(
            Credential::seal("Home", Security::Wpa2, "secret", device(&key), [], 1),
            Err(Refused::Invalid),
            "a passphrase is sealed to at least one device"
        );
    }
}
