//! The local peer's identity.
//!
//! Genuine multi-participant chat needs every peer to author under a
//! **distinct, stable** id — otherwise two people typing on two machines both
//! land as `u_me`, their messages render as the same author, and their
//! per-user reactions/read-watermarks collide in the shared CRDT. This module
//! resolves that identity once at startup:
//!
//! 1. The Tilo runtime identity (an inherited signing broker, loaded via
//!    [`runtime_identity`]) — the same identity the collaboration layer uses
//!    with. Its 32-byte group public key gives a stable unique id and its
//!    display name / avatar colour come straight from first-boot setup.
//! 2. A persisted per-install UUID under the chat data dir, so a peer keeps
//!    the same id across restarts even outside a full Tilo session.
//! 3. A deterministic `user@host` fallback, then a random id — so the app is
//!    always usable, never two peers colliding by accident.
//!
//! The ephemeral/demo/`--shots`/test path uses [`LocalIdentity::demo`], which
//! is exactly the seeded demo's local user (`u_me` / "You"), so the demo and
//! the whole existing test-suite keep their stable ids.

use crate::model::{self, Id};

/// The resolved local user: how this peer authors messages, reactions, voice
/// membership, and its own read watermark in the shared document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalIdentity {
    pub id: Id,
    pub name: String,
    pub initials: String,
    pub color_idx: usize,
}

impl LocalIdentity {
    /// The demo/test local user — identical to the seeded demo's `u_me`, so
    /// the ephemeral `--shots` frame and every existing test keep authoring
    /// under the stable demo id.
    pub fn demo() -> Self {
        Self {
            id: model::ME.to_string(),
            name: "You".to_string(),
            initials: "QD".to_string(),
            color_idx: 0,
        }
    }

    /// Resolve the live local identity from the environment. Never fabricates a
    /// shared id: each source below yields something distinct per real peer.
    pub fn resolve() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(id) = Self::from_runtime_identity() {
            return id;
        }
        if let Some(id) = Self::from_persisted_uuid() {
            return id;
        }
        Self::from_user_host()
    }

    /// The Tilo runtime identity — the canonical per-user identity the launcher
    /// provisions and the collab layer signs with.
    #[cfg(not(target_arch = "wasm32"))]
    fn from_runtime_identity() -> Option<Self> {
        let si = runtime_identity::load().ok()?;
        let pk = si.group_public_key();
        // A short, stable, collision-resistant id from the public key. The full
        // 32 bytes is overkill for a map key; the first 8 bytes (64 bits) are
        // ample and keep the id readable in logs / the document.
        let id = format!("u_{}", hex8(&pk));
        let name = {
            let n = si.display_name().trim().to_string();
            if n.is_empty() { user_display() } else { n }
        };
        Some(Self {
            initials: initials_of(&name),
            color_idx: color_from_bytes(&pk),
            id,
            name,
        })
    }

    /// A UUID persisted under the chat data dir, so a peer outside a full Tilo
    /// session (no `TILO_USER_IDENTITY`) still keeps one stable id across runs.
    fn from_persisted_uuid() -> Option<Self> {
        let dir = crate::storage::data_dir()?;
        let path = dir.join("peer-id");
        let id = match std::fs::read_to_string(&path) {
            Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => {
                let fresh = format!("u_{}", uuid::Uuid::new_v4().simple());
                std::fs::create_dir_all(&dir).ok()?;
                std::fs::write(&path, &fresh).ok()?;
                fresh
            }
        };
        let name = user_display();
        Some(Self {
            initials: initials_of(&name),
            color_idx: color_from_bytes(id.as_bytes()),
            id,
            name,
        })
    }

    /// Deterministic last resort: a `user@host` hash, so even with no disk and
    /// no runtime identity two different machines/users still differ.
    fn from_user_host() -> Self {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let host = hostname();
        let seed = format!("{user}@{host}");
        let id = if seed == "@" {
            format!("u_{}", uuid::Uuid::new_v4().simple())
        } else {
            format!("u_{:016x}", fnv1a(seed.as_bytes()))
        };
        let name = user_display();
        Self {
            initials: initials_of(&name),
            color_idx: color_from_bytes(id.as_bytes()),
            id,
            name,
        }
    }
}

/// Lower-hex of the first 8 bytes.
#[cfg(not(target_arch = "wasm32"))]
fn hex8(b: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(16);
    for byte in b.iter().take(8) {
        let _ = write!(s, "{byte:02x}");
    }
    s
}

/// A stable palette index (0..8) from arbitrary bytes.
fn color_from_bytes(b: &[u8]) -> usize {
    (fnv1a(b) % 8) as usize
}

/// [`color_from_bytes`], for a caller outside this module that has taken a
/// new id and has to move the colour with it. A colour derived from an id
/// the peer no longer authors under is a caret that disagrees with the
/// avatar beside it.
pub fn color_index_for(bytes: &[u8]) -> usize {
    color_from_bytes(bytes)
}

/// 64-bit FNV-1a — a tiny, dependency-free stable hash (never used for
/// security; only to spread ids across the colour palette / host fallback).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Present a tidy display name from `$USER`, splitting on common separators so
/// "ada.lovelace" → "Ada Lovelace". Falls back to "You".
fn user_display() -> String {
    let raw = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "You".to_string());
    let display: String = raw
        .split(['.', '_', '-', ' '])
        .filter(|p| !p.is_empty())
        .map(capitalise)
        .collect::<Vec<_>>()
        .join(" ");
    if display.is_empty() {
        "You".to_string()
    } else {
        display
    }
}

fn capitalise(p: &str) -> String {
    let mut ch = p.chars();
    match ch.next() {
        Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
        None => String::new(),
    }
}

/// Up-to-two-letter uppercase initials for an avatar monogram. Delegates the
/// word-split to the shared toolkit helper (so every app derives a monogram
/// the same way) and applies chat's own "Y" fallback for a nameless peer.
fn initials_of(name: &str) -> String {
    let s = ui::widget::Avatar::initials_for_name(name);
    if s.is_empty() { "Y".to_string() } else { s }
}

/// [`initials_of`], for a caller that has just learned the name the room
/// gave this participant and has to move the monogram with it.
pub fn initials_for(name: &str) -> String {
    initials_of(name)
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_identity_is_the_seeded_local_user() {
        let me = LocalIdentity::demo();
        assert_eq!(me.id, model::ME);
        assert_eq!(me.name, "You");
    }

    #[test]
    fn user_host_ids_are_distinct_per_peer_and_stable() {
        // Two different peers (different user@host) must never share an id, and
        // the same peer must resolve the same id twice — that stability + that
        // distinctness is exactly what genuine multi-participant chat needs.
        let a = format!("u_{:016x}", fnv1a(b"alice@apple"));
        let a2 = format!("u_{:016x}", fnv1a(b"alice@apple"));
        let b = format!("u_{:016x}", fnv1a(b"bob@banana"));
        assert_eq!(a, a2, "same peer resolves the same id");
        assert_ne!(a, b, "different peers get different ids");
    }

    #[test]
    fn initials_and_display_are_tidy() {
        assert_eq!(initials_of("Ada Lovelace"), "AL");
        assert_eq!(initials_of("mononym"), "M");
        assert_eq!(capitalise("ada"), "Ada");
    }

    #[test]
    fn hex8_and_color_are_stable() {
        let pk = [0xABu8; 32];
        assert_eq!(hex8(&pk), "abababababababab");
        assert_eq!(color_from_bytes(&pk), 5);
    }
}
