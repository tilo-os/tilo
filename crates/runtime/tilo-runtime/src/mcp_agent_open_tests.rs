//! An app's `open` asked of an app that runs already: what the door makes of the word that
//! starts an app when the app offers actions whose ids begin with it.
use super::*;

/// `notes#open` is what starts Notes, and says what is in it once it runs. The live Notes
/// offers an agent `open-link`, its one action whose id begins with `open`: asked to open on
/// Notes that runs already (another device of the person's opened it, and this one shows its
/// own view), the door runs no action nobody named, least of all one that reaches outside
/// the notebook and so asks the person first. Named in full, or by a start that is not the
/// word that starts an app, `open-link` is still the action run.
#[test]
fn opening_an_app_that_runs_is_never_another_action_its_name_begins() {
    let host = Notes::new(true).following_links();
    run(async {
        let server = agent(&host);
        for path in ["notes#open", "notes#start"] {
            let (text, failed) = tool(&server, "tilo_do", json!({ "path": path })).await;
            assert!(
                !failed && text.starts_with("ok: notes is running\nnotes  (so.tilo.notes)\n"),
                "{path}: {text}"
            );
        }
        assert_eq!(host.invoked(), Vec::new(), "nothing was run");
        // Nor with arguments, which are the open's own and no other action's.
        let (text, failed) = tool(
            &server,
            "tilo_do",
            json!({"path": "notes#open", "args": {"url": "https://example.org"}}),
        )
        .await;
        assert!(
            failed && text.contains("has no action open; its open-link is another action"),
            "{text}"
        );
        assert_eq!(host.invoked(), Vec::new(), "nothing was run");
        for path in ["notes#open-link", "notes#open-l"] {
            let (text, failed) = tool(
                &server,
                "tilo_do",
                json!({"path": path, "args": {"url": "https://example.org"}}),
            )
            .await;
            assert!(!failed, "{path}: {text}");
        }
        let ran: Vec<String> = host
            .invoked()
            .into_iter()
            .map(|(_, action, _)| action)
            .collect();
        assert_eq!(ran, ["open-link"; 2], "the action named is the action run");
    });
}
