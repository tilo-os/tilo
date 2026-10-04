//! Finding the person's own linked devices on the local network, with no
//! internet and without telling anyone else whose devices they are.
//!
//! # Transport
//!
//! mDNS through `swarm-discovery`, the engine under iroh's mDNS address
//! lookup. Each device answers for `<instance>._tilo-lan-v1._udp.local`. The
//! instance name is random, and so is the record's host name
//! (`<instance>-<port>.local`); both change at every rotation. The records
//! carry the endpoint's private-range addresses, its UDP port and one TXT
//! attribute, `a`, holding an [`Advert`]. Nothing in them names the identity,
//! the person, the device or its endpoint id.
//!
//! # Advert
//!
//! `a = base64url(version || salt || slot[0] || … || slot[7])`, with a
//! 16-byte random `salt` and eight 12-byte slots. For each linked device `P`
//! that this device `A` wants to be found by:
//!
//! ```text
//! K_AP     = tilo_seal::agreement::shared_key(X25519(A), X25519(P), LanDiscovery)
//! slot_A→P = BLAKE3-keyed(K_AP, "tilo-lan-advert-v1\0" || salt || A)[..12]
//! ```
//!
//! where `X25519(·)` is the device's Ed25519 key in its X25519 form (the
//! same conversion the personal fabric uses to seal secrets to a device) and
//! `A` is A's Ed25519 device key. Slots not used by a recipient are random
//! bytes, and the slots are sorted, so an observer learns neither the order
//! nor how many devices the person has. With more than eight linked devices
//! the eight with the lowest slot values are advertised, which is a fresh
//! random choice for every salt.
//!
//! A device `P` recognises an advert by computing `slot_C→P` for every
//! linked candidate `C` it still trusts and looking for it among the slots.
//! A match tells it which device advertised (`C`), so it hands the advert's
//! addresses to the endpoint for `C`'s id and dials.
//!
//! # Why pairwise keys, and why no clock
//!
//! - Only `A` and `P` can compute `K_AP`; nothing is shared fleet-wide, so a
//!   device that once belonged to the fleet learns nothing from adverts that
//!   were not made for it. When `P` is revoked (or leaves the device list),
//!   `A` stops putting a slot for `P` in its adverts and stops accepting
//!   `P`'s, effective at the next advert: P can no longer recognise or track
//!   A. No secret has to be redistributed.
//! - `A→P` and `P→A` slots differ (the sender is hashed in), so two devices
//!   of one person do not advertise a common value an observer could link.
//! - A fresh salt at every rotation makes every slot a new pseudorandom
//!   value; there is no time window, so devices whose clocks disagree (a
//!   board without a battery-backed clock and no NTP) still find each other.
//!   A replayed advert only points the dialler at an address that cannot
//!   complete the iroh handshake for `C`'s endpoint id.
//!
//! Finding a device is not trusting it: the dial goes through the same path
//! as the enrolled direct-peer list, so the connection authenticates the
//! endpoint key and the device certificate, and revocations are checked,
//! before anything syncs.
//!
//! # What an observer still sees
//!
//! That a Tilo device is present, its LAN addresses and the endpoint's UDP
//! port, which is chosen afresh by each session. Within one session the
//! address and port link successive adverts; across networks and sessions
//! nothing does.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use tilo_seal::{Label, PublicKey, SecretKey, SymmetricKey};
use tokio::sync::{mpsc, watch};

use super::actor::NetCmd;

/// mDNS service name: `_tilo-lan-v1._udp.local`.
pub const SERVICE_NAME: &str = "tilo-lan-v1";
/// The TXT attribute holding the [`Advert`].
const ATTRIBUTE: &str = "a";
const ADVERT_VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const TOKEN_LEN: usize = 12;
/// Slots in every advert, used or not.
pub const SLOTS: usize = 8;
const ENCODED_LEN: usize = 1 + SALT_LEN + SLOTS * TOKEN_LEN;
/// Length of the unpadded base64 text of an encoded advert.
const ENCODED_TEXT_LEN: usize = (ENCODED_LEN * 4).div_ceil(3);
const TOKEN_CONTEXT: &[u8] = b"tilo-lan-advert-v1\0";
/// How long one advert (salt, instance and host name) is used.
pub const ROTATION: Duration = Duration::from_secs(5 * 60);
/// Up to this much is added to each [`ROTATION`], so devices that started
/// together do not rotate in step.
const ROTATION_JITTER_SECS: u64 = 60;
/// How often the runner looks at its addresses and settings when nothing
/// else wakes it.
const POLL: Duration = Duration::from_secs(2);
/// A device found again at the same addresses is dialled at most this often.
const REDIAL: Duration = Duration::from_secs(20);

/// Who this device is, as far as adverts are concerned, and the pairwise
/// keys it has derived so far.
pub struct PairKeys {
    local: [u8; 32],
    secret: SecretKey,
    peers: HashMap<[u8; 32], Option<SymmetricKey>>,
}

impl PairKeys {
    #[must_use]
    pub fn new(device: &ed25519_dalek::SigningKey) -> Self {
        Self {
            local: device.verifying_key().to_bytes(),
            secret: SecretKey::from_bytes(device.to_scalar_bytes()),
            peers: HashMap::new(),
        }
    }

    /// This device's Ed25519 key.
    #[must_use]
    pub const fn local(&self) -> &[u8; 32] {
        &self.local
    }

    /// The key shared with `peer`, or `None` for a key that is not a valid
    /// Ed25519 point or is a low-order point.
    fn key(&mut self, peer: [u8; 32]) -> Option<&SymmetricKey> {
        let secret = &self.secret;
        self.peers
            .entry(peer)
            .or_insert_with(|| {
                let public = ed25519_dalek::VerifyingKey::from_bytes(&peer).ok()?;
                tilo_seal::agreement::shared_key(
                    secret,
                    &PublicKey::from_bytes(public.to_montgomery().to_bytes()),
                    Label::LanDiscovery,
                )
                .ok()
            })
            .as_ref()
    }

    /// Forget keys of devices no longer in `peers`.
    pub fn retain(&mut self, peers: &BTreeSet<[u8; 32]>) {
        self.peers.retain(|peer, _| peers.contains(peer));
    }
}

fn token(key: &SymmetricKey, salt: &[u8; SALT_LEN], sender: &[u8; 32]) -> [u8; TOKEN_LEN] {
    let mut hasher = blake3::Hasher::new_keyed(key.expose_secret());
    hasher.update(TOKEN_CONTEXT);
    hasher.update(salt);
    hasher.update(sender);
    let mut out = [0_u8; TOKEN_LEN];
    hasher.finalize_xof().fill(&mut out);
    out
}

/// One advertisement: a salt and [`SLOTS`] blinded tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advert {
    salt: [u8; SALT_LEN],
    slots: [[u8; TOKEN_LEN]; SLOTS],
}

impl Advert {
    /// An advert that each of `recipients` (at most [`SLOTS`] of them, see
    /// the module docs) recognises as coming from this device.
    ///
    /// # Errors
    ///
    /// When the operating system supplies no randomness.
    pub fn build(keys: &mut PairKeys, recipients: &BTreeSet<[u8; 32]>) -> tilo_seal::Result<Self> {
        let mut salt = [0_u8; SALT_LEN];
        tilo_seal::fill_random(&mut salt)?;
        let local = *keys.local();
        let mut tokens: Vec<[u8; TOKEN_LEN]> = recipients
            .iter()
            .filter(|peer| **peer != local)
            .filter_map(|peer| keys.key(*peer).map(|key| token(key, &salt, &local)))
            .collect();
        tokens.sort_unstable();
        tokens.truncate(SLOTS);
        while tokens.len() < SLOTS {
            let mut filler = [0_u8; TOKEN_LEN];
            tilo_seal::fill_random(&mut filler)?;
            tokens.push(filler);
        }
        tokens.sort_unstable();
        let mut slots = [[0_u8; TOKEN_LEN]; SLOTS];
        slots.copy_from_slice(&tokens);
        Ok(Self { salt, slots })
    }

    /// The TXT attribute value.
    #[must_use]
    pub fn encode(&self) -> String {
        use base64::Engine as _;
        let mut bytes = Vec::with_capacity(ENCODED_LEN);
        bytes.push(ADVERT_VERSION);
        bytes.extend_from_slice(&self.salt);
        for slot in &self.slots {
            bytes.extend_from_slice(slot);
        }
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Parse a TXT attribute value; `None` for anything but a version-1
    /// advert of exactly the expected length.
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        use base64::Engine as _;
        if text.len() != ENCODED_TEXT_LEN {
            return None;
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text)
            .ok()?;
        let (&version, rest) = bytes.split_first()?;
        if version != ADVERT_VERSION || rest.len() != SALT_LEN + SLOTS * TOKEN_LEN {
            return None;
        }
        let (salt, tokens) = rest.split_at(SALT_LEN);
        let mut slots = [[0_u8; TOKEN_LEN]; SLOTS];
        for (slot, chunk) in slots.iter_mut().zip(tokens.chunks_exact(TOKEN_LEN)) {
            slot.copy_from_slice(chunk);
        }
        Some(Self {
            salt: salt.try_into().ok()?,
            slots,
        })
    }

    /// The candidate that made this advert for this device, if any.
    pub fn sender<'a>(
        &self,
        keys: &mut PairKeys,
        candidates: impl IntoIterator<Item = &'a [u8; 32]>,
    ) -> Option<[u8; 32]> {
        let local = *keys.local();
        candidates.into_iter().copied().find(|candidate| {
            *candidate != local
                && keys
                    .key(*candidate)
                    .map(|key| token(key, &self.salt, candidate))
                    .is_some_and(|expected| self.slots.contains(&expected))
        })
    }
}

/// What the session wants from discovery right now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LanDiscoveryConfig {
    /// The continuity switch allows it.
    pub enabled: bool,
    /// Linked devices of the person this device still trusts.
    pub peers: BTreeSet<[u8; 32]>,
}

/// Whether `ip` may be advertised: private-range IPv4 and unique-local
/// IPv6 only. Global IPv6 addresses can embed the hardware address and
/// link-local IPv6 needs a scope an advert cannot carry.
fn advertisable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

fn local_addrs(endpoint: &iroh::Endpoint) -> BTreeSet<SocketAddr> {
    endpoint
        .addr()
        .ip_addrs()
        .copied()
        .filter(|addr| advertisable(addr.ip()))
        .collect()
}

fn random_label() -> Option<String> {
    let mut bytes = [0_u8; 8];
    tilo_seal::fill_random(&mut bytes).ok()?;
    // A DNS label must not start with a digit for some resolvers; prefix one.
    Some(format!("t{}", hex::encode(bytes)))
}

fn rotation_period() -> Duration {
    let mut jitter = [0_u8; 2];
    let _ = tilo_seal::fill_random(&mut jitter);
    ROTATION.saturating_add(Duration::from_secs(
        u64::from(u16::from_le_bytes(jitter)) % ROTATION_JITTER_SECS,
    ))
}

struct Advertising {
    _guard: swarm_discovery::DropGuard,
    instance: String,
    peers: BTreeSet<[u8; 32]>,
    addrs: BTreeSet<SocketAddr>,
    rotate_at: Instant,
}

fn start(
    keys: &mut PairKeys,
    peers: &BTreeSet<[u8; 32]>,
    addrs: &BTreeSet<SocketAddr>,
    found: &mpsc::Sender<(String, swarm_discovery::Peer)>,
) -> Option<Advertising> {
    let instance = random_label()?;
    let advert = Advert::build(keys, peers).ok()?;
    let mut by_port: BTreeMap<u16, Vec<IpAddr>> = BTreeMap::new();
    for addr in addrs {
        by_port.entry(addr.port()).or_default().push(addr.ip());
    }
    let interfaces = addrs
        .iter()
        .filter_map(|addr| match addr.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let found = found.clone();
    let mut discoverer =
        swarm_discovery::Discoverer::new_interactive(SERVICE_NAME.into(), instance.clone())
            .with_ip_class(swarm_discovery::IpClass::Auto)
            .with_multicast_interfaces_v4(interfaces)
            .with_callback(move |name: &str, peer: &swarm_discovery::Peer| {
                let _ = found.try_send((name.to_owned(), peer.clone()));
            });
    for (port, ips) in by_port {
        discoverer = discoverer.with_addrs(port, ips);
    }
    let discoverer = discoverer
        .with_txt_attributes([(ATTRIBUTE.to_owned(), Some(advert.encode()))])
        .ok()?;
    match discoverer.spawn(&tokio::runtime::Handle::current()) {
        Ok(guard) => Some(Advertising {
            _guard: guard,
            instance,
            peers: peers.clone(),
            addrs: addrs.clone(),
            rotate_at: Instant::now()
                .checked_add(rotation_period())
                .unwrap_or_else(Instant::now),
        }),
        Err(error) => {
            tracing::warn!(%error, "local-network discovery could not start");
            None
        }
    }
}

/// Run discovery on the current tokio runtime until `config` closes:
/// advertise to and recognise the devices `config` names while it is
/// enabled, and dial each one found through `cmd_tx`.
pub fn spawn(
    endpoint: iroh::Endpoint,
    device: &ed25519_dalek::SigningKey,
    cmd_tx: mpsc::Sender<NetCmd>,
) -> watch::Sender<LanDiscoveryConfig> {
    let (config_tx, mut config_rx) = watch::channel(LanDiscoveryConfig::default());
    let mut keys = PairKeys::new(device);
    tokio::spawn(async move {
        let (found_tx, mut found_rx) = mpsc::channel(64);
        let mut advertising: Option<Advertising> = None;
        let mut dialled: HashMap<[u8; 32], (BTreeSet<SocketAddr>, Instant)> = HashMap::new();
        let started = Instant::now();
        loop {
            let config = config_rx.borrow_and_update().clone();
            keys.retain(&config.peers);
            if !config.enabled || config.peers.is_empty() {
                if advertising.take().is_some() {
                    tracing::info!("local-network discovery stopped");
                }
            } else {
                let addrs = local_addrs(&endpoint);
                let stale = advertising.as_ref().is_none_or(|current| {
                    current.peers != config.peers
                        || current.addrs != addrs
                        || Instant::now() >= current.rotate_at
                });
                if stale && !addrs.is_empty() {
                    // Drop the old advert first, so the two never answer together.
                    drop(advertising.take());
                    advertising = start(&mut keys, &config.peers, &addrs, &found_tx);
                    if advertising.is_some() {
                        tracing::debug!(
                            devices = config.peers.len(),
                            addrs = addrs.len(),
                            "local-network discovery advertising"
                        );
                    }
                }
            }
            tokio::select! {
                changed = config_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(POLL) => {}
                Some((name, peer)) = found_rx.recv() => {
                    if !config.enabled
                        || peer.is_expiry()
                        || advertising.as_ref().is_some_and(|current| current.instance == name)
                    {
                        continue;
                    }
                    let Some(advert) = peer
                        .txt_attribute(ATTRIBUTE)
                        .flatten()
                        .and_then(Advert::decode)
                    else {
                        continue;
                    };
                    let Some(sender) = advert.sender(&mut keys, &config.peers) else {
                        continue;
                    };
                    let Ok(id) = iroh::EndpointId::from_bytes(&sender) else {
                        continue;
                    };
                    let addrs: BTreeSet<SocketAddr> = peer
                        .addrs()
                        .iter()
                        .map(|(ip, port)| SocketAddr::new(*ip, *port))
                        .filter(|addr| advertisable(addr.ip()))
                        .collect();
                    if addrs.is_empty() {
                        continue;
                    }
                    let now = Instant::now();
                    if dialled.get(&sender).is_some_and(|(known, at)| {
                        *known == addrs && now.duration_since(*at) < REDIAL
                    }) {
                        continue;
                    }
                    tracing::info!(
                        peer = %id.fmt_short(),
                        addrs = ?addrs,
                        since_start_ms = now.duration_since(started).as_millis(),
                        "found a linked device on the local network"
                    );
                    dialled.insert(sender, (addrs.clone(), now));
                    let addr = iroh::EndpointAddr::from_parts(
                        id,
                        addrs.into_iter().map(iroh::TransportAddr::Ip),
                    );
                    if cmd_tx.send(NetCmd::Connect { addr }).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    config_tx
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

    /// A linked device recognises the advert and learns who sent it; a
    /// device the advert was not made for, even one of the same person,
    /// recognises nothing.
    #[test]
    fn only_the_intended_recipients_recognise_an_advert() {
        let (laptop, board, phone, stranger) = (device(1), device(2), device(3), device(4));
        let mut laptop_keys = PairKeys::new(&laptop);
        let advert = Advert::build(&mut laptop_keys, &set(&[&board])).unwrap();
        let advert = Advert::decode(&advert.encode()).unwrap();

        let mut board_keys = PairKeys::new(&board);
        assert_eq!(
            advert.sender(&mut board_keys, &set(&[&laptop, &phone])),
            Some(laptop.verifying_key().to_bytes())
        );
        // The phone trusts the laptop but was left out of this advert.
        let mut phone_keys = PairKeys::new(&phone);
        assert_eq!(advert.sender(&mut phone_keys, &set(&[&laptop])), None);
        // Someone who knows every public key but holds none of the secrets.
        let mut stranger_keys = PairKeys::new(&stranger);
        assert_eq!(
            advert.sender(&mut stranger_keys, &set(&[&laptop, &board, &phone])),
            None
        );
        // The board does not take its own adverts for the laptop's.
        let own = Advert::build(&mut board_keys, &set(&[&laptop])).unwrap();
        assert_eq!(own.sender(&mut board_keys, &set(&[&laptop])), None);
        assert_eq!(
            own.sender(&mut laptop_keys, &set(&[&board])),
            Some(board.verifying_key().to_bytes())
        );
    }

    /// Every rotation draws a new salt, so no slot of one advert appears in
    /// the next; two devices of one person never advertise a common value;
    /// the advert's size does not depend on how many devices it is for.
    #[test]
    fn adverts_rotate_and_do_not_link_devices_or_reveal_fleet_size() {
        let (laptop, board, phone) = (device(1), device(2), device(3));
        let mut laptop_keys = PairKeys::new(&laptop);
        let mut board_keys = PairKeys::new(&board);
        let first = Advert::build(&mut laptop_keys, &set(&[&board, &phone])).unwrap();
        let second = Advert::build(&mut laptop_keys, &set(&[&board, &phone])).unwrap();
        assert_ne!(first.salt, second.salt);
        for slot in &first.slots {
            assert!(!second.slots.contains(slot));
        }
        // Both directions of one pair, under the same salt, still differ.
        let reply = Advert {
            salt: first.salt,
            ..Advert::build(&mut board_keys, &set(&[&laptop])).unwrap()
        };
        let key = laptop_keys
            .key(board.verifying_key().to_bytes())
            .unwrap()
            .clone();
        let laptop_to_board = token(&key, &first.salt, &laptop.verifying_key().to_bytes());
        let board_to_laptop = token(&key, &reply.salt, &board.verifying_key().to_bytes());
        assert_ne!(laptop_to_board, board_to_laptop);
        assert!(first.slots.contains(&laptop_to_board));

        let one = Advert::build(&mut laptop_keys, &set(&[&board])).unwrap();
        let none = Advert::build(&mut laptop_keys, &BTreeSet::new()).unwrap();
        assert_eq!(one.encode().len(), none.encode().len());
        assert_eq!(one.encode().len(), first.encode().len());
        assert!(one.slots.is_sorted());
    }

    /// Once a device is dropped from the trusted set (revoked, or removed
    /// from the list), it stops getting a slot and its adverts stop being
    /// recognised, even though it can still compute the old pair key.
    #[test]
    fn a_revoked_device_neither_recognises_nor_is_recognised() {
        let (laptop, board, revoked) = (device(1), device(2), device(3));
        let mut laptop_keys = PairKeys::new(&laptop);
        let mut revoked_keys = PairKeys::new(&revoked);
        let before = Advert::build(&mut laptop_keys, &set(&[&board, &revoked])).unwrap();
        assert!(before.sender(&mut revoked_keys, &set(&[&laptop])).is_some());

        let trusted = set(&[&board]);
        laptop_keys.retain(&trusted);
        let after = Advert::build(&mut laptop_keys, &trusted).unwrap();
        assert_eq!(after.sender(&mut revoked_keys, &set(&[&laptop])), None);
        let from_revoked = Advert::build(&mut revoked_keys, &set(&[&laptop])).unwrap();
        assert_eq!(from_revoked.sender(&mut laptop_keys, &trusted), None);
        assert!(
            from_revoked
                .sender(&mut laptop_keys, &set(&[&revoked]))
                .is_some(),
            "the advert itself is valid; only the trusted set excludes it"
        );
    }

    /// More linked devices than slots: each advert carries a fresh selection,
    /// so every device is reachable within a few rotations.
    #[test]
    fn large_fleets_take_turns_in_the_slots() {
        let laptop = device(1);
        let others: Vec<_> = (10..30).map(device).collect();
        let mut laptop_keys = PairKeys::new(&laptop);
        let all = set(&others.iter().collect::<Vec<_>>());
        let mut seen = BTreeSet::new();
        for _ in 0..40 {
            let advert = Advert::build(&mut laptop_keys, &all).unwrap();
            for other in &others {
                let mut keys = PairKeys::new(other);
                if advert.sender(&mut keys, &set(&[&laptop])).is_some() {
                    seen.insert(other.verifying_key().to_bytes());
                }
            }
        }
        assert_eq!(seen, all);
    }

    #[test]
    fn malformed_adverts_are_refused() {
        let mut keys = PairKeys::new(&device(1));
        let good = Advert::build(&mut keys, &BTreeSet::new()).unwrap().encode();
        assert!(Advert::decode(&good).is_some());
        assert!(Advert::decode(&good[..good.len() - 2]).is_none());
        assert!(Advert::decode(&format!("{good}AA")).is_none());
        assert!(Advert::decode("").is_none());
        assert!(Advert::decode("not base64!").is_none());
        let mut other_version = good.into_bytes();
        other_version[0] = b'B';
        assert!(Advert::decode(std::str::from_utf8(&other_version).unwrap()).is_none());
    }

    #[test]
    fn only_private_addresses_are_advertised() {
        for (ip, allowed) in [
            ("192.168.1.4", true),
            ("10.0.0.2", true),
            ("172.20.1.1", true),
            ("169.254.3.3", true),
            ("fd12::1", true),
            ("8.8.8.8", false),
            ("2001:db8::1", false),
            ("fe80::1", false),
            ("127.0.0.1", false),
        ] {
            assert_eq!(advertisable(ip.parse().unwrap()), allowed, "{ip}");
        }
    }
}
