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

/// The package whose window states when its log was read, as a row does: a `ListRow` whose
/// trailing fact is the stored moment.
const META_ROW: &str = r#"app so.tilo.lang.meta-row "Meta row" (icon: .hash, category: .utility)

/// Something read at a moment.
type Log {
    key: Text merge register
    /// When it was read.
    at: Time = Time("2026-06-30T10:41:00Z")
}

root Log

default view log for Log

/// The window: when the log was read, as the row's trailing fact says.
view log(l: Log) {
    std.page(l) {
        section("Fetched") {
            ui.ListRow(title: "Read", meta: l.at)
        }
    }
}
"#;

/// The locale a session says on its host: `formats` and the zone `zone` names.
fn locale(formats: tilo_formats::RegionFormats, zone: &str) -> tilolang_process::Locale {
    tilolang_process::Locale {
        formats,
        language: String::new(),
        zone: Some(zone.to_owned()),
    }
}

tilo_test_env::scenario! {
    fn a_rows_moment_reads_on_the_persons_clock_in_the_zone_the_session_names;

    phase the_zone_the_person_chose;
    {
        // A system without a zone database reads every zone as UTC, and says nothing here.
        if tilo_local_time::Zone::named("Pacific/Auckland").is_none() {
            return;
        }
        let app = "so.tilo.lang.meta-row";
        let compiled = compile(META_ROW, &tilolang::IdsLock::new(), &[])
            .unwrap_or_else(|diagnostics| panic!("the package compiles: {diagnostics:?}"));
        let (mut host, sent, inbox, _workspace) =
            running_with_no_view("ada", app, Bundle::new(&compiled, []), None);
        let h24 = tilo_formats::RegionFormats {
            time: tilo_formats::TimeStyle::H24,
            ..tilo_formats::RegionFormats::default()
        };
        // Ten forty-one UTC is the evening in Auckland and midday in Vienna: the row reads the
        // same moment on the clock of the zone their session names, never the device's.
        let _ = host.set_locale(&locale(h24, "Pacific/Auckland"));
        settle_with_no_view(&mut host);
        let lines = read(&mut host, &mut settle_with_no_view, &sent, &inbox, 1);
        assert!(lines.contains("list_row \"Read\""), "the row reads: {lines}");
        assert!(
            lines.contains("22:41"),
            "the moment is theirs, in their zone's evening: {lines}"
        );
        assert!(!lines.contains("10:41"), "never the device's UTC: {lines}");

        // They choose another zone, and the running instance reads it with no restart.
        let _ = host.set_locale(&locale(h24, "Europe/Vienna"));
        settle_with_no_view(&mut host);
        let lines = read(&mut host, &mut settle_with_no_view, &sent, &inbox, 2);
        assert!(
            lines.contains("12:41"),
            "the moment reads in the zone they chose: {lines}"
        );
        assert!(!lines.contains("22:41"), "not the one before it: {lines}");
    }
}
