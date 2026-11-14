//! An app's directory while the app is busy or its view is rebuilt: what the door makes of a
//! page that cannot be read now and of a level that lists less than the app has.
use super::*;

/// An app that cannot read its directory for a moment, as while another actor's action is
/// being applied, is asked again: the caller sees what the app has, not an app with nothing.
#[test]
fn an_app_that_cannot_list_for_a_moment_is_asked_again() {
    let steady = Notes::new(true);
    let busy = Notes::new(true);
    *busy.cannot_list.lock().unwrap() = 3;
    let (expected, failed) = run(tool(&agent(&steady), "tilo_ls", json!({"path": "notes"})));
    assert!(!failed, "{expected}");
    let (listed, failed) = run(tool(&agent(&busy), "tilo_ls", json!({"path": "notes"})));
    assert!(!failed, "{listed}");
    assert_eq!(listed, expected);
    assert_eq!(*busy.cannot_list.lock().unwrap(), 0);
}

/// An app that never can is said to be unable to list once the time an action is waited for
/// has passed: it is not reported as an app without the thing that was named.
#[test]
fn an_app_that_never_can_list_is_not_reported_as_empty() {
    let busy = Notes::new(true);
    *busy.cannot_list.lock().unwrap() = u32::MAX;
    let (text, failed, waited) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(async {
            let started = tokio::time::Instant::now();
            let (text, failed) =
                tool(&agent(&busy), "tilo_read", json!({"path": "notes/Todo"})).await;
            (text, failed, started.elapsed())
        });
    assert!(failed, "{text}");
    assert!(text.contains("cannot list"), "{text}");
    assert!(!text.contains("it has:"), "{text}");
    assert!(waited >= std::time::Duration::from_millis(DEFAULT_WAIT_MS));
}

/// A name missing for an instant, while the app's top level lists nothing though it says the
/// page is whole, is looked for again and found.
#[test]
fn a_name_missing_for_an_instant_is_looked_for_again() {
    let busy = Notes::new(true);
    *busy.lists_less.lock().unwrap() = 2;
    let (read, failed) = run(tool(
        &agent(&busy),
        "tilo_read",
        json!({"path": "notes/Todo"}),
    ));
    assert!(!failed, "{read}");
    assert_eq!(*busy.lists_less.lock().unwrap(), 0);
}

/// A name the app does not have is still said to be missing, with what the app has.
#[test]
fn a_name_the_app_does_not_have_is_still_reported() {
    let host = Notes::new(true);
    let (text, failed) = run(tool(
        &agent(&host),
        "tilo_read",
        json!({"path": "notes/Nope"}),
    ));
    assert!(failed, "{text}");
    assert!(text.contains("it has:"), "{text}");
}
