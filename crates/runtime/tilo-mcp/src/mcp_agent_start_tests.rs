//! What a caller is told at the start of its work: what its own listing would answer it, no
//! app it may not open, the same words every time, and never more than its bound.
use super::*;

/// The most a turn is told, as the server that tells it bounds it.
const MOST: usize = 6_000;

#[tokio::test]
async fn a_caller_is_told_at_the_start_what_its_own_listing_would_answer() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    let told = server.told_at_start(MOST).await;
    // The app in use, with an action as a call writes it; the installed one, by name.
    assert!(
        told.contains("\nnotes") && told.contains("#new-page("),
        "{told}"
    );
    assert!(told.contains("sheets"), "{told}");
    // The same words every time: what is told stands where a provider's cache keeps it.
    assert_eq!(told, server.told_at_start(MOST).await);
    // Every action told of the app is one its listing answers the same caller.
    let whole = json!({"path": "notes", "limit": 200});
    let (listed, failed) = tool(&server, "tilo_ls", whole).await;
    assert!(!failed, "{listed}");
    let of_notes = told
        .lines()
        .find(|line| line.starts_with("notes"))
        .expect("the app in use has its line");
    let mut actions = 0usize;
    for action in of_notes
        .split('#')
        .skip(1)
        .filter_map(|rest| rest.split(['(', ',', ' ']).next())
    {
        actions = actions.saturating_add(1);
        assert!(
            listed.contains(&format!("#{action}")),
            "{action} is told and not listed: {listed}"
        );
    }
    assert!(actions > 0, "{of_notes}");
}

/// What a turn is told names only what its caller may use: an app the workspace names to the
/// agent without letting it open it is not told to it at all.
#[tokio::test]
async fn an_app_the_caller_may_not_open_is_not_told_to_it() {
    let notes = Notes::new(true).installed(Installed::SheetsClosedToAgent);
    let told = agent(&notes).told_at_start(MOST).await;
    assert!(
        told.contains("\nnotes") && !told.to_lowercase().contains("sheets"),
        "{told}"
    );
}

#[tokio::test]
async fn what_is_told_keeps_within_its_bound() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    let whole = server.told_at_start(MOST).await;
    assert!(!whole.is_empty() && whole.len() <= MOST, "{}", whole.len());
    for most in [0, 40, 200, 400, whole.len()] {
        let told = server.told_at_start(most).await;
        assert!(told.len() <= most, "{most}: {} bytes: {told}", told.len());
    }
    assert_eq!(server.told_at_start(whole.len()).await, whole);
}
