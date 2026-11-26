//! `outline`: an agent reads what a tilolang app's window shows as text, calls the action a line
//! of it names by the address the line gives, and reads the change in the next outline.

use super::*;

/// What `outline` answers of the running app's window, asked as invocation `id`.
fn read(
    host: &mut TilolangProcess,
    settle: &mut dyn FnMut(&mut TilolangProcess),
    sent: &Sent,
    inbox: &Inbox,
    id: u64,
) -> String {
    let outline = published(host, "outline");
    deliver(inbox, id, &outline, "ada");
    settle(host);
    match answered(sent, id).expect("the read answers") {
        tilo_actions::InvocationUpdate::Succeeded { output, .. } => output
            .value
            .and_then(|value| value["outline"].as_str().map(str::to_owned))
            .expect("the read answers lines"),
        other => panic!("the read succeeded: {other:?}"),
    }
}

/// The address the running app publishes `action` at.
fn published(host: &TilolangProcess, action: &str) -> tilo_actions::ActionAddress {
    host.instances()
        .next()
        .expect("the app runs")
        .published()
        .iter()
        .find(|published| published.address.action == action)
        .unwrap_or_else(|| panic!("{action} is published"))
        .address
        .clone()
}

/// The action the line of `lines` that starts `starts` runs, by the address it names:
/// `<app>/<resource>#<action>`.
fn named(lines: &str, app: &str, starts: &str) -> tilo_actions::ActionAddress {
    let line = lines
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(starts))
        .unwrap_or_else(|| panic!("a line reads {starts}: {lines}"));
    let (_, address) = line
        .split_once(" → ")
        .unwrap_or_else(|| panic!("{line} names its action"));
    let (resource, action) = address
        .strip_prefix(&format!("{app}/"))
        .and_then(|rest| rest.split_once('#'))
        .unwrap_or_else(|| panic!("{address} is an address of {app}"));
    tilo_actions::ActionAddress::new(resource, action)
}

/// Counter's window read as text, the action its Increment line names called as the line names
/// it, and the change read back.
fn reads_calls_and_reads_the_change(
    app: &str,
    host: &mut TilolangProcess,
    settle: &mut dyn FnMut(&mut TilolangProcess),
    sent: &Sent,
    inbox: &Inbox,
) -> tilo_actions::ActionAddress {
    let lines = read(host, settle, sent, inbox, 1);
    let shows = lines
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(&format!("window shows {app}/")))
        .unwrap_or_else(|| panic!("the window says the resource it shows: {lines}"));
    assert!(
        lines.contains("text \"0\""),
        "the window reads the count: {lines}"
    );
    // The control names the action it runs on the resource the window shows.
    let named = named(&lines, app, "button \"Increment\"");
    assert_eq!(
        named,
        tilo_actions::ActionAddress::new(shows, "increment"),
        "{lines}"
    );

    deliver(inbox, 2, &named, "ada");
    settle(host);
    assert!(
        matches!(
            answered(sent, 2),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ),
        "the action the line names runs: {:?}",
        answered(sent, 2)
    );
    let lines = read(host, settle, sent, inbox, 3);
    assert!(
        lines.contains("text \"1\""),
        "the next read shows the change: {lines}"
    );
    named
}

tilo_test_env::scenario! {
    fn an_agent_reads_the_window_calls_the_action_it_names_and_reads_the_change;

    phase a_window_the_person_has_open;
    {
        let (app, bundle) = shipped(&shipped_dir().join("counter"));
        let (mut host, mut window, sent, inbox, _route) = running_app("ada", &app, bundle);
        let mut settled = |host: &mut TilolangProcess| settle(host, &mut window);
        settled(&mut host);
        let named = reads_calls_and_reads_the_change(&app, &mut host, &mut settled, &sent, &inbox);
        // What the line names is the action exactly as the app publishes it.
        assert_eq!(named, published(&host, "increment"));
    }

    // An instance an agent's calls run in shows no window: it reads what its window would show.
    phase an_instance_that_shows_nothing;
    {
        let (app, bundle) = shipped(&shipped_dir().join("counter"));
        let (mut host, sent, inbox, _workspace) = running_with_no_view("ada", &app, bundle, None);
        settle_with_no_view(&mut host);
        reads_calls_and_reads_the_change(&app, &mut host, &mut settle_with_no_view, &sent, &inbox);
    }
}
