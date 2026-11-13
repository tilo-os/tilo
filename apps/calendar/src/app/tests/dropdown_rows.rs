//! The editor's dropdown rows in a German window.

use super::*;
use crate::app::editor;

#[test]
fn the_dropdown_rows_read_in_german() {
    let catalog = &crate::resource::CATALOG;
    catalog.set_locale("de-DE");
    let repeats: Vec<String> = editor::repeat_options(&Event::default())
        .into_iter()
        .map(|(_, label)| label)
        .collect();
    assert_eq!(repeats[0], "Wiederholt sich nicht");
    assert_eq!(repeats[1], "Jeden Tag");
    assert_eq!(repeats[4], "Alle 2 Wochen");
    let custom = Event {
        recurrence: Recurrence::Weekly,
        recurrence_count: Some(3),
        ..Event::default()
    };
    assert_eq!(
        editor::repeat_options(&custom)
            .last()
            .map(|(_, label)| label.as_str()),
        Some("Jede Woche, 3 Mal"),
        "a custom rule counts in German"
    );
    let alerts: Vec<String> = editor::alert_options(&Event::default())
        .into_iter()
        .map(|(_, label)| label)
        .collect();
    assert_eq!(alerts[0], "Keine Erinnerung");
    assert_eq!(alerts[1], "Zum Beginn");
    assert_eq!(alerts[2], "5 Minuten vorher");
    assert_eq!(alerts[6], "1 Stunde vorher");
    assert_eq!(alerts[7], "1 Tag vorher");
    let zones: Vec<String> = editor::timezone_options(&Event::default())
        .into_iter()
        .map(|(_, label)| label)
        .collect();
    assert_eq!(zones[0], "Keine Zeitzone");
    catalog.set_locale("en-US");
}
