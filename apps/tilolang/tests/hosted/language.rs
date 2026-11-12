//! The language a hosted window reads in: the person's, as their session names it with the
//! instance and again when they choose another in Settings.

use super::*;

/// A person whose desktop speaks German reads a hosted window's words in German: the instance runs
/// in a host whose own process speaks the source language (a host is sandboxed with only the
/// locale its launcher happened to carry, and cannot read the person's settings), and the language
/// their session names is what the window speaks, so every word it paints — its app's own, an
/// action's name, another app's place, the standard library's frames — comes from a catalogue in
/// that language for every app.
#[test]
fn a_hosted_windows_own_words_read_in_its_language() {
    // Today ships German: its own button, its title and its frames read in it.
    let mut today = Session::in_language(TODAY, "ada", "de-DE");
    let shown = today.shown();
    assert!(
        shown.iter().any(|word| word.starts_with("Morgenroutine")),
        "Today's morning cleanup reads in German: {shown:?}"
    );
    assert!(
        !shown.iter().any(|word| word.starts_with("Morning cleanup")),
        "no word Today's package has in German is left in its source language: {shown:?}"
    );
    assert!(
        shown.iter().any(|word| word == "Heute"),
        "Today's own page reads in German: {shown:?}"
    );
    // The places its page draws are the labels of the apps that hold them, in the language the
    // window speaks: Notes' pinned place reads in German, Tasks' Today place — a package that
    // ships its source language only — reads its own word, never a translation of another app's.
    assert!(
        shown.iter().any(|word| word == "Angeheftet"),
        "Notes' place reads in German in Today's window: {shown:?}"
    );
    assert!(
        shown.iter().any(|word| word == "Today"),
        "Tasks' place reads the word its own catalogue gives: {shown:?}"
    );

    // Notes ships German too: its own new note reads in it, in a window of its own.
    let mut notes = Session::in_language(NOTES, "ada", "de-DE");
    let shown = notes.shown();
    assert!(
        shown.iter().any(|word| word.starts_with("Neue Notiz")),
        "Notes' new note reads in German: {shown:?}"
    );

    // A window speaks one language: Tasks ships no German, so its window reads English there —
    // and every word of it, the frames' included, is the English its catalogue holds.
    let mut tasks = Session::in_language(TASKS, "ada", "de-DE");
    let shown = tasks.shown();
    assert!(
        shown.iter().any(|word| word.starts_with("New task")),
        "Tasks' own action reads its catalogue's word: {shown:?}"
    );
    assert!(
        !shown.iter().any(|word| word.contains("Aufgabe")),
        "a window whose package ships no German reads no German: {shown:?}"
    );
}

/// A language the person chooses in Settings while a hosted window is open relabels that window
/// where it stands: the session says it on the host's door, as it says their formats, and every
/// word the window paints reads in it on the next frame, without the window opening again.
#[test]
fn a_language_chosen_while_a_hosted_window_is_open_relabels_it() {
    let mut today = Session::in_language(TODAY, "ada", "en-US");
    let shown = today.shown();
    assert!(
        shown.iter().any(|word| word.starts_with("Morning cleanup")),
        "Today opens in English: {shown:?}"
    );
    let formats = today.host.formats().get();
    assert!(
        today.host.set_locale(&tilolang_host::Locale {
            formats,
            language: "de-DE".to_owned(),
        }),
        "another language is a change"
    );
    today.settle();
    let shown = today.shown();
    assert!(
        shown.iter().any(|word| word.starts_with("Morgenroutine")),
        "Today's button reads German where it stands: {shown:?}"
    );
    assert!(
        shown.iter().any(|word| word == "Heute"),
        "Today's page reads German where it stands: {shown:?}"
    );
    assert!(
        !shown.iter().any(|word| word.starts_with("Morning cleanup")),
        "no English word is left: {shown:?}"
    );
}
