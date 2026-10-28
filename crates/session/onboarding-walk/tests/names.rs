//! Every control the walk presses is one the session's onboarding names:
//! a name it never registers is never listed, and a walk waiting for it
//! runs out its timeout.

/// A checkbox is named by its row (`with_id`); the first argument of
/// `Checkbox::new` names nothing.
#[test]
fn the_walk_presses_the_sessions_own_controls() {
    let view = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../shell/src/onboarding_view.rs"),
    )
    .unwrap();
    for press in onboarding_walk::PRESSES {
        let name = press.name;
        let named = if press.checkbox {
            view.contains(&format!(".with_id(\"{name}\")"))
        } else {
            view.contains(&format!("\"{name}\""))
        };
        assert!(named, "the onboarding view names no control {name}");
    }
}
