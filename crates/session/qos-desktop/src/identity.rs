use std::sync::atomic::{AtomicU16, AtomicU64, Ordering as AtomicOrdering};

/// Process-wide monotonic counter behind [`WorkspaceId::new`],
/// [`StripId::new`] and `next_id()`.
/// [`WorkspaceTree::seed_next_id_from_self`] advances it past the highest
/// persisted id at load, or a fresh process restarts at `1` and collides
/// with an already-loaded entry. Only the low 48 bits come from here; the
/// high 16 carry [`NEXT_ID_PEER_SALT`] so two peers minting in parallel
/// never collide — otherwise `apply_changes` folds two distinct
/// workspaces with the same id into one.
pub static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// High-16-bit peer salt for every id [`next_id`] mints. Default `0`
/// keeps tests on `0x0001, 0x0002, …`; the hosted entry point overwrites
/// it at startup with a hash of the local Iroh key. `Relaxed`: written
/// once at boot, nothing piggy-backs on visibility.
pub static NEXT_ID_PEER_SALT: AtomicU16 = AtomicU16::new(0);

/// Mask that selects the low-48-bit counter portion of a salted id.
pub const PEER_ID_COUNTER_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Shift amount that lifts the 16-bit peer salt into the high bits.
pub const PEER_ID_SALT_SHIFT: u32 = 48;

pub fn next_id() -> u64 {
    let counter = NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed);
    let counter = counter & PEER_ID_COUNTER_MASK;
    let salt = u64::from(NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed));
    (salt << PEER_ID_SALT_SHIFT) | counter
}

/// Extract the 16-bit peer salt encoded in the high bits of a salted id.
pub fn peer_salt_of(id: u64) -> u16 {
    (id >> PEER_ID_SALT_SHIFT) as u16
}

/// Extract the low-48-bit counter component of a salted id.
pub fn counter_of(id: u64) -> u64 {
    id & PEER_ID_COUNTER_MASK
}

/// Mint in an explicitly owned namespace without process-global state. The
/// resulting canonical IDs must be persisted before publication.
pub fn mint_in_namespace(salt: u16, counter: &mut u64) -> Result<u64, String> {
    if salt == 0 || *counter == 0 || *counter > PEER_ID_COUNTER_MASK {
        return Err("invalid or exhausted desktop identity namespace".into());
    }
    let id = (u64::from(salt) << PEER_ID_SALT_SHIFT) | *counter;
    *counter = counter
        .checked_add(1)
        .ok_or("desktop identity namespace exhausted")?;
    Ok(id)
}
