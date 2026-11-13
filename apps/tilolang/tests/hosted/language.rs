//! The language a hosted window reads in: the person's, as their session names it with the
//! instance and again when they choose another in Settings.

use super::*;

/// A person whose desktop speaks German reads every hosted window's words in German: the instance
/// runs in a host whose own process speaks the source language (a host is sandboxed with only the
/// locale its launcher happened to carry, and cannot read the person's settings), and the language
/// their session names is what the window speaks, so every word it paints — its app's own, an
/// action's name, another app's place, the standard library's frames — comes from a catalogue in
/// that language, in a window of any app.
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
    // window speaks: Notes' pinned place, Tasks' Today place and Chat's unread place each read the
    // word their own package gives in German, though Today's instance draws them.
    assert!(
        shown.iter().any(|word| word == "Angeheftet"),
        "Notes' place reads in German in Today's window: {shown:?}"
    );
    assert!(
        shown.iter().any(|word| word == "Ungelesen"),
        "Chat's place reads in German in Today's window: {shown:?}"
    );
    assert!(
        !shown.iter().any(|word| word == "Today" || word == "Unread"),
        "no card names another app's place in the source language: {shown:?}"
    );

    // Notes ships German too: its own new note reads in it, in a window of its own.
    let mut notes = Session::in_language(NOTES, "ada", "de-DE");
    let shown = notes.shown();
    assert!(
        shown.iter().any(|word| word.starts_with("Neue Notiz")),
        "Notes' new note reads in German: {shown:?}"
    );

    // Tasks' window reads German throughout: its action, the places it lists, its frames.
    let mut tasks = Session::in_language(TASKS, "ada", "de-DE");
    let shown = tasks.shown();
    assert!(
        shown.iter().any(|word| word.starts_with("Neue Aufgabe")),
        "Tasks' own action reads in German: {shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|word| word.starts_with("Heute") || word == "Aufgaben"),
        "Tasks' places and pages read in German: {shown:?}"
    );
    assert!(
        !shown.iter().any(|word| word.starts_with("New task")),
        "no word Tasks' package has in German is left in its source language: {shown:?}"
    );

    // Chat's window: its sidebar, its new channel and the place it opens on read German.
    let mut chat = Session::in_language(CHAT, "ada", "de-DE");
    let shown = chat.shown();
    for word in ["Kanäle", "Kein Kanal offen"] {
        assert!(
            shown.iter().any(|shown| shown == word),
            "Chat reads {word:?} in German: {shown:?}"
        );
    }
    assert!(
        !shown.iter().any(|word| word == "Channels"),
        "no word Chat's package has in German is left in its source language: {shown:?}"
    );
}

/// A language the person chooses in Settings while hosted windows are open relabels them where
/// they stand, all of them: the session says their language on every host's door, as it says their
/// formats, and every word each window paints reads in it on the next frame, without a window
/// opening again. A place one app's card names reads in the language of the window drawing it.
#[test]
fn a_language_chosen_while_hosted_windows_are_open_relabels_them() {
    let mut today = Session::in_language(TODAY, "ada", "en-US");
    let mut tasks = Session::in_language(TASKS, "ada", "en-US");
    let shown = today.shown();
    assert!(
        shown.iter().any(|word| word.starts_with("Morning cleanup")),
        "Today opens in English: {shown:?}"
    );
    assert!(
        shown.iter().any(|word| word == "Today"),
        "Tasks' Today place reads English in Today's card: {shown:?}"
    );
    let shown = tasks.shown();
    assert!(
        shown.iter().any(|word| word.starts_with("New task")),
        "Tasks opens in English: {shown:?}"
    );

    // The person picks German once: the one locale their session says on the door of every host
    // it runs, each of which speaks it to every instance it holds.
    let german = tilolang_host::Locale {
        formats: today.host.formats().get(),
        language: "de-DE".to_owned(),
    };
    assert!(
        today.host.set_locale(&german),
        "another language is a change"
    );
    assert!(
        tasks.host.set_locale(&german),
        "another language is a change"
    );
    today.settle();
    tasks.settle();

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
        !shown.iter().any(|word| word == "Today" || word == "Unread"),
        "the card naming Tasks' place and the one naming Chat's read German where they stand: {shown:?}"
    );
    assert!(
        !shown.iter().any(|word| word.starts_with("Morning cleanup")),
        "no English word is left in Today's window: {shown:?}"
    );

    let shown = tasks.shown();
    assert!(
        shown.iter().any(|word| word.starts_with("Neue Aufgabe")),
        "Tasks' window relabels where it stands: {shown:?}"
    );
    assert!(
        !shown.iter().any(|word| word.starts_with("New task")),
        "no English word is left in Tasks' window: {shown:?}"
    );
}
