//! "Linked device, where are you?" on the nearby rendezvous: how one
//! person's linked devices find each other's endpoint addresses on a local
//! network, without telling anyone else whose devices they are.
//!
//! # Exchange
//!
//! 1. A session broadcasts a [`Probe`] to [`super::NEARBY_PORT`] from a
//!    socket of its own, on every interface.
//! 2. A session listening there that finds a slot made for it among the
//!    probe's slots knows which linked device asked. It answers that socket,
//!    once per probe, with a reply sealed to the asking device's key.
//! 3. The asker opens the reply, checks it came from a device it trusts,
//!    and dials that device's endpoint at the addresses the reply names.
//!
//! Finding a device is not trusting it. The dial goes through the same path
//! as the enrolled direct-peer list: the endpoint key is authenticated by the
//! transport, and the device certificate and revocations are checked before
//! anything syncs.
//!
//! # Probe
//!
//! `"TLOLINK1" || epoch (u32 LE) || salt (16) || slot[0] … slot[7] (12 each)
//! || zero padding`, [`DATAGRAM_LEN`] bytes. `epoch` counts [`EPOCH_SECS`]
//! steps (see *Freshness*); `salt` is fresh random bytes for every probe. For
//! each linked device `P` the asking device `A` wants to reach:
//!
//! ```text
//! K_AP     = tilo_seal::agreement::shared_key(X25519(A), X25519(P), LanDiscovery)
//! slot_A→P = BLAKE3-keyed(K_AP, "tilo-linked-probe-v1\0" || epoch || salt || A)[..12]
//! ```
//!
//! `X25519(·)` is the device's Ed25519 key in its X25519 form, the same
//! conversion the pairing offers and the personal fabric use to seal to a
//! device. Unused slots are random and the slots are sorted, so the probe's
//! bytes say nothing about how many devices the person has or which slot is
//! whose. With more than [`SLOTS`] linked devices a uniformly random
//! [`SLOTS`] of them get a slot in each probe.
//!
//! `P` recognises a probe by computing `slot_C→P` for each linked device `C`
//! it still trusts. Only `A` and `P` can compute `K_AP`: nothing is shared
//! across the fleet, so a device that left it (or was revoked) learns nothing
//! from probes not made for it, and once `A` stops trusting `P` it stops
//! making slots for `P` and stops answering `P`. No secret has to be
//! redistributed. `A→P` and `P→A` slots differ (the sender is hashed in), so
//! two of a person's devices never show an observer a common value.
//!
//! # Reply
//!
//! `"TLOLINKR" || salt || HPKE box`, also [`DATAGRAM_LEN`] bytes, so neither
//! direction's size says anything about the device and a reply is no larger
//! than the probe that caused it. The box is sealed to `X25519(A)` with
//! [`tilo_seal::Label::LanDiscoveryReply`] and associated data
//! `"TLOLINKR" || salt || A`; inside, zero-padded to a fixed length, are the
//! answering device's key `P`, its endpoint's local addresses, and
//! `BLAKE3-keyed(K_AP, "tilo-linked-reply-v1\0" || salt || P || addresses)[..16]`,
//! which proves to `A` that `P` itself answered this probe and is not a
//! stranger redirecting `A`'s dial.
//!
//! # Freshness
//!
//! A recorded probe replayed later, or on another network, must not make a
//! linked device answer and so give away that it is there. [`Freshness`]
//! decides which epochs a device sends and accepts:
//!
//! - A device whose clock is synchronised to a time source sends its own
//!   epoch and accepts probes within one epoch of it (about half an hour).
//! - A device without one (a board with no battery-backed clock on a network
//!   with no time source) does not send its own clock, which may be the
//!   image's build date and would single it out. It sends the newest epoch
//!   it learned from a linked device's probe, advanced by the time since, or
//!   0 when it has learned none.
//! - Every device remembers, per linked device and across restarts, the
//!   newest epoch it accepted from it, and refuses older probes from it.
//! - Each probe is answered once.
//!
//! What remains: a probe recorded from a device that has never been heard
//! from at a newer epoch, replayed to a linked device with no time source,
//! still gets one answer per recording. The answer is sealed to the device
//! that made the probe and comes from the answering device's own address.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tilo_seal::{Label, PublicKey, SecretKey, SymmetricKey};
use zeroize::Zeroizing;

/// First bytes of a probe.
pub const PROBE_MAGIC: [u8; 8] = *b"TLOLINK1";
/// First bytes of a reply.
pub const REPLY_MAGIC: [u8; 8] = *b"TLOLINKR";
/// Length of one epoch.
pub const EPOCH_SECS: u64 = 10 * 60;
/// Slots in every probe, used or not.
pub const SLOTS: usize = 8;
/// Most addresses a reply carries.
pub const MAX_REPLY_ADDRS: usize = 8;
const SALT_LEN: usize = 16;
const SLOT_LEN: usize = 12;
const MAC_LEN: usize = 16;
/// The sealed body of a reply, padded: version, device key, up to
/// [`MAX_REPLY_ADDRS`] addresses (at most 20 bytes each as postcard IPv6),
/// MAC, and length prefixes.
const REPLY_BODY_LEN: usize = 256;
/// HPKE sealed-box overhead: version byte, encapsulated key, AEAD tag.
const SEALED_OVERHEAD: usize = 1 + 32 + 16;
/// Where a reply's sealed box starts.
const REPLY_HEADER_LEN: usize = REPLY_MAGIC.len() + SALT_LEN;
/// Length of every probe and every reply.
pub const DATAGRAM_LEN: usize = REPLY_HEADER_LEN + SEALED_OVERHEAD + REPLY_BODY_LEN;
const PROBE_BODY_LEN: usize = PROBE_MAGIC.len() + 4 + SALT_LEN + SLOTS * SLOT_LEN;
const PROBE_CONTEXT: &[u8] = b"tilo-linked-probe-v1\0";
const REPLY_CONTEXT: &[u8] = b"tilo-linked-reply-v1\0";
const REPLY_VERSION: u8 = 1;

/// The salt that ties a reply to the probe it answers.
pub type Salt = [u8; SALT_LEN];

/// This device's key and the pairwise keys derived from it so far.
pub struct LinkKeys {
    local: [u8; 32],
    secret: SecretKey,
    pairs: HashMap<[u8; 32], Option<SymmetricKey>>,
}

impl LinkKeys {
    /// Keys of the device whose Ed25519 key is `device`.
    #[must_use]
    pub fn new(device: &ed25519_dalek::SigningKey) -> Self {
        let scalar = Zeroizing::new(device.to_scalar_bytes());
        Self {
            local: device.verifying_key().to_bytes(),
            secret: SecretKey::from_bytes(*scalar),
            pairs: HashMap::new(),
        }
    }

    /// This device's Ed25519 key.
    #[must_use]
    pub const fn local(&self) -> &[u8; 32] {
        &self.local
    }

    /// The key shared with `peer`; `None` for a key that is not a valid
    /// Ed25519 point, a low-order point, or this device.
    fn pair(&mut self, peer: [u8; 32]) -> Option<&SymmetricKey> {
        if peer == self.local {
            return None;
        }
        let secret = &self.secret;
        self.pairs
            .entry(peer)
            .or_insert_with(|| {
                let public = sealing_public(peer)?;
                tilo_seal::agreement::shared_key(secret, &public, Label::LanDiscovery).ok()
            })
            .as_ref()
    }

    /// Forget pairwise keys of devices not in `peers`.
    pub fn retain(&mut self, peers: &BTreeSet<[u8; 32]>) {
        self.pairs.retain(|peer, _| peers.contains(peer));
    }
}

fn sealing_public(device: [u8; 32]) -> Option<PublicKey> {
    let public = ed25519_dalek::VerifyingKey::from_bytes(&device).ok()?;
    Some(PublicKey::from_bytes(public.to_montgomery().to_bytes()))
}

fn keyed<const N: usize>(key: &SymmetricKey, parts: &[&[u8]]) -> [u8; N] {
    let mut hasher = blake3::Hasher::new_keyed(key.expose_secret());
    for part in parts {
        hasher.update(part);
    }
    let mut out = [0_u8; N];
    hasher.finalize_xof().fill(&mut out);
    out
}

fn slot(key: &SymmetricKey, epoch: u32, salt: &Salt, sender: &[u8; 32]) -> [u8; SLOT_LEN] {
    keyed(key, &[PROBE_CONTEXT, &epoch.to_le_bytes(), salt, sender])
}

/// The epoch of Unix time `unix_secs`.
#[must_use]
pub fn epoch_at(unix_secs: u64) -> u32 {
    u32::try_from(unix_secs / EPOCH_SECS).unwrap_or(u32::MAX)
}

/// A probe: which of the asker's linked devices are being asked, blinded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    epoch: u32,
    salt: Salt,
    slots: [[u8; SLOT_LEN]; SLOTS],
}

impl Probe {
    /// A probe each of `recipients` recognises as coming from this device,
    /// at `epoch`.
    ///
    /// # Errors
    ///
    /// When the operating system supplies no randomness.
    pub fn build(
        keys: &mut LinkKeys,
        recipients: &BTreeSet<[u8; 32]>,
        epoch: u32,
    ) -> tilo_seal::Result<Self> {
        let mut salt = [0_u8; SALT_LEN];
        tilo_seal::fill_random(&mut salt)?;
        let local = *keys.local();
        let mut chosen: Vec<([u8; 8], [u8; SLOT_LEN])> = Vec::new();
        for recipient in recipients {
            let Some(key) = keys.pair(*recipient) else {
                continue;
            };
            let mut order = [0_u8; 8];
            tilo_seal::fill_random(&mut order)?;
            chosen.push((order, slot(key, epoch, &salt, &local)));
        }
        chosen.sort_unstable();
        let mut slots: Vec<[u8; SLOT_LEN]> = chosen
            .into_iter()
            .take(SLOTS)
            .map(|(_, slot)| slot)
            .collect();
        while slots.len() < SLOTS {
            let mut filler = [0_u8; SLOT_LEN];
            tilo_seal::fill_random(&mut filler)?;
            slots.push(filler);
        }
        slots.sort_unstable();
        let mut fixed = [[0_u8; SLOT_LEN]; SLOTS];
        fixed.copy_from_slice(&slots);
        Ok(Self {
            epoch,
            salt,
            slots: fixed,
        })
    }

    /// The probe's salt, which its replies echo.
    #[must_use]
    pub const fn salt(&self) -> &Salt {
        &self.salt
    }

    /// The epoch the probe was made for.
    #[must_use]
    pub const fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The datagram.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(DATAGRAM_LEN);
        bytes.extend_from_slice(&PROBE_MAGIC);
        bytes.extend_from_slice(&self.epoch.to_le_bytes());
        bytes.extend_from_slice(&self.salt);
        for slot in &self.slots {
            bytes.extend_from_slice(slot);
        }
        bytes.resize(DATAGRAM_LEN, 0);
        bytes
    }

    /// Parse a datagram; `None` unless it is exactly a probe.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != DATAGRAM_LEN {
            return None;
        }
        let (body, padding) = bytes.split_at(PROBE_BODY_LEN);
        if padding.iter().any(|byte| *byte != 0) {
            return None;
        }
        let rest = body.strip_prefix(&PROBE_MAGIC)?;
        let (epoch, rest) = rest.split_first_chunk::<4>()?;
        let (salt, rest) = rest.split_first_chunk::<SALT_LEN>()?;
        let mut slots = [[0_u8; SLOT_LEN]; SLOTS];
        for (slot, chunk) in slots.iter_mut().zip(rest.chunks_exact(SLOT_LEN)) {
            slot.copy_from_slice(chunk);
        }
        Some(Self {
            epoch: u32::from_le_bytes(*epoch),
            salt: *salt,
            slots,
        })
    }

    /// The device among `candidates` that made this probe for this device.
    /// Whether the probe is fresh is [`Freshness::accepts`]'s question.
    pub fn sender<'a>(
        &self,
        keys: &mut LinkKeys,
        candidates: impl IntoIterator<Item = &'a [u8; 32]>,
    ) -> Option<[u8; 32]> {
        candidates.into_iter().copied().find(|candidate| {
            keys.pair(*candidate)
                .map(|key| slot(key, self.epoch, &self.salt, candidate))
                .is_some_and(|expected| self.slots.contains(&expected))
        })
    }
}

/// Which probe epochs a device sends and accepts (see the module docs).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Freshness {
    /// Per linked device (hex key), the newest epoch accepted from it.
    floors: BTreeMap<String, u32>,
    /// The newest epoch accepted from any linked device.
    learned: Option<u32>,
    #[serde(skip)]
    learned_at: Option<Instant>,
}

impl Freshness {
    /// Restore remembered epochs; anything unreadable starts afresh.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Self {
        let mut freshness: Self = serde_json::from_slice(bytes).unwrap_or_default();
        freshness.learned_at = freshness.learned.map(|_| Instant::now());
        freshness
    }

    /// What to persist.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// The learned epoch, advanced by the time since it was learned.
    fn estimate(&self) -> Option<u32> {
        let learned = self.learned?;
        let elapsed = self
            .learned_at
            .map_or(0, |at| at.elapsed().as_secs() / EPOCH_SECS);
        Some(learned.saturating_add(u32::try_from(elapsed).unwrap_or(u32::MAX)))
    }

    /// The epoch to put in this device's probes. `synced` is this device's
    /// own epoch when its clock is synchronised to a time source.
    #[must_use]
    pub fn outgoing(&self, synced: Option<u32>) -> u32 {
        synced.or_else(|| self.estimate()).unwrap_or(0)
    }

    /// Whether a probe from `asker` at `epoch` is fresh enough to answer.
    #[must_use]
    pub fn accepts(&self, asker: &[u8; 32], epoch: u32, synced: Option<u32>) -> bool {
        let floor = self.floors.get(&hex::encode(asker)).copied().unwrap_or(0);
        synced.is_none_or(|now| now.abs_diff(epoch) <= 1) && epoch.saturating_add(1) >= floor
    }

    /// Remember an accepted probe from `asker` at `epoch`. Returns whether
    /// anything to persist changed.
    pub fn record(&mut self, asker: &[u8; 32], epoch: u32) -> bool {
        let floor = self.floors.entry(hex::encode(asker)).or_insert(0);
        let mut changed = false;
        if epoch > *floor {
            *floor = epoch;
            changed = true;
        }
        if self.learned.is_none_or(|learned| epoch > learned) {
            self.learned = Some(epoch);
            self.learned_at = Some(Instant::now());
            changed = true;
        }
        changed
    }

    /// Forget devices not in `peers`.
    pub fn retain(&mut self, peers: &BTreeSet<[u8; 32]>) -> bool {
        let before = self.floors.len();
        let keep: BTreeSet<String> = peers.iter().map(hex::encode).collect();
        self.floors.retain(|peer, _| keep.contains(peer));
        self.floors.len() != before
    }
}

#[derive(Serialize, Deserialize)]
struct ReplyBody {
    version: u8,
    device: [u8; 32],
    addrs: Vec<SocketAddr>,
    mac: [u8; MAC_LEN],
}

fn reply_aad(salt: &Salt, asker: &[u8; 32]) -> Vec<u8> {
    [&REPLY_MAGIC[..], salt, asker].concat()
}

fn reply_mac(
    key: &SymmetricKey,
    salt: &Salt,
    device: &[u8; 32],
    addrs: &[SocketAddr],
) -> Option<[u8; MAC_LEN]> {
    let addrs = postcard::to_stdvec(addrs).ok()?;
    Some(keyed(key, &[REPLY_CONTEXT, salt, device, &addrs]))
}

/// Seal the answer to `asker`'s probe with `salt`: this device and the
/// addresses its endpoint can be dialled at (the first
/// [`MAX_REPLY_ADDRS`] of them).
///
/// # Errors
///
/// When `asker` is not a usable key or sealing fails.
pub fn seal_reply(
    keys: &mut LinkKeys,
    asker: [u8; 32],
    salt: &Salt,
    addrs: &[SocketAddr],
) -> Result<Vec<u8>, tilo_seal::Error> {
    let addrs: Vec<SocketAddr> = addrs.iter().copied().take(MAX_REPLY_ADDRS).collect();
    let device = *keys.local();
    let mac = keys
        .pair(asker)
        .and_then(|key| reply_mac(key, salt, &device, &addrs))
        .ok_or(tilo_seal::Error::InvalidKey)?;
    let mut body = postcard::to_stdvec(&ReplyBody {
        version: REPLY_VERSION,
        device,
        addrs,
        mac,
    })
    .map_err(|_| tilo_seal::Error::Seal)?;
    if body.len() > REPLY_BODY_LEN {
        return Err(tilo_seal::Error::Seal);
    }
    body.resize(REPLY_BODY_LEN, 0);
    let recipient = sealing_public(asker).ok_or(tilo_seal::Error::InvalidKey)?;
    let sealed = tilo_seal::hpke::seal(
        &recipient,
        Label::LanDiscoveryReply,
        &reply_aad(salt, &asker),
        &body,
    )?;
    Ok([&REPLY_MAGIC[..], salt, &sealed.to_bytes()].concat())
}

/// The salt a reply datagram names, so the asker can check it sent that
/// probe before opening it.
#[must_use]
pub fn reply_salt(bytes: &[u8]) -> Option<Salt> {
    if bytes.len() != DATAGRAM_LEN {
        return None;
    }
    let rest = bytes.strip_prefix(&REPLY_MAGIC)?;
    rest.split_first_chunk::<SALT_LEN>().map(|(salt, _)| *salt)
}

/// Open a reply to this device's probe with `salt`: the answering device,
/// if it is one of `trusted`, and the addresses it named.
#[must_use]
pub fn open_reply(
    keys: &mut LinkKeys,
    bytes: &[u8],
    salt: &Salt,
    trusted: &BTreeSet<[u8; 32]>,
) -> Option<([u8; 32], Vec<SocketAddr>)> {
    if reply_salt(bytes)? != *salt {
        return None;
    }
    let sealed = bytes.get(REPLY_HEADER_LEN..)?;
    let local = *keys.local();
    let plaintext = tilo_seal::hpke::open_bytes(
        &keys.secret,
        Label::LanDiscoveryReply,
        &reply_aad(salt, &local),
        sealed,
    )
    .ok()?;
    let (body, padding) = postcard::take_from_bytes::<ReplyBody>(&plaintext).ok()?;
    if body.version != REPLY_VERSION
        || body.addrs.len() > MAX_REPLY_ADDRS
        || padding.iter().any(|byte| *byte != 0)
        || !trusted.contains(&body.device)
    {
        return None;
    }
    let expected = keys
        .pair(body.device)
        .and_then(|key| reply_mac(key, salt, &body.device, &body.addrs))?;
    // Both sides of the comparison are this device's to know; a timing
    // difference tells an attacker nothing it could not learn by trying.
    (expected == body.mac).then_some((body.device, body.addrs))
}

/// Whether the kernel considers the clock synchronised to a time source.
/// Without that, a device cannot tell a stale probe from a fresh one.
#[must_use]
pub fn clock_is_synchronized() -> bool {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `timex` is plain data; modes = 0 only reads the state.
        let mut timex: libc::timex = unsafe { std::mem::zeroed() };
        // SAFETY: adjtimex reads into the struct we own.
        let state = unsafe { libc::adjtimex(&raw mut timex) };
        state >= 0 && state != libc::TIME_ERROR
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    fn set(keys: &[&ed25519_dalek::SigningKey]) -> BTreeSet<[u8; 32]> {
        keys.iter()
            .map(|key| key.verifying_key().to_bytes())
            .collect()
    }

    fn id(key: &ed25519_dalek::SigningKey) -> [u8; 32] {
        key.verifying_key().to_bytes()
    }

    /// A linked device recognises the probe and learns who asked; a device
    /// the probe was not made for, even one of the same person, and a
    /// stranger who knows every public key recognise nothing.
    #[test]
    fn only_the_intended_recipients_recognise_a_probe() {
        let (laptop, board, phone, stranger) = (device(1), device(2), device(3), device(4));
        let mut laptop_keys = LinkKeys::new(&laptop);
        let probe = Probe::build(&mut laptop_keys, &set(&[&board]), 7).unwrap();
        let probe = Probe::decode(&probe.encode()).unwrap();

        let mut board_keys = LinkKeys::new(&board);
        assert_eq!(
            probe.sender(&mut board_keys, &set(&[&laptop, &phone])),
            Some(id(&laptop))
        );
        let mut phone_keys = LinkKeys::new(&phone);
        assert_eq!(probe.sender(&mut phone_keys, &set(&[&laptop])), None);
        let mut stranger_keys = LinkKeys::new(&stranger);
        assert_eq!(
            probe.sender(&mut stranger_keys, &set(&[&laptop, &board, &phone])),
            None
        );
        // A device does not take its own probe for another's.
        let own = Probe::build(&mut board_keys, &set(&[&laptop]), 7).unwrap();
        assert_eq!(own.sender(&mut board_keys, &set(&[&laptop])), None);
    }

    /// Every probe has a fresh salt, so none of its slots appears in the
    /// next; the two directions of a pair never share a value; probes and
    /// replies have one size whatever the fleet or the device's addresses.
    #[test]
    fn datagrams_are_unlinkable_and_of_one_size() {
        let (laptop, board, phone) = (device(1), device(2), device(3));
        let mut laptop_keys = LinkKeys::new(&laptop);
        let mut board_keys = LinkKeys::new(&board);
        let first = Probe::build(&mut laptop_keys, &set(&[&board, &phone]), 7).unwrap();
        let second = Probe::build(&mut laptop_keys, &set(&[&board, &phone]), 7).unwrap();
        assert_ne!(first.salt, second.salt);
        assert!(first.slots.iter().all(|slot| !second.slots.contains(slot)));

        let key = laptop_keys.pair(id(&board)).unwrap().clone();
        assert_ne!(
            slot(&key, 7, &first.salt, &id(&laptop)),
            slot(&key, 7, &first.salt, &id(&board))
        );

        let none = Probe::build(&mut laptop_keys, &BTreeSet::new(), 7).unwrap();
        assert_eq!(none.encode().len(), DATAGRAM_LEN);
        assert_eq!(first.encode().len(), DATAGRAM_LEN);
        assert!(first.slots.is_sorted());
        let many: Vec<SocketAddr> = (0..20)
            .map(|n| format!("[fd00::{n}]:4433").parse().unwrap())
            .collect();
        for addrs in [&[][..], &many[..1], &many[..]] {
            let reply = seal_reply(&mut board_keys, id(&laptop), first.salt(), addrs).unwrap();
            assert_eq!(reply.len(), DATAGRAM_LEN);
        }
    }

    /// A device with a synchronised clock refuses probes from other
    /// epochs; every device refuses a probe older than one it already
    /// accepted from the same device, across restarts; a device without a
    /// time source sends what it learned, not its own clock.
    #[test]
    fn freshness_refuses_stale_and_replayed_probes() {
        let (laptop, board) = (device(1), device(2));
        let asker = id(&laptop);
        let mut freshness = Freshness::default();
        assert_eq!(freshness.outgoing(None), 0);
        assert_eq!(freshness.outgoing(Some(500)), 500);
        for (epoch, synced, fresh) in [
            (99, Some(100), true),
            (101, Some(100), true),
            (102, Some(100), false),
            (50, Some(100), false),
            (7, None, true),
        ] {
            assert_eq!(freshness.accepts(&asker, epoch, synced), fresh, "{epoch}");
        }
        assert!(freshness.record(&asker, 100));
        assert!(!freshness.record(&asker, 90));
        assert_eq!(freshness.outgoing(None), 100);
        let restored = Freshness::decode(&freshness.encode());
        for (epoch, fresh) in [(98, false), (99, true), (100, true), (4000, true)] {
            assert_eq!(restored.accepts(&asker, epoch, None), fresh, "{epoch}");
        }
        // Another device's floor is its own.
        assert!(restored.accepts(&id(&board), 5, None));
        assert!(Freshness::decode(b"not json").learned.is_none());

        // The epoch is bound into the slot: rewriting it breaks recognition.
        let mut laptop_keys = LinkKeys::new(&laptop);
        let mut board_keys = LinkKeys::new(&board);
        let probe = Probe::build(&mut laptop_keys, &set(&[&board]), 100).unwrap();
        let mut bytes = probe.encode();
        bytes[8] ^= 1;
        let moved = Probe::decode(&bytes).unwrap();
        assert_eq!(moved.sender(&mut board_keys, &set(&[&laptop])), None);
    }

    /// Once a device is no longer trusted it gets no slot and its probes and
    /// replies are refused, although it can still compute the old pair key.
    #[test]
    fn a_revoked_device_neither_recognises_nor_is_recognised() {
        let (laptop, board, revoked) = (device(1), device(2), device(3));
        let mut laptop_keys = LinkKeys::new(&laptop);
        let mut revoked_keys = LinkKeys::new(&revoked);
        let before = Probe::build(&mut laptop_keys, &set(&[&board, &revoked]), 7).unwrap();
        assert!(before.sender(&mut revoked_keys, &set(&[&laptop])).is_some());

        let trusted = set(&[&board]);
        laptop_keys.retain(&trusted);
        let after = Probe::build(&mut laptop_keys, &trusted, 7).unwrap();
        assert_eq!(after.sender(&mut revoked_keys, &set(&[&laptop])), None);
        let from_revoked = Probe::build(&mut revoked_keys, &set(&[&laptop]), 7).unwrap();
        assert_eq!(from_revoked.sender(&mut laptop_keys, &trusted), None);
        let reply = seal_reply(&mut revoked_keys, id(&laptop), after.salt(), &[]).unwrap();
        assert_eq!(
            open_reply(&mut laptop_keys, &reply, after.salt(), &trusted),
            None
        );
    }

    /// More linked devices than slots: every device gets its turn.
    #[test]
    fn large_fleets_take_turns_in_the_slots() {
        let laptop = device(1);
        let others: Vec<_> = (10..30).map(device).collect();
        let mut laptop_keys = LinkKeys::new(&laptop);
        let all = set(&others.iter().collect::<Vec<_>>());
        let mut seen = BTreeSet::new();
        for _ in 0..40 {
            let probe = Probe::build(&mut laptop_keys, &all, 7).unwrap();
            for other in &others {
                let mut keys = LinkKeys::new(other);
                if probe.sender(&mut keys, &set(&[&laptop])).is_some() {
                    seen.insert(id(other));
                }
            }
        }
        assert_eq!(seen, all);
    }

    /// A reply opens only for the asker, only for the probe it answers, and
    /// only if the device that sealed it is the trusted device it names.
    #[test]
    fn replies_are_bound_to_the_asker_the_probe_and_the_answering_device() {
        let (laptop, board, phone) = (device(1), device(2), device(3));
        let mut laptop_keys = LinkKeys::new(&laptop);
        let mut board_keys = LinkKeys::new(&board);
        let trusted = set(&[&board, &phone]);
        let probe = Probe::build(&mut laptop_keys, &trusted, 7).unwrap();
        let addrs: Vec<SocketAddr> = vec!["10.0.0.2:4433".parse().unwrap()];
        let reply = seal_reply(&mut board_keys, id(&laptop), probe.salt(), &addrs).unwrap();
        assert_eq!(reply_salt(&reply), Some(*probe.salt()));
        assert_eq!(
            open_reply(&mut laptop_keys, &reply, probe.salt(), &trusted),
            Some((id(&board), addrs))
        );
        // Another probe's salt.
        let other = Probe::build(&mut laptop_keys, &trusted, 7).unwrap();
        assert_eq!(
            open_reply(&mut laptop_keys, &reply, other.salt(), &trusted),
            None
        );
        // Sealed to someone else.
        let mut phone_keys = LinkKeys::new(&phone);
        assert_eq!(
            open_reply(&mut phone_keys, &reply, probe.salt(), &set(&[&board])),
            None
        );
        // A stranger who knows the laptop's and the board's public keys
        // claims to be the board, to redirect the laptop's dial.
        let mut body = postcard::to_stdvec(&ReplyBody {
            version: REPLY_VERSION,
            device: id(&board),
            addrs: vec!["10.0.0.66:4433".parse().unwrap()],
            mac: [0; MAC_LEN],
        })
        .unwrap();
        body.resize(REPLY_BODY_LEN, 0);
        let forged = tilo_seal::hpke::seal(
            &sealing_public(id(&laptop)).unwrap(),
            Label::LanDiscoveryReply,
            &reply_aad(probe.salt(), &id(&laptop)),
            &body,
        )
        .unwrap();
        let forged = [&REPLY_MAGIC[..], probe.salt(), &forged.to_bytes()].concat();
        assert_eq!(
            open_reply(&mut laptop_keys, &forged, probe.salt(), &trusted),
            None
        );
    }

    #[test]
    fn malformed_datagrams_are_refused() {
        let mut keys = LinkKeys::new(&device(1));
        let good = Probe::build(&mut keys, &BTreeSet::new(), 7)
            .unwrap()
            .encode();
        assert!(Probe::decode(&good).is_some());
        assert!(Probe::decode(&good[..good.len() - 1]).is_none());
        assert!(Probe::decode(&[good.as_slice(), &[0]].concat()).is_none());
        let mut other_magic = good.clone();
        other_magic[7] = b'2';
        assert!(Probe::decode(&other_magic).is_none());
        let mut dirty_padding = good.clone();
        if let Some(last) = dirty_padding.last_mut() {
            *last = 1;
        }
        assert!(Probe::decode(&dirty_padding).is_none());
        assert!(reply_salt(&good).is_none());
        assert!(reply_salt(&[0; DATAGRAM_LEN + 1]).is_none());
    }
}
