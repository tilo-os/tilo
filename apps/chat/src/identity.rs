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
    fn from_runtime_identity() -> Option<Self> {
        let si = ui::local_identity::bound()?;
        let pk = si.group_public_key;
        // A short, stable, collision-resistant id from the public key. The full
        // 32 bytes is overkill for a map key; the first 8 bytes (64 bits) are
        // ample and keep the id readable in logs / the document.
        let id = format!("u_{}", hex8(&pk));
        let name = {
            let n = si.display_name.trim().to_string();
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
        let (dir, name) = tilo_files::file_at(&dir.join("peer-id")).ok()?;
        let stored = tilo_files::blocking::read(&dir, &name)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
        let id = match stored {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => {
                let fresh = format!("u_{}", uuid::Uuid::new_v4().simple());
                tilo_files::blocking::write_atomic(&dir, &name, fresh.as_bytes()).ok()?;
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
        let user = tilo_paths::session::user_name().unwrap_or_default();
        let host = tilo_paths::session::host_name().unwrap_or_default();
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
fn hex8(b: &[u8]) -> String {
    hex::encode(b.get(..8).unwrap_or(b))
}

/// A stable palette index (0..8) from arbitrary bytes.
fn color_from_bytes(b: &[u8]) -> usize {
    (fnv1a(b) % 8) as usize
}

/// `color_from_bytes`, for a caller outside this module that has taken a
/// new id and has to move the colour with it. A colour derived from an id
/// the peer no longer authors under is a caret that disagrees with the
/// avatar beside it.
pub fn color_index_for(bytes: &[u8]) -> usize {
    color_from_bytes(bytes)
}

/// 64-bit FNV-1a — a stable hash (never used for
/// security; only to spread ids across the colour palette / host fallback).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, bytes);
    std::hash::Hasher::finish(&hasher)
}

/// Present a tidy display name from the login name, splitting on common
/// separators so "ada.lovelace" → "Ada Lovelace". Falls back to "You".
fn user_display() -> String {
    let raw = tilo_paths::session::user_name().unwrap_or_else(|| "You".to_string());
    let display: String = raw
        .split(['.', '_', '-', ' '])
        .filter(|p| !p.is_empty())
        .map(ui::format::capitalize_first)
        .collect::<Vec<_>>()
        .join(" ");
    if display.is_empty() {
        "You".to_string()
    } else {
        display
    }
}

/// Up-to-two-letter uppercase initials for an avatar monogram. Delegates the
/// word-split to the shared toolkit helper (so every app derives a monogram
/// the same way) and applies chat's own "Y" fallback for a nameless peer.
fn initials_of(name: &str) -> String {
    let s = ui::widget::Avatar::initials_for_name(name);
    if s.is_empty() { "Y".to_string() } else { s }
}

/// `initials_of`, for a caller that has just learned the name the room
/// gave this participant and has to move the monogram with it.
pub fn initials_for(name: &str) -> String {
    initials_of(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_derivation_is_stable_distinct_and_tidy() {
        // Demo identity is the seeded local user.
        let me = LocalIdentity::demo();
        assert_eq!(me.id, model::ME, "case demo: id");
        assert_eq!(me.name, "You", "case demo: name");

        // Two different peers (different user@host) must never share an id, and
        // the same peer must resolve the same id twice — that stability + that
        // distinctness is exactly what genuine multi-participant chat needs.
        let a = format!("u_{:016x}", fnv1a(b"alice@apple"));
        let a2 = format!("u_{:016x}", fnv1a(b"alice@apple"));
        let b = format!("u_{:016x}", fnv1a(b"bob@banana"));
        assert_eq!(a, a2, "same peer resolves the same id");
        assert_ne!(a, b, "different peers get different ids");

        // Initials and display are tidy.
        assert_eq!(
            initials_of("Ada Lovelace"),
            "AL",
            "case initials: two words"
        );
        assert_eq!(initials_of("mononym"), "M", "case initials: mononym");
        assert_eq!(
            ui::format::capitalize_first("ada"),
            "Ada",
            "case capitalize_first"
        );

        // hex8 and colour are stable.
        let pk = [0xABu8; 32];
        assert_eq!(hex8(&pk), "abababababababab", "case hex8");
        assert_eq!(color_from_bytes(&pk), 5, "case color_from_bytes");
    }
}
