//! A program that waits on the times the workspace's calendars hold: a moment
//! before each entry of a category, carried out with every device off and a
//! clock the test sets. The calendar is kept as a pull of its server left
//! it, and changed as the next pull would after another client changed it.

use std::sync::Arc;

use tilo_automation::resource_time::{CALENDAR_START, PERSON_CALENDARS};

use super::judging::keeps_an_assistant;
use super::*;

/// The moment `text` names, ms since the epoch.
fn at(text: &str) -> u64 {
    u64::try_from(text.parse::<jiff::Timestamp>().unwrap().as_millisecond()).unwrap()
}

/// One entry as a calendar server holds it, half an hour long, at `start`
/// on the clock of Berlin.
fn entry(uid: &str, title: &str, start: &str, more: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Server//EN\r\nBEGIN:VEVENT\r\n\
         UID:{uid}\r\nSUMMARY:{title}\r\nDTSTART;TZID=Europe/Berlin:{start}\r\nDURATION:PT30M\r\n\
         {more}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

/// `series` with the occurrence of `day` moved to `start` on the same day.
fn moved(series: &str, day: &str, start: &str) -> String {
    series.replace(
        "END:VCALENDAR\r\n",
        &format!(
            "BEGIN:VEVENT\r\nUID:standup-1@example.org\r\nSUMMARY:Standup\r\n\
             RECURRENCE-ID;TZID=Europe/Berlin:{day}T093000\r\n\
             DTSTART;TZID=Europe/Berlin:{day}T{start}\r\nDURATION:PT30M\r\nCATEGORIES:Board\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n"
        ),
    )
}

/// A person's own program: a quarter of an hour before each entry they filed
/// under Board, hand their assistant an instruction.
fn before_board_entries() -> Program {
    own_program(
        tilo_automation::TriggerSpec::ResourceTime {
            kind: PERSON_CALENDARS.into(),
            field: CALENDAR_START.into(),
            offset_secs: -900,
            filter: serde_json::Map::from_iter([("categories".to_owned(), Value::from("board"))]),
        },
        vec![tell("Say what this entry is about.")],
    )
}

/// What the assistant was handed so far: for each instruction the date its
/// occurrence has in its series and the moment the occurrence starts.
fn handed(assistant: &Assistant) -> Vec<(String, u64)> {
    assistant
        .taken()
        .iter()
        .map(|instruction| {
            // The event's identity: that it is a time, the workspace, the
            // trigger's place, what holds the time, the time held.
            let event = instruction.cause.event.as_deref().unwrap();
            let mut parts = event.split('\0');
            assert_eq!(parts.next(), Some("time"), "{event:?}");
            let held_by = parts.nth(2).unwrap();
            let at_ms = parts.next().unwrap().parse().unwrap();
            let day = held_by.rsplit('/').next().unwrap().to_owned();
            (day, at_ms)
        })
        .collect()
}

/// A person who keeps an assistant in a workspace of theirs, on a holder no
/// device is connected to: the holder's workspaces, the workspace, the
/// person's certificate, and what keeps the holder going while it is held.
pub(super) async fn person_with_an_assistant(
    scratch: &std::path::Path,
) -> (Workspaces, String, DeviceCert, impl Sized) {
    let (key, cert) = governor();
    let holder = super::event_server_taking_part(scratch, &key, &cert).await;
    let workspaces = holder.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let owner = hex::encode(cert.user_public_key);
    keeps_an_assistant(&workspaces, &workspace, &cert, &owner, None).await;
    (workspaces, workspace, cert, holder)
}

/// The program [`before_board_entries`] carried out for the person of
/// `cert`, its progress kept in `state` and its time read from `clock`: as
/// it starts, and as it starts again after its holder stopped.
async fn waiting(
    (workspaces, workspace, cert): (&Workspaces, &str, &DeviceCert),
    assistant: &Arc<Assistant>,
    clock: &Arc<tilo_clock::ManualClock>,
    state: std::path::PathBuf,
) -> ProgramRunner {
    let mut program = carried_out_for(
        program_runner(workspaces, workspace, cert, before_board_entries(), state).await,
        hex::encode(cert.user_public_key),
        assistant,
    );
    program.use_clock(clock.clone());
    program
}

/// Set the clock to `when` and let the program look once.
async fn pass(runner: &mut ProgramRunner, clock: &tilo_clock::ManualClock, when: &str) {
    clock.set(at(when));
    runner.step().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_moment_before_each_entry_of_a_category_fires_once_and_follows_the_calendar() {
    let scratch = tempfile::tempdir().unwrap();
    let (workspaces, workspace, cert, _holder) = person_with_an_assistant(scratch.path()).await;
    let set = (&workspaces, workspace.as_str(), &cert);
    let state = scratch.path().join("times.json");
    let assistant = Assistant::answering(vec![]);

    // Every day at 09:30 in Berlin from Monday 19 October 2026, over the end
    // of summer time on the 25th: Wednesday is left out and Thursday is
    // moved to 14:00. Lunch is filed under nothing. Friday is also a whole
    // day filed under Board.
    let standup = moved(
        &entry(
            "standup-1@example.org",
            "Standup",
            "20261019T093000",
            "RRULE:FREQ=DAILY;COUNT=9\r\nEXDATE;TZID=Europe/Berlin:20261021T093000\r\n\
             CATEGORIES:Board\r\n",
        ),
        "20261022",
        "140000",
    );
    let lunch = entry("lunch-1@example.org", "Lunch", "20261019T120000", "");
    let offsite = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Server//EN\r\n\
                   BEGIN:VEVENT\r\nUID:offsite-1@example.org\r\nSUMMARY:Offsite\r\n\
                   DTSTART;VALUE=DATE:20261023\r\nDTEND;VALUE=DATE:20261024\r\n\
                   CATEGORIES:Board\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        .to_owned();
    let keep = async |entries: Vec<String>| {
        workspaces
            .keep_calendar_as_pulled(&workspace, &entries)
            .await
            .unwrap();
    };
    keep(vec![standup.clone(), lunch.clone(), offsite.clone()]).await;

    let manual = Arc::new(tilo_clock::ManualClock::new(at("2026-10-19T06:00:00Z")));
    let mut runner = waiting(set, &assistant, &manual, state.clone()).await;
    let mut expected: Vec<(String, u64)> = Vec::new();

    // Nothing before its moment, and its moment once, however often the
    // program looks.
    pass(&mut runner, &manual, "2026-10-19T06:00:00Z").await;
    pass(&mut runner, &manual, "2026-10-19T07:14:59Z").await;
    assert_eq!(handed(&assistant), expected);
    pass(&mut runner, &manual, "2026-10-19T07:15:00Z").await;
    expected.push(("2026-10-19".into(), at("2026-10-19T07:30:00Z")));
    assert_eq!(handed(&assistant), expected);
    pass(&mut runner, &manual, "2026-10-19T07:20:00Z").await;
    // An entry of no category is nobody's moment.
    pass(&mut runner, &manual, "2026-10-19T09:50:00Z").await;
    assert_eq!(handed(&assistant), expected);

    // The next occurrence of the series is its own time.
    pass(&mut runner, &manual, "2026-10-20T07:15:00Z").await;
    expected.push(("2026-10-20".into(), at("2026-10-20T07:30:00Z")));
    assert_eq!(handed(&assistant), expected);

    // The day the series leaves out, and the hour an occurrence was moved
    // away from: nothing. The moved one fires before where it was moved to.
    pass(&mut runner, &manual, "2026-10-21T07:40:00Z").await;
    pass(&mut runner, &manual, "2026-10-22T07:40:00Z").await;
    assert_eq!(handed(&assistant), expected);
    pass(&mut runner, &manual, "2026-10-22T11:45:00Z").await;
    expected.push(("2026-10-22".into(), at("2026-10-22T12:00:00Z")));
    assert_eq!(handed(&assistant), expected);

    // A whole day begins at its person's midnight, 22:00 UTC the evening
    // before in summer time.
    pass(&mut runner, &manual, "2026-10-22T21:45:00Z").await;
    expected.push(("2026-10-23".into(), at("2026-10-22T22:00:00Z")));
    assert_eq!(handed(&assistant), expected);

    // The server restarts: what was answered stays answered, and the next
    // moment still comes.
    drop(runner);
    let mut runner = waiting(set, &assistant, &manual, state.clone()).await;
    pass(&mut runner, &manual, "2026-10-23T07:00:00Z").await;
    assert_eq!(handed(&assistant), expected);
    pass(&mut runner, &manual, "2026-10-23T07:15:00Z").await;
    expected.push(("2026-10-23".into(), at("2026-10-23T07:30:00Z")));
    assert_eq!(handed(&assistant), expected);

    // Down over Saturday's moment and back before the entry starts: late,
    // and still ahead of it.
    pass(&mut runner, &manual, "2026-10-24T07:25:00Z").await;
    expected.push(("2026-10-24".into(), at("2026-10-24T07:30:00Z")));
    assert_eq!(handed(&assistant), expected);

    // Down over Sunday's moment and back once the entry has begun: nothing,
    // then or later. Summer time ended that night, so it began at 08:30 UTC.
    pass(&mut runner, &manual, "2026-10-25T08:31:00Z").await;
    pass(&mut runner, &manual, "2026-10-25T12:00:00Z").await;
    assert_eq!(handed(&assistant), expected);

    // Monday is an hour later by UTC than a week before: 09:30 in Berlin
    // stayed 09:30.
    pass(&mut runner, &manual, "2026-10-26T07:15:00Z").await;
    assert_eq!(handed(&assistant), expected);
    pass(&mut runner, &manual, "2026-10-26T08:15:00Z").await;
    expected.push(("2026-10-26".into(), at("2026-10-26T08:30:00Z")));
    assert_eq!(handed(&assistant), expected);

    // Another client moves that occurrence to 11:00 after its moment was
    // answered: it holds another time, whose moment comes too.
    keep(vec![
        moved(&standup, "20261026", "110000"),
        lunch.clone(),
        offsite.clone(),
    ])
    .await;
    pass(&mut runner, &manual, "2026-10-26T08:20:00Z").await;
    assert_eq!(handed(&assistant), expected);
    pass(&mut runner, &manual, "2026-10-26T09:45:00Z").await;
    expected.push(("2026-10-26".into(), at("2026-10-26T10:00:00Z")));
    assert_eq!(handed(&assistant), expected);

    // The series is taken off the calendar before Tuesday's moment: nothing
    // is held any more, and nothing fires.
    keep(vec![lunch, offsite]).await;
    pass(&mut runner, &manual, "2026-10-27T08:15:00Z").await;
    pass(&mut runner, &manual, "2026-10-27T09:00:00Z").await;
    assert_eq!(handed(&assistant), expected);
    assert!(
        runner.dead_letters().next().is_none(),
        "{:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
}
