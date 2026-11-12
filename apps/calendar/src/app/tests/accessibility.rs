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
