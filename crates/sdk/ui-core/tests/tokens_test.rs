#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::Theme;
use tilo_ui::tokens::{Radii, Spacing, Tokens};

#[test]
fn tokens_dark_has_11_neutral_stops() {
    let t = Tokens::dark();
    assert_eq!(t.colors.neutral.len(), 11);
    assert_ne!(t.colors.neutral[0], t.colors.neutral[10]);
}

#[test]
fn spacing_scale_is_4px_base() {
    let s = Spacing::default();
    assert_eq!(s.s0, 0.0);
    assert_eq!(s.s1, 4.0);
    assert_eq!(s.s2, 8.0);
    assert_eq!(s.s3, 12.0);
    assert_eq!(s.s4, 16.0);
    assert_eq!(s.s6, 24.0);
    assert_eq!(s.s8, 32.0);
    assert_eq!(s.s12, 48.0);
    assert_eq!(s.s16, 64.0);
}

#[test]
fn radii_has_five_steps_plus_full() {
    let r = Radii::default();
    assert!(r.sm < r.md);
    assert!(r.md < r.lg);
    assert!(r.lg < r.xl);
    assert!(r.xl < r.full);
}

#[test]
fn theme_exposes_tokens() {
    let t = Theme::dark();
    assert_eq!(t.padding_standard, t.tokens.spacing.s3);
    assert_eq!(f64::from(t.tokens.radii.md), t.radius_standard);
}

use tilo_ui::dnd::DragPayload;
use tilo_ui::permissions::{PermissionScope, ResourceRef, Verb};
use tilo_ui::presence::{PresenceCtx, PresenceKey};

#[test]
fn presence_ctx_empty_is_constructible() {
    let _ = PresenceCtx::empty();
    let _k = PresenceKey::Widget { path: vec![] };
}

#[test]
fn permission_scope_constructs() {
    let s = PermissionScope::workspace(Verb::Write);
    assert_eq!(s.op, Verb::Write);
    assert_eq!(s.resource, ResourceRef::Workspace);
}

#[test]
fn drag_payload_tab_variant_exists() {
    let _ = DragPayload::Tab { group: 0, tab: 0 };
}
