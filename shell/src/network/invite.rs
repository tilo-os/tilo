//! Peer-invite token format — Phase 3 Task 3.3 of
//! `docs/superpowers/plans/2026-05-02-peer-sync-end-to-end.md`.
//!
//! An invite is a base64-url-encoded JSON blob signed by the inviting
//! owner's libp2p ed25519 keypair. The receiver decodes it, verifies
//! the signature against the embedded public key, checks the public
//! key matches the embedded `PeerId`, and dials the embedded
//! multiaddr to join the workspace.
//!
//! ```text
//! /invite        → ShellNetwork::issue_invite() → token string → clipboard
//! /connect <tok> → validate_invite_token        → ShellNetwork::dial(addr)
//!                                              → JoinWorkspace { ws }
//! ```
//!
//! Token wire format (base64-url, no-pad):
//!
//! ```ignore
//! { "workspace_id": u64,
//!   "room_id": "…-…-…" (uuid),
//!   "owner_peer_id": "12D3KooW…" (base58),
//!   "owner_public_key": <base64-url protobuf-encoded PublicKey>,
//!   "owner_addr": "/ip4/…/tcp/…/p2p/…",
//!   "expiry_unix_seconds": u64,
//!   "signature": <bytes — ed25519 over the canonical payload>
//! }
//! ```
//!
//! We embed `owner_public_key` because libp2p's `PeerId` is a multihash
//! of the protobuf-encoded public key — there's no stable accessor to
//! reverse a `PeerId` back into a `PublicKey` for verification. We
//! still cross-check `owner_peer_id == PeerId::from(public_key)` so a
//! token that ships a key + peer-id mismatch is rejected.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};

use crate::shell::state::{RoomId, Workspace, WorkspaceId, WorkspaceTree};

/// Signed invite issued by a workspace owner so a remote peer can dial
/// in and replicate the tree. Round-trips through
/// [`make_invite_token`] / [`validate_invite_token`].
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct PeerInviteToken {
    /// Gossipsub topic the invite grants access to. The receiver will
    /// issue a `JoinWorkspace { ws: workspace_id }` once the signature
    /// checks out.
    ///
    /// This is the **inviter's local** workspace handle. It names a
    /// transport topic and nothing else; it is not an identity the
    /// receiver can compare against its own workspaces, because the
    /// same number means a different workspace on every machine. That
    /// is what [`Self::room_id`] is for.
    pub workspace_id: u64,
    /// The inviter's durable room identity
    /// (`crate::shell::state::RoomId`), rendered as a uuid string.
    ///
    /// The field that makes an invite an invite *to something*. The
    /// receiver adopts this as its local workspace's room
    /// ([`crate::shell::state::WorkspaceTree::set_room`]), so both ends
    /// address one cluster and the deltas their apps produce actually
    /// reach each other. Without it the invitee joins a topic and then
    /// talks past the inviter forever, in a room of one.
    ///
    /// Signed along with everything else: an attacker who could rewrite
    /// this in transit could redirect an invitee into a room of the
    /// attacker's choosing while the rest of the token still verified.
    pub room_id: String,
    /// Inviter's libp2p `PeerId`, rendered as base58 so it round-
    /// trips through `PeerId::from_str` cleanly. Cross-checked
    /// against `owner_public_key` on validate.
    pub owner_peer_id: String,
    /// Inviter's libp2p `PublicKey`, protobuf-encoded so the receiver
    /// can verify `signature` without having to re-derive the key
    /// from the peer-id. base64-url no-pad encoded.
    pub owner_public_key: String,
    /// Multiaddr the receiver should dial to reach the inviter (e.g.
    /// `/ip4/192.168.1.5/tcp/4001`). Validated by libp2p's `Multiaddr`
    /// parser at `dial` time, not here — this struct stays transport-
    /// agnostic.
    pub owner_addr: String,
    /// Absolute unix timestamp (seconds since epoch) after which the
    /// token is rejected. v1 issues 1-hour expiries; the receiver
    /// rejects expired tokens before signature verification so
    /// expired-but-valid signatures don't leak validity oracles.
    pub expiry_unix_seconds: u64,
    /// ed25519 signature over the canonical payload string built by
    /// [`canonical_payload`]. Verified against `owner_public_key`.
    pub signature: Vec<u8>,
}

/// Build the canonical signing-payload string from a token's fields.
///
/// Fields are joined with `|` in a fixed order: `workspace_id |
/// room_id | owner_peer_id | owner_public_key | owner_addr |
/// expiry_unix_seconds`. Pinned by
/// [`tests::canonical_payload_is_stable`] so a future field addition
/// can't silently break verification on already-issued tokens.
///
/// `room_id` is inside the signature rather than alongside it because
/// it decides which collaboration cluster the invitee ends up in. A
/// token that carried it unsigned would let anyone who can touch the
/// bytes in transit point the invitee at a different room while every
/// other check still passed.
fn canonical_payload(
    workspace_id: u64,
    room_id: &str,
    owner_peer_id: &str,
    owner_public_key: &str,
    owner_addr: &str,
    expiry_unix_seconds: u64,
) -> String {
    format!(
        "{workspace_id}|{room_id}|{owner_peer_id}|{owner_public_key}|{owner_addr}|\
         {expiry_unix_seconds}"
    )
}

/// Wall-clock seconds since the unix epoch, or `0` if the system clock
/// is somehow before 1970.
fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Issue an invite to `workspace`, signed by `keypair`.
///
/// Takes the whole [`Workspace`] rather than a pair of ids so the
/// transport topic and the room identity cannot be sourced from two
/// different workspaces — an invite that named one workspace's topic
/// and another's room would connect the peers and then route their
/// deltas nowhere, which is the hardest kind of failure to see.
///
/// `expiry_seconds_from_now` is added to the current unix time to form
/// the absolute expiry. v1 callers pass `3600` (one hour). The owner's
/// public key is embedded so receivers can verify without round-
/// tripping through the libp2p DHT.
///
/// Returns the base64-url-encoded JSON token, ready to drop on the
/// clipboard. Refuses a workspace whose room has not been minted:
/// there is nothing to invite anyone *into* yet, and a token carrying
/// the nil room would hand the invitee an address that reaches nobody.
pub fn make_invite_token(
    keypair: &libp2p::identity::Keypair,
    workspace: &Workspace,
    owner_addr: &str,
    expiry_seconds_from_now: u64,
) -> std::io::Result<String> {
    if workspace.room.is_nil() {
        return Err(std::io::Error::other(
            "workspace has no room identity to invite into",
        ));
    }
    let workspace_id = workspace.id.raw();
    let room_id = workspace.room.to_string();
    let public_key = keypair.public();
    let owner_peer_id = public_key.to_peer_id().to_base58();
    let owner_public_key = B64.encode(public_key.encode_protobuf());
    let expiry_unix_seconds = now_unix_seconds().saturating_add(expiry_seconds_from_now);

    let payload = canonical_payload(
        workspace_id,
        &room_id,
        &owner_peer_id,
        &owner_public_key,
        owner_addr,
        expiry_unix_seconds,
    );
    let signature = keypair
        .sign(payload.as_bytes())
        .map_err(std::io::Error::other)?;

    let token = PeerInviteToken {
        workspace_id,
        room_id,
        owner_peer_id,
        owner_public_key,
        owner_addr: owner_addr.to_string(),
        expiry_unix_seconds,
        signature,
    };
    let json = serde_json::to_vec(&token).map_err(std::io::Error::other)?;
    Ok(B64.encode(&json))
}

/// Decode and verify an invite token issued by [`make_invite_token`].
///
/// Returns the parsed token on success. Errors are surfaced as `String`
/// so the palette `/connect` UI can render them inline without pulling
/// in a custom error enum.
///
/// Verification order:
/// 1. base64-url decode → JSON parse,
/// 2. expiry is in the future,
/// 3. the room id is a real (non-nil) uuid,
/// 4. embedded public key parses,
/// 5. embedded `owner_peer_id` matches `PeerId::from(owner_public_key)`,
/// 6. signature verifies over [`canonical_payload`].
///
/// Step 5 closes a forge-by-key-substitution attack: without it, an
/// attacker could swap the public key for one they control while
/// keeping the victim's peer-id, then sign with the substituted key
/// and pass step 6.
///
/// Step 3 rejects a token that would put the invitee in the nil room.
/// It is cheap and it is checked before the signature so a validly
/// signed but meaningless invite is refused rather than accepted into
/// a room that addresses nobody.
pub fn validate_invite_token(token_str: &str) -> Result<PeerInviteToken, String> {
    let json = B64
        .decode(token_str)
        .map_err(|e| format!("base64 decode: {e}"))?;
    let token: PeerInviteToken = serde_json::from_slice(&json)
        .map_err(|e| format!("json parse: {e}"))?;

    let now = now_unix_seconds();
    if token.expiry_unix_seconds < now {
        return Err("token expired".into());
    }

    match RoomId::parse(&token.room_id) {
        Some(room) if !room.is_nil() => {}
        Some(_) => return Err("invite names the unminted room".into()),
        None => return Err("invite carries no valid room id".into()),
    }

    let pk_bytes = B64
        .decode(token.owner_public_key.as_bytes())
        .map_err(|e| format!("public-key base64 decode: {e}"))?;
    let public_key = libp2p::identity::PublicKey::try_decode_protobuf(&pk_bytes)
        .map_err(|e| format!("public-key protobuf decode: {e}"))?;

    let derived_peer_id = public_key.to_peer_id().to_base58();
    if derived_peer_id != token.owner_peer_id {
        return Err("peer-id does not match embedded public key".into());
    }

    let payload = canonical_payload(
        token.workspace_id,
        &token.room_id,
        &token.owner_peer_id,
        &token.owner_public_key,
        &token.owner_addr,
        token.expiry_unix_seconds,
    );
    if !public_key.verify(payload.as_bytes(), &token.signature) {
        return Err("signature mismatch".into());
    }

    Ok(token)
}

/// Accept a verified invite into `tree`: point `local` at the room the
/// inviter named.
///
/// The other half of an invite. Validating a token proves who issued it
/// and which room they meant; this is what makes the invitee actually
/// *be* in that room — their local workspace keeps its own id, its own
/// strips and its own capabilities, and swaps only the identity its
/// apps collaborate under. Every app launched into it afterwards
/// announces the inviter's cluster, which is the whole reason a delta
/// crosses the wire at all.
///
/// The change is mirrored into the automerge doc and flushed to disk by
/// [`WorkspaceTree::set_room`], so the invitee is still in the room
/// after a restart — an invite you have to re-accept every boot is not
/// membership.
///
/// Returns `Err` when the token does not verify or names no workspace
/// this tree has. Deliberately takes the *token string*, not a parsed
/// token, so there is no way to reach this having skipped validation.
pub fn accept_invite_into(
    tree: &mut WorkspaceTree,
    local: WorkspaceId,
    token_str: &str,
) -> Result<RoomId, String> {
    let token = validate_invite_token(token_str)?;
    let room = RoomId::parse(&token.room_id)
        .ok_or_else(|| "invite carries no valid room id".to_string())?;
    if tree.room_of(local).is_none() {
        return Err(format!(
            "no local workspace {} to accept the invite into",
            local.raw()
        ));
    }
    tree.set_room(local, room);
    Ok(room)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity::Keypair;

    /// Pin the canonical payload format. A drift here silently invalidates
    /// every already-issued token, so call out the change with a test
    /// update + plan update rather than letting it sneak in.
    #[test]
    fn canonical_payload_is_stable() {
        let p = canonical_payload(99, "PEER", "PK", "/ip4/127.0.0.1/tcp/1234", 1_700_000_000);
        assert_eq!(p, "99|PEER|PK|/ip4/127.0.0.1/tcp/1234|1700000000");
    }

    #[test]
    fn round_trip_token_via_make_and_validate() {
        let kp = Keypair::generate_ed25519();
        let token_str = make_invite_token(&kp, 99, "/ip4/127.0.0.1/tcp/1234", 3600)
            .expect("make");
        let parsed = validate_invite_token(&token_str).expect("valid");
        assert_eq!(parsed.workspace_id, 99);
        assert_eq!(parsed.owner_addr, "/ip4/127.0.0.1/tcp/1234");
        assert_eq!(parsed.owner_peer_id, kp.public().to_peer_id().to_base58());
    }

    #[test]
    fn tampered_workspace_id_fails_signature_check() {
        let kp = Keypair::generate_ed25519();
        let token_str = make_invite_token(&kp, 99, "/ip4/127.0.0.1/tcp/1234", 3600)
            .expect("make");
        let bytes = B64.decode(&token_str).expect("b64");
        let mut token: PeerInviteToken = serde_json::from_slice(&bytes).expect("json");
        token.workspace_id = 100;
        let bad = B64.encode(serde_json::to_vec(&token).expect("re-json"));
        let err = validate_invite_token(&bad).expect_err("must fail");
        assert!(err.contains("signature"), "expected signature error, got {err:?}");
    }

    #[test]
    fn tampered_addr_fails_signature_check() {
        let kp = Keypair::generate_ed25519();
        let token_str = make_invite_token(&kp, 7, "/ip4/127.0.0.1/tcp/1234", 3600)
            .expect("make");
        let bytes = B64.decode(&token_str).expect("b64");
        let mut token: PeerInviteToken = serde_json::from_slice(&bytes).expect("json");
        token.owner_addr = "/ip4/10.0.0.1/tcp/9999".into();
        let bad = B64.encode(serde_json::to_vec(&token).expect("re-json"));
        assert!(validate_invite_token(&bad).is_err());
    }

    #[test]
    fn substituted_public_key_is_rejected() {
        // Forge attempt: keep the legitimate peer-id but swap in an
        // attacker key + sign with it. Step 4 of `validate_invite_token`
        // catches this even though the signature itself would verify.
        let victim = Keypair::generate_ed25519();
        let attacker = Keypair::generate_ed25519();

        let victim_peer_id = victim.public().to_peer_id().to_base58();
        let attacker_pk_b64 = B64.encode(attacker.public().encode_protobuf());
        let expiry = now_unix_seconds() + 3600;
        let payload =
            canonical_payload(1, &victim_peer_id, &attacker_pk_b64, "/ip4/1.2.3.4/tcp/1", expiry);
        let signature = attacker.sign(payload.as_bytes()).unwrap();

        let token = PeerInviteToken {
            workspace_id: 1,
            owner_peer_id: victim_peer_id,
            owner_public_key: attacker_pk_b64,
            owner_addr: "/ip4/1.2.3.4/tcp/1".into(),
            expiry_unix_seconds: expiry,
            signature,
        };
        let token_str = B64.encode(serde_json::to_vec(&token).unwrap());
        let err = validate_invite_token(&token_str).expect_err("forge must fail");
        assert!(err.contains("peer-id"), "expected peer-id mismatch, got {err:?}");
    }

    #[test]
    fn expired_token_is_rejected() {
        // Issue a token whose expiry is "now + 0s". Sleep one second and
        // assert validate_invite_token rejects it. The "0 seconds in the
        // future" expiry latches the current second, so by the time the
        // sleep returns `now` is strictly greater.
        let kp = Keypair::generate_ed25519();
        let token_str = make_invite_token(&kp, 99, "/ip4/127.0.0.1/tcp/1234", 0)
            .expect("make");
        std::thread::sleep(std::time::Duration::from_secs(1));
        let err = validate_invite_token(&token_str).expect_err("must fail");
        assert!(err.contains("expired"), "expected expiry error, got {err:?}");
    }

    #[test]
    fn malformed_base64_rejected() {
        let err = validate_invite_token("!!!not-base64!!!").expect_err("must fail");
        assert!(err.contains("base64"), "expected base64 error, got {err:?}");
    }

    #[test]
    fn malformed_json_rejected() {
        // base64-decodes fine but isn't valid JSON.
        let bad = B64.encode(b"not json");
        let err = validate_invite_token(&bad).expect_err("must fail");
        assert!(err.contains("json"), "expected json error, got {err:?}");
    }
}
