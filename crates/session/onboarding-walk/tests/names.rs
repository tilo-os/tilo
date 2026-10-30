//! Every control the walk presses is one the session's onboarding names:
//! a name it never registers is never listed, and a walk waiting for it
//! runs out its timeout.

/// The setup app draws onboarding's controls. A checkbox is named by the
/// acknowledgement it is built as (`acknowledgement(id, ..)` names its row
/// with `with_id`); any other control by its id.
#[test]
fn the_walk_presses_the_sessions_own_controls() {
    let app = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../onboarding/src");
    let mut view = String::new();
    for entry in std::fs::read_dir(app).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            view.push_str(&std::fs::read_to_string(path).unwrap());
        }
    }
    let acknowledgements: Vec<&str> = view
        .match_indices("acknowledgement(")
        .filter_map(|(at, _)| view.get(at..))
        .map(|rest| rest.split(')').next().unwrap_or(rest))
        .collect();
    for press in onboarding_walk::PRESSES {
        let quoted = format!("\"{}\"", press.name);
        let named = if press.checkbox {
            acknowledgements.iter().any(|call| call.contains(&quoted))
        } else {
            view.contains(&quoted)
        };
        assert!(named, "the onboarding app names no control {}", press.name);
    }
}
