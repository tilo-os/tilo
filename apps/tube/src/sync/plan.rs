//! Documented implementation plan for Grayjay "Sync" interop.
//!
//! This module is **documentation as code**: no behaviour, just the bounded,
//! reviewable plan a future implementer follows behind the `grayjay-sync`
//! feature. It exists so the seam in [`super`] is backed by a concrete design
//! rather than a vague TODO, while still leaving the actual Noise work
//! unimplemented (per the brief).
//!
//! # Step plan
//!
//! 1. **Read the source first** (clean-room — note the licensing caution in
//!    [`super`]). Extract the *facts* below from:
//!    - `futo-org/grayjay-android`: `states/StateSync.kt`,
//!      `sync/internal/SyncSocketSession.kt`, `GJSyncOpcodes.kt`
//!    - `Grayjay.Desktop`: `StateSync.cs`
//!    - relay: `JustOptimize/Grayjay-Sync`
//!    Confirm every UNVERIFIED item before writing wire code.
//!
//! 2. **Noise handshake** via the [`snow`](https://docs.rs/snow) crate. Pick the
//!    handshake pattern read from `SyncSocketSession.kt` (UNVERIFIED — likely an
//!    `XX`/`IK`-family pattern with a static keypair per device). Build a
//!    `snow::Builder` with the matching pattern string and the device static
//!    key; drive handshake → transport state.
//!
//! 3. **Framing.** Wrap the Noise transport in Grayjay's length-prefixed frame
//!    format (UNVERIFIED: read `SyncSocketSession.kt` for the header layout —
//!    opcode + length + payload; whether payloads are JSON or a binary
//!    serialization). Implement encode/decode against [`opcodes`].
//!
//! 4. **Pairing.** Implement QR / manual pairing: exchange static public keys
//!    out-of-band, then authenticate the Noise handshake against the stored
//!    [`super::PeerId`]. Android listens on TCP **12315**; also support the
//!    relay path for NAT'd peers.
//!
//! 5. **Packages.** Map [`packages`] ⇄ [`super::SyncState`]. Send/receive
//!    `SyncSubscriptionsPackage` etc.; feed pulled state into Tube's CRDT so the
//!    existing merge / undo machinery resolves conflicts (no bespoke merge).
//!
//! 6. **Relay fallback.** Speak the reference relay protocol so two phones (or a
//!    phone + Tube) behind NATs can still sync via a forwarding server.
//!
//! 7. **Wire it behind the `grayjay-sync` feature**, swapping
//!    [`super::default_transport`] from the stub to the real transport. Keep the
//!    default build on the stub so nothing here touches the hermetic path.

/// Grayjay sync opcodes, **ported from `GJSyncOpcodes.kt` — values UNVERIFIED**.
///
/// The integer values below are placeholders pending a read of the source; the
/// *names* are the package/operation taxonomy to implement. Do not ship these
/// values without confirming them against `GJSyncOpcodes.kt` (and cross-checking
/// `StateSync.cs`).
pub mod opcodes {
    /// Handshake / keep-alive ping. UNVERIFIED value.
    pub const PING: u8 = 0;
    /// Acknowledgement. UNVERIFIED value.
    pub const PONG: u8 = 1;
    /// A data package follows (subscriptions / playlists / history …).
    /// UNVERIFIED value.
    pub const DATA: u8 = 2;
    /// Request the peer send a given package kind. UNVERIFIED value.
    pub const REQUEST: u8 = 3;
}

/// The Grayjay "packages" that carry state, as named in `StateSync.kt`. The
/// payload schema of each is UNVERIFIED — confirm from source before encoding.
pub mod packages {
    /// `SyncSubscriptionsPackage` — the subscribed-channel set. Maps to
    /// [`super::super::SyncState::subscriptions`].
    pub const SUBSCRIPTIONS: &str = "SyncSubscriptionsPackage";
    /// `SyncSubscriptionGroupsPackage` — channel groupings (no Tube equivalent
    /// yet; would extend `SyncState`).
    pub const SUBSCRIPTION_GROUPS: &str = "SyncSubscriptionGroupsPackage";
    /// `SyncPlaylistsPackage` — user playlists (future Tube feature).
    pub const PLAYLISTS: &str = "SyncPlaylistsPackage";
    /// Watch history. Maps to [`super::super::SyncState::watch_history`].
    pub const HISTORY: &str = "SyncHistoryPackage";
}

/// The TCP port Grayjay-Android listens on for direct P2P sync.
pub const ANDROID_LISTEN_PORT: u16 = 12315;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_match_grayjay_taxonomy() {
        // Pin the package names we will speak so a typo is caught in review.
        assert_eq!(packages::SUBSCRIPTIONS, "SyncSubscriptionsPackage");
        assert_eq!(packages::PLAYLISTS, "SyncPlaylistsPackage");
        assert_eq!(ANDROID_LISTEN_PORT, 12315);
    }

    #[test]
    fn opcodes_are_distinct() {
        let all = [opcodes::PING, opcodes::PONG, opcodes::DATA, opcodes::REQUEST];
        let mut seen: Vec<u8> = all.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), all.len(), "opcode placeholders must be distinct");
    }
}
