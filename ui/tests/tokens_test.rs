use quick_ui::tokens::{Tokens, ColorRamp, Spacing, Radii};

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
