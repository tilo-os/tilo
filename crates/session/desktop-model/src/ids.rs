//! Identifiers of the desktop model and the salted id space they share.
//!
//! Workspace, strip, window-group and tab ids are all `u64`s from one space:
//! the low 48 bits are a per-minter counter, the high 16 a peer salt, so two
//! peers minting in parallel never collide once their changes merge. Minting
//! is a value ([`IdMinter`]) the host owns; this crate holds no global state.

use serde::{Deserialize, Serialize};

/// Local workspace identifier, newtype-wrapped over a salted `u64`
/// minted by an [`IdMinter`].
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct WorkspaceId(pub u64);

impl WorkspaceId {
    pub fn raw(&self) -> u64 {
        self.0
    }

    /// The id of the workspace that is `room`, the same on every device: the
    /// room's transport shard, which Activity, the relay and custody already
    /// derive from it (`tilo_workspace_join::room_workspace_id`, pinned to
    /// the same value by the shell's tests). A new workspace takes it; one
    /// stored before keeps the id it has.
    #[must_use]
    pub fn of_room(room: RoomId) -> Self {
        // Encoded on the stack, as the transport derivation does.
        let mut canonical = [0u8; 36];
        let hash = blake3::derive_key(
            "tilo-room-transport-v1",
            room.as_uuid()
                .hyphenated()
                .encode_lower(&mut canonical)
                .as_bytes(),
        );
        let mut first8 = [0u8; 8];
        first8.copy_from_slice(&hash[..8]);
        let id = u64::from_le_bytes(first8);
        Self(if id == 0 { 1 } else { id })
    }
}

/// Durable, globally-unique identity of a workspace as a collaboration
/// room. Unlike [`WorkspaceId`] (a local counter) it is minted once as a
/// random v4 uuid and carried rather than recomputed, so machines that
/// have never met still agree on it. [`Self::nil`] is the unminted
/// sentinel.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct RoomId(uuid::Uuid);

impl RoomId {
    /// Mint a fresh room identity, once per workspace at creation.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    /// The "no identity yet" sentinel. Distinguishable from every
    /// minted id because `Uuid::new_v4` never produces the nil uuid.
    pub const fn nil() -> Self {
        Self(uuid::Uuid::nil())
    }

    /// Whether this is the unminted sentinel.
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }

    /// The underlying uuid, for the wire formats that speak `Uuid`.
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }

    /// Parse a room id off the wire (invite token, automerge doc,
    /// `TILO_WORKSPACE_ID`). `None` on anything that isn't a valid uuid.
    pub fn parse(s: &str) -> Option<Self> {
        uuid::Uuid::parse_str(s).ok().map(Self)
    }
}

impl std::fmt::Display for RoomId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<uuid::Uuid> for RoomId {
    fn from(v: uuid::Uuid) -> Self {
        Self(v)
    }
}

impl From<u64> for WorkspaceId {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<WorkspaceId> for u64 {
    fn from(v: WorkspaceId) -> Self {
        v.0
    }
}

/// Local strip identifier, newtype-wrapped like [`WorkspaceId`].
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct StripId(pub u64);

impl StripId {
    pub fn raw(&self) -> u64 {
        self.0
    }
}

impl From<u64> for StripId {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<StripId> for u64 {
    fn from(v: StripId) -> Self {
        v.0
    }
}

pub type GroupId = u64;
pub type TabId = u64;
pub type FileId = u64;

/// Snapshot of the focused tab, cloned out of the desktop tree so callers
/// can release whatever guards it before using the values.
#[derive(Clone, Debug)]
pub struct TabRef {
    pub id: TabId,
    pub title: String,
}

/// Mask that selects the low-48-bit counter portion of a salted id.
pub const PEER_ID_COUNTER_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Shift amount that lifts the 16-bit peer salt into the high bits.
pub const PEER_ID_SALT_SHIFT: u32 = 48;

/// Compose a salted id from its peer salt and counter. Counter bits above
/// [`PEER_ID_COUNTER_MASK`] are discarded.
pub const fn compose_id(salt: u16, counter: u64) -> u64 {
    ((salt as u64) << PEER_ID_SALT_SHIFT) | (counter & PEER_ID_COUNTER_MASK)
}

/// Extract the 16-bit peer salt encoded in the high bits of a salted id.
pub const fn peer_salt_of(id: u64) -> u16 {
    (id >> PEER_ID_SALT_SHIFT) as u16
}

/// The namespace no device mints in. It once held the ids every member of
/// a room derived for a shared strip; documents and state files may still
/// carry such ids, so no device's own ids may ever fall among them. A device
/// whose key would hash to this salt (or to `0`, which has no id space)
/// takes another; see [`device_salt`].
pub const RESERVED_SALT: u16 = 0xFFFF;

/// The salt a device with the 16-bit key fold `folded` mints under: the fold
/// itself, except the two values no device may own (`0` has no id space,
/// [`RESERVED_SALT`] is reserved).
pub const fn device_salt(folded: u16) -> u16 {
    match folded {
        0 => 1,
        RESERVED_SALT => RESERVED_SALT - 1,
        salt => salt,
    }
}

/// Extract the low-48-bit counter component of a salted id.
pub const fn counter_of(id: u64) -> u64 {
    id & PEER_ID_COUNTER_MASK
}

/// The id namespace a room's own strips are minted in: the named places its
/// operator lists. Derived from the room alone, so every host of the room
/// mints them alike, and never a device's own namespace (a device whose key
/// happens to fold here is refused its own, by the host admitting its
/// writes).
#[must_use]
pub fn room_strips_salt(room: &str) -> u16 {
    let hash = blake3::hash(format!("room strips {room}").as_bytes());
    let [a, b, ..] = *hash.as_bytes();
    device_salt(u16::from_le_bytes([a, b]))
}

/// The layout id namespace of the device a name stands for (its device key
/// in hex, or any stable device name): the fold every host derives the same
/// way, so a page's windows, the relay that admits them and a native peer
/// that reads them all spell one device's namespace alike. FNV-1a folded to
/// 16 bits and passed through [`device_salt`], so it is never `0` and never
/// [`RESERVED_SALT`].
#[must_use]
pub fn namespace_of_device(device: &str) -> u16 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in device.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let folded = (hash >> 16) ^ (hash & 0xffff);
    device_salt(u16::try_from(folded).unwrap_or(u16::MAX))
}

/// Something that hands out fresh ids from the salted id space. Operations
/// that create windows, groups or strips take one of these rather than
/// reaching for a global, so a host decides whether ids come from a
/// per-tree [`IdMinter`] or a process-wide counter.
pub trait MintIds {
    /// A fresh id, distinct from every id this source handed out before.
    fn mint_id(&mut self) -> u64;
}

/// A salted id allocator: `salt` fills the high 16 bits, `counter` is the
/// next low-48-bit value to hand out. A plain value — the host owns where
/// it lives and how it is shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdMinter {
    salt: u16,
    counter: u64,
}

impl IdMinter {
    /// A minter for `salt` whose first id has counter `1`, so ids under
    /// salt `0` read `1, 2, 3, …`.
    pub const fn new(salt: u16) -> Self {
        Self { salt, counter: 1 }
    }

    pub const fn resume(salt: u16, next_counter: u64) -> Self {
        Self {
            salt,
            counter: next_counter,
        }
    }

    pub const fn salt(&self) -> u16 {
        self.salt
    }

    /// The counter the next minted id will carry.
    pub const fn next_counter(&self) -> u64 {
        self.counter
    }

    /// Hand out the next id and advance the counter.
    pub fn mint(&mut self) -> u64 {
        let id = compose_id(self.salt, self.counter);
        self.counter = self.counter.wrapping_add(1);
        id
    }

    /// Advance past `id` if it was minted under this minter's salt, so a
    /// reloaded tree never re-mints an id it already holds. Ids under
    /// another salt belong to another peer and leave the counter alone.
    /// [`Self::mint`] for a durable owner, which must never reuse or wrap
    /// an id: refuses salt `0` (no namespace), the [`RESERVED_SALT`] namespace
    /// nobody mints in, a counter of `0`, and a counter beyond the 48 bits
    /// an id carries. The owner persists
    /// [`Self::next_counter`] before publishing what it minted.
    pub fn mint_in_namespace(&mut self) -> Result<u64, String> {
        if self.salt == 0
            || self.salt == RESERVED_SALT
            || self.counter == 0
            || self.counter > PEER_ID_COUNTER_MASK
        {
            return Err("invalid or exhausted desktop identity namespace".into());
        }
        let id = compose_id(self.salt, self.counter);
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or("desktop identity namespace exhausted")?;
        Ok(id)
    }

    pub fn observe(&mut self, id: u64) {
        if peer_salt_of(id) == self.salt {
            self.counter = self.counter.max(counter_of(id).saturating_add(1));
        }
    }
}

impl Default for IdMinter {
    fn default() -> Self {
        Self::new(0)
    }
}

impl MintIds for IdMinter {
    fn mint_id(&mut self) -> u64 {
        self.mint()
    }
}

/// [`IdMinter::mint_in_namespace`] for an owner that keeps its counter as a
/// plain value (read from and written back to its room document): mints
/// under `salt` from `*counter` and advances it.
///
/// # Errors
///
/// A description when the namespace is invalid or exhausted; `*counter` is
/// then unchanged.
pub fn mint_in_namespace(salt: u16, counter: &mut u64) -> Result<u64, String> {
    let mut minter = IdMinter::resume(salt, *counter);
    let id = minter.mint_in_namespace()?;
    *counter = minter.next_counter();
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rooms_workspace_id_is_its_transport_shard() {
        // The same literal `tilo_workspace_join::room_workspace_id` is pinned
        // to: every device and the relay name the room's workspace alike.
        let room = RoomId::parse("0b7e7dfa-3ad5-4b43-9d6c-5f0d7c1f6a11").unwrap();
        assert_eq!(WorkspaceId::of_room(room).raw(), 3_029_697_355_180_468_570);
        let other = RoomId::parse("0b7e7dfa-3ad5-4b43-9d6c-5f0d7c1f6a12").unwrap();
        assert_ne!(WorkspaceId::of_room(other), WorkspaceId::of_room(room));
    }

    #[test]
    fn id_minter_salts_counts_and_observes_within_its_bits() {
        {
            let mut minter = IdMinter::new(0x1234);
            let a = minter.mint();
            let b = minter.mint();
            assert_eq!(peer_salt_of(a), 0x1234);
            assert_eq!(counter_of(a), 1);
            assert_eq!(counter_of(b), 2);
        }
        {
            let mut minter = IdMinter::default();
            assert_eq!(minter.mint(), 1);
            assert_eq!(minter.mint(), 2);
        }
        {
            let mut minter = IdMinter::new(0x1111);
            minter.observe(compose_id(0x1111, 5));
            minter.observe(compose_id(0x2222, 1000));
            assert_eq!(minter.next_counter(), 6);
            minter.observe(compose_id(0x1111, 2));
            assert_eq!(minter.next_counter(), 6, "observing never moves backwards");
        }
        {
            let id = compose_id(0xFFFF, u64::MAX);
            assert_eq!(peer_salt_of(id), 0xFFFF);
            assert_eq!(counter_of(id), PEER_ID_COUNTER_MASK);
        }
    }

    #[test]
    fn a_durable_namespace_refuses_to_wrap_or_run_unsalted() {
        let mut minter = IdMinter::resume(0x0a, PEER_ID_COUNTER_MASK);
        assert_eq!(
            minter.mint_in_namespace(),
            Ok(compose_id(0x0a, PEER_ID_COUNTER_MASK))
        );
        assert!(minter.mint_in_namespace().is_err());
        assert!(IdMinter::new(0).mint_in_namespace().is_err());
        assert!(IdMinter::resume(0x0a, 0).mint_in_namespace().is_err());
    }
}
