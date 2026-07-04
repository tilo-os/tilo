use ui::tokens::Tokens;

/// Apply nirios-specific overrides on top of the base token set (more dramatic motion,
/// heavier elevation for shell chrome). v1 overrides spring stiffness for the sidebar
/// wheel + overview scale transitions.
pub fn nirios_dark() -> Tokens {
    let mut t = Tokens::dark();
    t.motion.springs.snappy.stiffness = 320.0;
    t.motion.springs.bouncy.damping = 10.0;
    t.elevation.e3.blur = 18.0;
    t.elevation.e4.blur = 36.0;
    t
}
