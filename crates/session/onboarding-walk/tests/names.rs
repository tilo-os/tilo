//! Every control the walk presses is one the session's onboarding names:
//! a name it never registers is never listed, and a walk waiting for it
//! runs out its timeout.

/// A checkbox is named by its row (`with_id`); the first argument of
/// `Checkbox::new` names nothing.
#[test]
fn the_walk_presses_the_sessions_own_controls() {
    // The view and the page modules beside it.
    let shell = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../shell/src");
    let mut view = std::fs::read_to_string(shell.join("onboarding_view.rs")).unwrap();
    for entry in std::fs::read_dir(shell.join("onboarding_view")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            view.push_str(&std::fs::read_to_string(path).unwrap());
        }
    }
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
