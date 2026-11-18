//! What a caller is told at the start of its work: what its own listing would answer it, every
//! action by its name, no app it may not open, and the same words every time.
use super::*;

/// The actions `line` names, each as a path ends with it, without its `#`.
fn named_on(line: &str) -> Vec<&str> {
    line.split('#')
        .skip(1)
        .filter_map(|rest| rest.split(['(', ',', ' ']).next())
        .collect()
}

#[tokio::test]
async fn a_caller_is_told_at_the_start_what_its_own_listing_would_answer() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    let told = server.told_at_start().await;
    // The app in use, with an action by its name; the installed one, by name.
    assert!(
        told.contains("\nnotes") && told.contains("#new-page"),
        "{told}"
    );
    assert!(told.contains("\nsheets"), "{told}");
    // The same words every time: nothing in them changes from one ask to the next.
    assert_eq!(told, server.told_at_start().await);
    // Every action told of the app is one its listing answers the same caller.
    let whole = json!({"path": "notes", "limit": 200});
    let (listed, failed) = tool(&server, "tilo_ls", whole).await;
    assert!(!failed, "{listed}");
    let of_notes = told
        .lines()
        .find(|line| line.starts_with("notes"))
        .expect("the app in use has its line");
    let actions = named_on(of_notes);
    assert!(!actions.is_empty(), "{of_notes}");
    for action in actions {
        assert!(
            listed.contains(&format!("#{action}")),
            "{action} is told and not listed: {listed}"
        );
    }
}

/// No count of bytes cuts what is told: every action the caller's own listing of the app names
/// is told to it by its name, on the app's line or among what is said once.
#[tokio::test]
async fn every_action_the_listing_names_is_told_by_its_name() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    let told = server.told_at_start().await;
    let whole = json!({"path": "notes", "kind": "actions", "limit": 200});
    let (listed, failed) = tool(&server, "tilo_ls", whole).await;
    assert!(!failed, "{listed}");
    let mut actions = 0usize;
    for line in listed.lines().filter(|line| line.starts_with('#')) {
        for action in named_on(line).into_iter().take(1) {
            actions = actions.saturating_add(1);
            let written = format!("#{action}");
            assert!(
                told.split([',', '\n', ' ']).any(|word| word == written),
                "{action} is listed and not told:\n{told}"
            );
        }
    }
    assert!(actions > 0, "{listed}");
    // An action is told by its name alone: what it takes is its own listing's.
    assert!(!told.contains("#new-page("), "{told}");
}

/// What a turn is told names only what its caller may use: an app the workspace names to the
/// agent without letting it open it is not told to it at all.
#[tokio::test]
async fn an_app_the_caller_may_not_open_is_not_told_to_it() {
    let notes = Notes::new(true).installed(Installed::SheetsClosedToAgent);
    let told = agent(&notes).told_at_start().await;
    assert!(
        told.contains("\nnotes") && !told.to_lowercase().contains("sheets"),
        "{told}"
    );
}
