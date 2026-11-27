//! What a tilolang action returns reaches its invoker over the app's connection: the value its
//! declaration carries as data (a note as the reference to the note it made, a note's Markdown as
//! the text it wrote), and a refusal with neither.

use super::*;

/// The address the running app publishes `action` on, which the window shows: the notebook its
/// `new_note` runs on, the app's own resource for the read every app publishes.
fn published_from(
    host: &TilolangProcess,
    action: &str,
    on: impl Fn(&str) -> bool,
) -> tilo_actions::ActionAddress {
    let hosted = host.instances().next().expect("the app runs");
    hosted
        .published()
        .iter()
        .find(|published| published.address.action == action && on(&published.address.resource))
        .unwrap_or_else(|| {
            panic!(
                "{action} is published where the window shows it: {:?}",
                hosted
                    .published()
                    .iter()
                    .map(|published| (&published.address, &published.action_label))
                    .collect::<Vec<_>>()
            )
        })
        .address
        .clone()
}

/// The address an invocation of `action` on the note `note` names: a note's actions are its
/// own, whatever the window shows.
fn on_note(note: &str, action: &str) -> tilo_actions::ActionAddress {
    tilo_actions::ActionAddress {
        resource: note.to_owned(),
        action: action.to_owned(),
    }
}

/// Deliver `address` to the app as `actor`, with `arguments` when it takes any, and give the
/// update it sent back.
fn delivered(
    host: &mut TilolangProcess,
    window: &mut Window,
    sent: &Sent,
    inbox: &Inbox,
    id: u64,
    address: &tilo_actions::ActionAddress,
    arguments: Option<serde_json::Value>,
    actor: &str,
) -> tilo_actions::InvocationUpdate {
    inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id,
        invocation: tilo_actions::InvocationId::new(),
        resource: address.resource.clone(),
        action: address.action.clone(),
        argument: arguments
            .map(|arguments| format!("{}{arguments}", tilo_actions::ARGUMENTS_JSON_PREFIX)),
        actor: tilo_actions::MemberId::person(actor).into(),
        context: None,
    });
    settle(host, window);
    answered(sent, id).unwrap_or_else(|| panic!("the app ends the invocation {id}"))
}

/// The output of a succeeded invocation.
fn succeeded(update: &tilo_actions::InvocationUpdate) -> &tilo_actions::ActionOutput {
    match update {
        tilo_actions::InvocationUpdate::Succeeded { output, .. } => output,
        other => panic!("the invocation succeeded: {other:?}"),
    }
}

/// A note made through Notes' own `new_note`, as the invoker hears it: the published id of the
/// note it made.
fn a_note(
    host: &mut TilolangProcess,
    window: &mut Window,
    sent: &Sent,
    inbox: &Inbox,
    id: u64,
    title: &str,
    body: &str,
) -> String {
    let new_note = published_from(host, "new_note", |resource| {
        resource.starts_with(&format!("tl:{NOTES}:"))
    });
    let update = delivered(
        host,
        window,
        sent,
        inbox,
        id,
        &new_note,
        Some(serde_json::json!({"title": title, "body": body})),
        "ada",
    );
    succeeded(&update)
        .resources
        .first()
        .unwrap_or_else(|| panic!("the output names the note it made: {update:?}"))
        .clone()
}

tilo_test_env::scenario! {
    fn a_notes_action_returns_what_it_made_and_what_it_wrote;

    phase a_note_is_returned_as_the_typed_reference_to_it;
    // An action that returns a resource returns the reference to the one it made, as the API
    // reads one: the note's app, type and id, and the published id it names beside it — typed
    // data, not a line a person reads.
    {
        let (mut host, mut window, sent, inbox, _route) = running_app("ada", NOTES, notes());
        settle(&mut host, &mut window);
        let new_note = published_from(&host, "new_note", |resource| {
            resource.starts_with(&format!("tl:{NOTES}:"))
        });
        let update = delivered(
            &mut host,
            &mut window,
            &sent,
            &inbox,
            1,
            &new_note,
            Some(serde_json::json!({"title": "Take the ferry"})),
            "ada",
        );
        let output = succeeded(&update);
        let made = output
            .resources
            .first()
            .unwrap_or_else(|| panic!("the output names the note it made: {update:?}"))
            .clone();
        assert!(made.starts_with(&format!("tl:{NOTES}:")), "{made}");
        let value = output
            .value
            .as_ref()
            .unwrap_or_else(|| panic!("a resource value is the reference to it: {update:?}"));
        assert_eq!(value["app"], NOTES, "{value}");
        assert_eq!(value["kind"], format!("tl.{NOTES}.Note"), "{value}");
        assert_eq!(
            format!("tl:{NOTES}:{}", value["id"].as_str().expect("an id")),
            made,
            "the reference names the note the output made: {value}"
        );
    }

    phase a_note_written_through_its_action_reads_back_as_its_markdown;
    // The issue's own round trip: a note written with a body, then read back through Notes'
    // `export_markdown`, which returns the note as the Markdown it holds.
    {
        let (mut host, mut window, sent, inbox, _route) = running_app("ada", NOTES, notes());
        settle(&mut host, &mut window);
        let body = "## Plan\n\n## Found so far\n\n## Still open\n";
        let note = a_note(&mut host, &mut window, &sent, &inbox, 1, "The ferry", body);
        let update = delivered(
            &mut host,
            &mut window,
            &sent,
            &inbox,
            2,
            &on_note(&note, "export_markdown"),
            None,
            "ada",
        );
        let output = succeeded(&update);
        let markdown = output
            .value
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("the note comes back as its Markdown: {update:?}"));
        assert!(markdown.contains("# The ferry"), "{markdown}");
        assert!(
            markdown.contains("## Plan") && markdown.contains("## Still open"),
            "the body written is the body read back: {markdown}"
        );
        assert!(output.resources.is_empty(), "{output:?}");
    }

    // A person the workspace refuses is refused where the call is admitted, on the device,
    // before it reaches an instance: `tilo-app-host`'s
    // `a_held_record_is_admitted_by_its_workspace_whatever_its_verdict_said`.
    phase a_value_that_says_nothing_does_not_inherit_the_last_one;
    // Each invocation ends with its own plan, and one that returns nothing carries no
    // value — never the note the previous invocation made.
    {
        let (mut host, mut window, sent, inbox, _route) = running_app("ada", NOTES, notes());
        settle(&mut host, &mut window);
        let note = a_note(&mut host, &mut window, &sent, &inbox, 1, "The ferry", "");
        let update = delivered(
            &mut host,
            &mut window,
            &sent,
            &inbox,
            2,
            &on_note(&note, "keep_image"),
            Some(serde_json::json!({"picture": ["fox.png", "abc123"]})),
            "ada",
        );
        let output = succeeded(&update);
        assert_eq!(
            output.value, None,
            "an action that returns nothing carries no value: {update:?}"
        );
    }

    phase a_cancellation_of_a_finished_invocation_changes_nothing;
    // An invocation runs to its end in the frame it arrives in and ends there, so a
    // cancellation that follows it changes nothing: the caller's one output is the value the
    // action gave, never a cancellation beside it.
    {
        let (mut host, mut window, sent, inbox, _route) = running_app("ada", NOTES, notes());
        settle(&mut host, &mut window);
        let new_note = published_from(&host, "new_note", |resource| {
            resource.starts_with(&format!("tl:{NOTES}:"))
        });
        let update = delivered(&mut host, &mut window, &sent, &inbox, 1, &new_note, None, "ada");
        assert!(
            !succeeded(&update).resources.is_empty(),
            "the invocation returned the note it made"
        );
        inbox
            .borrow_mut()
            .push_back(CollabEvent::InvocationCancellation { id: 1 });
        settle(&mut host, &mut window);
        assert!(
            !sent.borrow().iter().any(|request| matches!(
                request,
                CollabRequest::UpdateInvocation { id: 1, update } if !update.is_terminal()
                    || matches!(update, tilo_actions::InvocationUpdate::Cancelled { .. })
            )),
            "the finished invocation is not ended again"
        );
    }
}
