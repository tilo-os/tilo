//! The probe and reply datagrams (see the crate documentation for the
//! exchange they carry).

use std::net::SocketAddr;

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use tilo_seal::{Context, Label, PublicKey, SealedBox, SecretKey, SymmetricKey, envelope, hpke};
use tilo_user_identity::DeviceCert;

/// First bytes of a probe.
pub const PROBE_MAGIC: [u8; 8] = *b"TLOLINK2";
/// First bytes of a reply.
pub const REPLY_MAGIC: [u8; 8] = *b"TLOLNKR2";
/// Slots in every probe, used or not.
pub const SLOTS: usize = 8;
/// Most addresses a reply carries.
pub const MAX_REPLY_ADDRS: usize = 16;
/// Length of every probe and every reply.
pub const DATAGRAM_LEN: usize = PROBE_HEADER_LEN + SLOTS * SLOT_LEN + BODY_LEN;

const SALT_LEN: usize = 16;
const PROBE_HEADER_LEN: usize = PROBE_MAGIC.len() + 4 + SALT_LEN;
/// HPKE sealed-box overhead: version byte, encapsulated key, AEAD tag.
const SEALED_OVERHEAD: usize = 1 + 32 + 16;
/// A slot: the probe's body key sealed to one device.
const SLOT_LEN: usize = SEALED_OVERHEAD + 32;
/// Room for the asking device's certificate in the body.
const CERT_CAP: usize = 448;
const SIGNATURE_LEN: usize = 64;
const CERT_FIELD_END: usize = 2 + CERT_CAP;
const BODY_PLAIN_LEN: usize = CERT_FIELD_END + SIGNATURE_LEN;
/// `tilo-seal` envelope overhead: magic, version, algorithm, key source,
/// key id, nonce, tag.
const ENVELOPE_OVERHEAD: usize = 5 + 1 + 1 + 1 + 8 + 24 + 16;
const BODY_LEN: usize = ENVELOPE_OVERHEAD + BODY_PLAIN_LEN;
const REPLY_HEADER_LEN: usize = REPLY_MAGIC.len() + SALT_LEN;
const REPLY_AAD_LEN: usize = REPLY_HEADER_LEN + 32;
/// What the asking device signs: the header and the slots.
const SIGNED_LEN: usize = PROBE_HEADER_LEN + SLOTS * SLOT_LEN;
const REPLY_PLAIN_LEN: usize = DATAGRAM_LEN - REPLY_HEADER_LEN - SEALED_OVERHEAD;
const PROBE_CONTEXT: &[u8] = b"tilo-lan-probe-v2\0";
const REPLY_CONTEXT: &[u8] = b"tilo-lan-reply-v2\0";

/// The salt that ties a reply to the probe it answers.
pub type Salt = [u8; SALT_LEN];

/// A device's X25519 sealing keys, derived from its Ed25519 identity the
/// way the personal device fabric seals to a device.
pub(crate) fn sealing_secret(device: &SigningKey) -> SecretKey {
    SecretKey::from_bytes(device.to_scalar_bytes())
}

pub(crate) fn sealing_public(device: &[u8; 32]) -> Option<PublicKey> {
    let key = VerifyingKey::from_bytes(device).ok()?;
    Some(PublicKey::from_bytes(key.to_montgomery().to_bytes()))
}

/// Reads a datagram front to back, never past its end.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(head)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
}

fn random<const N: usize>() -> Option<[u8; N]> {
    let mut bytes = [0_u8; N];
    tilo_seal::fill_random(&mut bytes).ok()?;
    Some(bytes)
}

/// A probe this device sent, kept to open the replies to it.
#[derive(Clone, Debug)]
pub struct SentProbe {
    pub salt: Salt,
    pub asked: Vec<[u8; 32]>,
}

/// Build a probe from `device`, whose certificate is `cert`, asking for
/// `asked` (at most [`SLOTS`]; the caller picks which).
pub fn probe(
    device: &SigningKey,
    cert: &DeviceCert,
    asked: &[[u8; 32]],
    epoch: u32,
) -> Result<(Vec<u8>, SentProbe), &'static str> {
    if asked.len() > SLOTS {
        return Err("too many devices for one probe");
    }
    let salt: Salt = random().ok_or("no randomness")?;
    let mut out = Vec::with_capacity(DATAGRAM_LEN);
    out.extend_from_slice(&PROBE_MAGIC);
    out.extend_from_slice(&epoch.to_le_bytes());
    out.extend_from_slice(&salt);
    let header = out.clone();
    let body_key = SymmetricKey::generate().map_err(|_| "no randomness")?;
    let mut slots: Vec<Vec<u8>> = Vec::with_capacity(SLOTS);
    for device in asked {
        let recipient = sealing_public(device).ok_or("not a device key")?;
        slots.push(
            hpke::seal_to_bytes(
                &recipient,
                Label::LanDiscoveryProbe,
                &header,
                body_key.expose_secret(),
            )
            .map_err(|_| "could not seal a slot")?,
        );
    }
    while slots.len() < SLOTS {
        // Indistinguishable from a used slot: a real box to a key nobody
        // holds.
        let nobody = SecretKey::generate()
            .and_then(|key| key.public_key())
            .map_err(|_| "no randomness")?;
        let filler: [u8; 32] = random().ok_or("no randomness")?;
        slots.push(
            hpke::seal_to_bytes(&nobody, Label::LanDiscoveryProbe, &header, &filler)
                .map_err(|_| "could not seal a slot")?,
        );
    }
    // Sorted, so the order says nothing about which slot is whose.
    slots.sort_unstable();
    for slot in &slots {
        out.extend_from_slice(slot);
    }
    let signed = out.clone();
    let cert_bytes = postcard::to_allocvec(cert).map_err(|_| "certificate does not encode")?;
    if cert_bytes.len() > CERT_CAP {
        return Err("certificate too large for a probe");
    }
    let mut body = Vec::with_capacity(BODY_PLAIN_LEN);
    body.extend_from_slice(
        &u16::try_from(cert_bytes.len())
            .map_err(|_| "certificate too large")?
            .to_le_bytes(),
    );
    body.extend_from_slice(&cert_bytes);
    body.resize(CERT_FIELD_END, 0);
    body.extend_from_slice(&device.sign(&probe_digest(&signed)).to_bytes());
    let sealed = envelope::seal(
        &body_key,
        Context::new(Label::LanDiscoveryProbe, &signed),
        &body,
    )
    .map_err(|_| "could not seal the probe")?;
    out.extend_from_slice(&sealed);
    Ok((
        out,
        SentProbe {
            salt,
            asked: asked.to_vec(),
        },
    ))
}

fn probe_digest(signed: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PROBE_CONTEXT);
    hasher.update(signed);
    *hasher.finalize().as_bytes()
}

/// A probe that asked for this device, opened.
#[derive(Clone, Debug)]
pub struct OpenedProbe {
    pub epoch: u32,
    pub salt: Salt,
    /// The asking device's certificate. Its key signed the probe; whether
    /// the certificate is valid and the person's is for the caller to check.
    pub asker: DeviceCert,
}

/// The epoch and salt of any probe, before the work of opening it.
#[must_use]
pub fn probe_header(datagram: &[u8]) -> Option<(u32, Salt)> {
    if datagram.len() != DATAGRAM_LEN {
        return None;
    }
    let mut reader = Reader(datagram);
    if reader.array::<8>()? != PROBE_MAGIC {
        return None;
    }
    let epoch = u32::from_le_bytes(reader.array()?);
    Some((epoch, reader.array()?))
}

/// Open `datagram` if it is a probe asking for `device`, signed by the
/// device its certificate names.
#[must_use]
pub fn open_probe(device: &SigningKey, datagram: &[u8]) -> Option<OpenedProbe> {
    let (epoch, salt) = probe_header(datagram)?;
    let (signed, sealed_body) = datagram.split_at_checked(SIGNED_LEN)?;
    let (header, slots) = signed.split_at_checked(PROBE_HEADER_LEN)?;
    let secret = sealing_secret(device);
    let body_key = slots.chunks_exact(SLOT_LEN).find_map(|slot| {
        let sealed = SealedBox::from_bytes(slot).ok()?;
        let key = hpke::open(&secret, Label::LanDiscoveryProbe, header, &sealed).ok()?;
        SymmetricKey::from_slice(&key).ok()
    })?;
    let body = envelope::open(
        &body_key,
        Context::new(Label::LanDiscoveryProbe, signed),
        sealed_body,
    )
    .ok()?;
    if body.len() != BODY_PLAIN_LEN {
        return None;
    }
    let mut reader = Reader(&body);
    let cert_len = usize::from(u16::from_le_bytes(reader.array()?));
    let padded_cert = reader.take(CERT_CAP)?;
    let asker: DeviceCert = postcard::from_bytes(padded_cert.get(..cert_len)?).ok()?;
    let signature = Signature::from_bytes(&reader.array()?);
    VerifyingKey::from_bytes(&asker.device_public_key)
        .ok()?
        .verify(&probe_digest(signed), &signature)
        .ok()?;
    Some(OpenedProbe { epoch, salt, asker })
}

fn reply_digest(salt: &Salt, asker: &[u8; 32], answerer: &[u8; 32], addrs: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(REPLY_CONTEXT);
    hasher.update(salt);
    hasher.update(asker);
    hasher.update(answerer);
    hasher.update(addrs);
    *hasher.finalize().as_bytes()
}

fn reply_aad(salt: &Salt, asker: &[u8; 32]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(REPLY_AAD_LEN);
    aad.extend_from_slice(&REPLY_MAGIC);
    aad.extend_from_slice(salt);
    aad.extend_from_slice(asker);
    aad
}

/// Answer `probe` from `device`, naming the addresses its endpoint listens
/// on (at most [`MAX_REPLY_ADDRS`] are kept).
pub fn reply(
    device: &SigningKey,
    probe: &OpenedProbe,
    addrs: &[SocketAddr],
) -> Result<Vec<u8>, &'static str> {
    let asker = probe.asker.device_public_key;
    let answerer = device.verifying_key().to_bytes();
    let mut addrs = addrs.to_vec();
    addrs.truncate(MAX_REPLY_ADDRS);
    let addr_bytes = postcard::to_allocvec(&addrs).map_err(|_| "addresses do not encode")?;
    let mut plain = Vec::with_capacity(REPLY_PLAIN_LEN);
    plain.extend_from_slice(&answerer);
    plain.extend_from_slice(
        &device
            .sign(&reply_digest(&probe.salt, &asker, &answerer, &addr_bytes))
            .to_bytes(),
    );
    plain.extend_from_slice(
        &u16::try_from(addr_bytes.len())
            .map_err(|_| "too many addresses")?
            .to_le_bytes(),
    );
    plain.extend_from_slice(&addr_bytes);
    if plain.len() > REPLY_PLAIN_LEN {
        return Err("addresses do not fit a reply");
    }
    plain.resize(REPLY_PLAIN_LEN, 0);
    let recipient = sealing_public(&asker).ok_or("not a device key")?;
    let sealed = hpke::seal_to_bytes(
        &recipient,
        Label::LanDiscoveryReply,
        &reply_aad(&probe.salt, &asker),
        &plain,
    )
    .map_err(|_| "could not seal the reply")?;
    let mut out = Vec::with_capacity(DATAGRAM_LEN);
    out.extend_from_slice(&REPLY_MAGIC);
    out.extend_from_slice(&probe.salt);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// A device that answered a probe of this device, and where it listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub device: [u8; 32],
    pub addrs: Vec<SocketAddr>,
}

/// The salt of a reply, to find the probe it answers.
#[must_use]
pub fn reply_salt(datagram: &[u8]) -> Option<Salt> {
    if datagram.len() != DATAGRAM_LEN {
        return None;
    }
    let mut reader = Reader(datagram);
    if reader.array::<8>()? != REPLY_MAGIC {
        return None;
    }
    reader.array()
}

/// Open `datagram` as a reply to `sent`, a probe of `device`: sealed to it,
/// from one of the devices it asked for, signed by that device.
#[must_use]
pub fn open_reply(device: &SigningKey, sent: &SentProbe, datagram: &[u8]) -> Option<Reply> {
    if reply_salt(datagram)? != sent.salt {
        return None;
    }
    let asker = device.verifying_key().to_bytes();
    let plain = hpke::open_bytes(
        &sealing_secret(device),
        Label::LanDiscoveryReply,
        &reply_aad(&sent.salt, &asker),
        datagram.get(REPLY_HEADER_LEN..)?,
    )
    .ok()?;
    if plain.len() != REPLY_PLAIN_LEN {
        return None;
    }
    let mut reader = Reader(&plain);
    let answerer: [u8; 32] = reader.array()?;
    if !sent.asked.contains(&answerer) {
        return None;
    }
    let signature = Signature::from_bytes(&reader.array()?);
    let len = usize::from(u16::from_le_bytes(reader.array()?));
    let addr_bytes = reader.take(len)?;
    VerifyingKey::from_bytes(&answerer)
        .ok()?
        .verify(
            &reply_digest(&sent.salt, &asker, &answerer, addr_bytes),
            &signature,
        )
        .ok()?;
    let addrs: Vec<SocketAddr> = postcard::from_bytes(addr_bytes).ok()?;
    (addrs.len() <= MAX_REPLY_ADDRS).then_some(Reply {
        device: answerer,
        addrs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    fn key(byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[byte; 32])
    }

    fn cert(root: &SigningKey, device: &SigningKey) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    #[test]
    fn a_probe_opens_only_for_the_devices_it_asks_and_names_its_signer() {
        let root = key(1);
        let (asker, target, other) = (key(2), key(3), key(4));
        let asker_cert = cert(&root, &asker);
        let (datagram, sent) =
            probe(&asker, &asker_cert, &[target.verifying_key().to_bytes()], 7).unwrap();
        assert_eq!(datagram.len(), DATAGRAM_LEN);
        assert!(open_probe(&other, &datagram).is_none());
        let opened = open_probe(&target, &datagram).unwrap();
        assert_eq!(opened.asker, asker_cert);
        assert_eq!((opened.epoch, opened.salt), (7, sent.salt));

        let mut tampered = datagram;
        tampered[9] ^= 1;
        assert!(
            open_probe(&target, &tampered).is_none(),
            "the epoch is bound"
        );
    }

    #[test]
    fn a_probe_signed_by_another_key_than_its_certificate_is_refused() {
        let root = key(1);
        let (asker, impostor, target) = (key(2), key(5), key(3));
        // A collaborator holding the asker's certificate cannot sign as it.
        let (datagram, _) = probe(
            &impostor,
            &cert(&root, &asker),
            &[target.verifying_key().to_bytes()],
            1,
        )
        .unwrap();
        assert!(open_probe(&target, &datagram).is_none());
    }

    #[test]
    fn probes_with_more_or_fewer_devices_look_alike() {
        let root = key(1);
        let asker = key(2);
        let target = [key(3).verifying_key().to_bytes()];
        let one = probe(&asker, &cert(&root, &asker), &target, 1).unwrap().0;
        let none = probe(&asker, &cert(&root, &asker), &[], 1).unwrap().0;
        assert_eq!(one.len(), none.len());
        assert_ne!(
            one[..PROBE_HEADER_LEN],
            none[..PROBE_HEADER_LEN],
            "fresh salt"
        );
    }

    #[test]
    fn a_reply_reaches_only_the_asker_from_a_device_it_asked() {
        let root = key(1);
        let (asker, target, stranger) = (key(2), key(3), key(4));
        let (datagram, sent) = probe(
            &asker,
            &cert(&root, &asker),
            &[target.verifying_key().to_bytes()],
            1,
        )
        .unwrap();
        let opened = open_probe(&target, &datagram).unwrap();
        let addrs: Vec<SocketAddr> = vec!["10.0.0.5:4433".parse().unwrap()];
        let answer = reply(&target, &opened, &addrs).unwrap();
        assert_eq!(answer.len(), DATAGRAM_LEN, "no larger than the probe");
        assert_eq!(
            open_reply(&asker, &sent, &answer),
            Some(Reply {
                device: target.verifying_key().to_bytes(),
                addrs: addrs.clone(),
            })
        );
        assert!(open_reply(&stranger, &sent, &answer).is_none());
        // A device the probe did not ask for cannot redirect the asker.
        let forged = OpenedProbe {
            asker: cert(&root, &asker),
            ..opened
        };
        let unasked = reply(&stranger, &forged, &addrs).unwrap();
        assert!(open_reply(&asker, &sent, &unasked).is_none());
    }
}
