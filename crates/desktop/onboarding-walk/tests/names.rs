//! Every control the walk presses is one setup's app names: a name it never
//! shows is never listed, and a walk waiting for it runs out its timeout.

/// Setup's app draws onboarding's controls, each named by the English words
/// its view gives it.
#[test]
fn the_walk_presses_the_setup_apps_own_controls() {
    let app = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/onboarding/app.tl"),
    )
    .unwrap();
    let names = onboarding_walk::PRESSES
        .iter()
        .map(|press| press.name)
        .chain(onboarding_walk::SETUP_ONLY);
    for name in names {
        let quoted = format!("\"{name}\"");
        assert!(app.contains(&quoted), "setup's app names no control {name}");
    }
}
