//! Who is reading and writing: the participant a view is for.
//!
//! The id is the one the host gives the view (`ViewCx::me`), the same id
//! every action that participant takes is signed with, so authorship,
//! reactions and read watermarks are attributed alike on every replica. What
//! is resolved here is only how to show that participant before the room
//! names them: a display name, a monogram and a colour.
//!
//! The demo used by screenshots and tests reads as [`LocalIdentity::demo`],
//! the seeded demo's own person (`u_me` / "You").

use crate::model::{self, Id};

/// The participant a view is for, as it is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalIdentity {
    pub id: Id,
    pub name: String,
    pub initials: String,
    pub color_idx: usize,
}

impl LocalIdentity {
    /// The demo's own person — identical to the seeded demo's `u_me`.
    pub fn demo() -> Self {
        Self {
            id: model::ME.to_string(),
            name: "You".to_string(),
            initials: "QD".to_string(),
            color_idx: 0,
        }
    }

    /// Participant `id` as this device shows its own person: the display
    /// name first-boot setup gave the runtime identity, else the login name.
    pub fn for_participant(id: &str) -> Self {
        let name = ui::local_identity::bound()
            .map(|identity| identity.display_name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(user_display);
        Self {
            initials: initials_of(&name),
            color_idx: color_from_bytes(id.as_bytes()),
            id: id.to_owned(),
            name,
        }
    }
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
    let s = ui::widget::initials_for_name(name);
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
    fn identity_derivation_is_stable_and_tidy() {
        let me = LocalIdentity::demo();
        assert_eq!(me.id, model::ME, "case demo: id");
        assert_eq!(me.name, "You", "case demo: name");

        let a = LocalIdentity::for_participant("alice");
        assert_eq!(a.id, "alice");
        assert_eq!(a, LocalIdentity::for_participant("alice"), "stable");
        assert_eq!(a.color_idx, color_from_bytes(b"alice"));

        assert_eq!(
            initials_of("Ada Lovelace"),
            "AL",
            "case initials: two words"
        );
        assert_eq!(initials_of("mononym"), "M", "case initials: mononym");
        assert_eq!(color_from_bytes(&[0xABu8; 32]), 5, "case color_from_bytes");
    }
}
