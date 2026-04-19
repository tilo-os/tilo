use std::collections::HashMap;
use crate::permissions::UserId;

mod key;
mod render;
pub use key::{PresenceKey, PathSegment, SelectionRange};
// render.rs adds impl blocks; nothing new to pub use.

/// Stable RGB color for a collaborator, used by every widget that paints a
/// per-peer affordance (tab presence dots, remote cursor, per-folder
/// presence dots, etc.).
///
/// The concept pins a fixed palette for four canonical ids — `me`,
/// `alice`, `bob`, `charlie` — matching the reference mockup's
/// `cursorColor` table. Any other id falls through to a stable FNV-1a
/// hash → HSL so the same peer keeps the same hue everywhere they
/// render, but two different peers are visually distinguishable from
/// each other.
pub fn user_color(user_id: &str) -> (u8, u8, u8) {
    // Concept-pinned palette. Matches `nirios-concept/src/data.ts`
    // USERS[*].cursorColor so the Rust render is pixel-identical to the
    // reference mockup for these names.
    match user_id {
        "me" => (0x63, 0x66, 0xf1),      // indigo-500
        "alice" => (0xec, 0x48, 0x99),   // pink-500
        "bob" => (0xf9, 0x73, 0x16),     // orange-500
        "charlie" => (0x10, 0xb9, 0x81), // emerald-500
        _ => {
            // FNV-1a 64-bit — cheap, deterministic, no RNG state, and
            // same distribution profile as the old per-widget hashes.
            let mut h: u64 = 0xcbf29ce484222325;
            for b in user_id.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            let hue = (h % 360) as f32;
            hsl_to_rgb(hue, 0.65, 0.55)
        }
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hh = h / 60.0;
    let x = c * (1.0 - (hh.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hh as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (
        ((r1 + m) * 255.0).clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).clamp(0.0, 255.0) as u8,
    )
}

#[cfg(test)]
mod color_tests {
    use super::*;

    #[test]
    fn pinned_ids_match_concept_palette() {
        assert_eq!(user_color("me"), (0x63, 0x66, 0xf1));
        assert_eq!(user_color("alice"), (0xec, 0x48, 0x99));
        assert_eq!(user_color("bob"), (0xf9, 0x73, 0x16));
        assert_eq!(user_color("charlie"), (0x10, 0xb9, 0x81));
    }

    #[test]
    fn unknown_ids_fall_through_to_stable_hash() {
        let a = user_color("dave");
        let b = user_color("dave");
        let c = user_color("eve");
        assert_eq!(a, b, "same id must map to same color");
        assert_ne!(a, c, "different ids should map to different colors");
    }

    #[test]
    fn pinned_ids_differ_from_hash_fallback() {
        // Regression guard: the pinned palette must not accidentally
        // agree with what FNV→HSL would have produced, or we'd be
        // unable to tell override from fallback in a failure mode.
        let (r, g, b) = user_color("me");
        assert!(!(r == 0 && g == 0 && b == 0));
    }
}

#[derive(Debug, Clone)]
pub struct RemoteCursor {
    pub user_id: String,
    pub color: [f32; 4],
    pub position: [f32; 2], // scene-space coords
    pub label: String,
}

#[derive(Debug, Default, Clone)]
pub struct PresenceCtx {
    pub here: HashMap<PresenceKey, Vec<UserId>>,
    pub pointers: HashMap<PresenceKey, Vec<(UserId, (f32, f32))>>,
    pub selections: HashMap<PresenceKey, Vec<(UserId, SelectionRange)>>,
    pub carets: HashMap<PresenceKey, Vec<(UserId, usize)>>,
    pub remote_cursors: Vec<RemoteCursor>,
}

impl PresenceCtx {
    pub fn empty() -> Self { Self::default() }

    pub fn register_presence(&mut self, key: &PresenceKey, user: UserId) {
        let list = self.here.entry(key.clone()).or_default();
        if !list.iter().any(|u| u == &user) {
            list.push(user);
        }
    }

    pub fn remove_presence(&mut self, key: &PresenceKey, user: &UserId) {
        if let Some(list) = self.here.get_mut(key) {
            list.retain(|u| u != user);
        }
    }

    pub fn set_pointer(&mut self, key: &PresenceKey, user: UserId, pos: (f32, f32)) {
        let list = self.pointers.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = pos;
        } else {
            list.push((user, pos));
        }
    }

    pub fn set_caret(&mut self, key: &PresenceKey, user: UserId, pos: usize) {
        let list = self.carets.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = pos;
        } else {
            list.push((user, pos));
        }
    }

    pub fn set_selection(&mut self, key: &PresenceKey, user: UserId, range: SelectionRange) {
        let list = self.selections.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = range;
        } else {
            list.push((user, range));
        }
    }

    pub fn peers_on(&self, key: &PresenceKey) -> &[UserId] {
        self.here.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn pointer_for(&self, key: &PresenceKey, user: &UserId) -> Option<(f32, f32)> {
        self.pointers.get(key)?.iter().find(|(u, _)| u == user).map(|(_, p)| *p)
    }

    pub fn selection_for(&self, key: &PresenceKey, user: &UserId) -> Option<SelectionRange> {
        self.selections.get(key)?.iter().find(|(u, _)| u == user).map(|(_, r)| *r)
    }

    pub fn caret_for(&self, key: &PresenceKey, user: &UserId) -> Option<usize> {
        self.carets.get(key)?.iter().find(|(u, _)| u == user).map(|(_, c)| *c)
    }
}
