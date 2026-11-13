//! What the calendar reads out to assistive technology, in the language the
//! person's window speaks.

use super::*;

#[test]
fn a_chip_dragged_to_another_day_reads_out_in_the_person_s_language() {
    let h = Harness::new(1280, 800);
    let event = selected_event_id(&h.app);
    let day = NaiveDate::from_ymd_opt(2026, 5, 4).expect("a day");
    let action = Action::MoveEventToDay {
        id: event,
        original: day,
        day,
    };
    let name = |locale: &str| {
        crate::resource::CATALOG.set_locale(locale);
        let (_, name, _, _) = h
            .app
            .a11y_control(&action)
            .expect("a dragged chip has a semantic action");
        name
    };
    assert_eq!(
        name("de-DE"),
        format!("Termin auf {} verschieben", date::fmt_day_long(day)),
        "the day a chip is dragged to is read out in the person's language"
    );
    assert_eq!(
        name("en-US"),
        format!("Move event to {}", date::fmt_day_long(day))
    );
}

fn reads_out(h: &Harness, action: &Action, locale: &str) -> String {
    crate::resource::CATALOG.set_locale(locale);
    h.app
        .a11y_control(action)
        .expect("a painted control has a semantic action")
        .1
}

fn assert_both_languages(h: &Harness, what: &str, action: &Action, english: &str, german: &str) {
    assert_eq!(
        reads_out(h, action, "en-US"),
        english,
        "{what}, in the source language"
    );
    assert_eq!(reads_out(h, action, "de-DE"), german, "{what}, in German");
}

#[test]
fn the_sidebar_and_the_sheet_read_out_in_the_person_s_language() {
    let h = Harness::new(1280, 800);
    let event = selected_event_id(&h.app);
    let event_title = h
        .app
        .model
        .event(event)
        .expect("the open event")
        .display_title();
    let project = h.app.model.projects.iter().next().expect("a project");
    let account = &h.app.accounts.accounts[0];
    let calendar = &account.calendars[0];
    let inbox = crate::resource::t("calendar-tasks-inbox");
    assert_both_languages(
        &h,
        "the sheet's done button",
        &Action::CloseDetail,
        "Done",
        "Fertig",
    );
    assert_both_languages(
        &h,
        "the task list's close button",
        &Action::CloseList,
        "Close list",
        "Liste schließen",
    );
    assert_both_languages(
        &h,
        "a smart list",
        &Action::List(ListSel::Inbox),
        &format!("Show tasks in {inbox}"),
        "Aufgaben in Posteingang anzeigen",
    );
    assert_both_languages(
        &h,
        "a project list",
        &Action::List(ListSel::Project(project.id)),
        &format!("Show tasks in {}", project.name),
        &format!("Aufgaben in {} anzeigen", project.name),
    );
    assert_both_languages(
        &h,
        "a calendar's visibility",
        &Action::SetCalendarVisible {
            id: calendar.id.clone(),
            visible: true,
        },
        &format!("Show calendar {}", calendar.name),
        &format!("Kalender {} anzeigen", calendar.name),
    );
    assert_both_languages(
        &h,
        "an account's disclosure",
        &Action::ToggleAccountCollapsed(account.id.clone()),
        &format!("{} calendars", account.label()),
        &format!("Kalender von {}", account.label()),
    );
    assert_both_languages(
        &h,
        "an event chip",
        &Action::OpenEvent(event),
        &format!("Open event {event_title}"),
        &format!("Termin {event_title} öffnen"),
    );
    assert_both_languages(
        &h,
        "an event's delete button",
        &Action::DeleteEvent(event),
        "Delete event",
        "Termin löschen",
    );
}

#[test]
fn a_task_and_its_checkbox_read_out_in_the_person_s_language() {
    let h = Harness::new(1280, 800);
    let open = h
        .app
        .model
        .tasks
        .iter()
        .find(|task| !task.done)
        .expect("an open task");
    let done = h
        .app
        .model
        .tasks
        .iter()
        .find(|task| task.done)
        .expect("a finished task");
    assert_both_languages(
        &h,
        "a task",
        &Action::OpenTask(open.id),
        &format!("Open task {}", open.title),
        &format!("Aufgabe {} öffnen", open.title),
    );
    assert_both_languages(
        &h,
        "an open task's checkbox",
        &Action::ToggleTask(open.id),
        &format!("Mark task {} complete", open.title),
        &format!("Aufgabe {} als erledigt markieren", open.title),
    );
    assert_both_languages(
        &h,
        "a finished task's checkbox",
        &Action::ToggleTask(done.id),
        &format!("Reopen task {}", done.title),
        &format!("Aufgabe {} wieder öffnen", done.title),
    );
}
