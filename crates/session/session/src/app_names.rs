//! What an application is called to the person, and what a window of it is called: one answer for
//! every surface that names an app or a window — the window bars, the strips and their tabs, the
//! switcher, the overview, the window menus, the questions an app asks, and search's rows. A
//! surface never shows an app id and never falls back to one: an id with no name anywhere is
//! written as words.
//! The naming itself is [`tilo_desktop_search::names`]'s, which every desktop carries; this adds
//! what only a session knows: the name the device's app lists last read for an installed app or
//! desktop entry.

/// The `Name[xx]` candidates of the person's interface language: what every desktop entry this
/// session reads is named in, so a German machine with English chosen lists `Files` and not
/// `Dateien` .
pub(crate) fn entry_locales() -> Vec<String> {
    desktop_entry::locale::language_candidates(
        settings_store::appearance::preferred_language().as_deref(),
    )
}

/// What the app `app_id` is called: the installed package's own name, else the machine's desktop
/// entry for it, else the registry's name, else its id written as words
/// (`org.example.file-explorer` → "File Explorer").
pub(crate) fn app_name(app_id: &str) -> String {
    let id = app_id.trim();
    if id.is_empty() {
        return String::new();
    }
    if let Some(name) = listed_name(id) {
        return name;
    }
    tilo_desktop_search::names::app_name(id)
}

/// What a notification from a program the launcher proved with no installed app behind it is
/// called: the name the program gives itself when that is a name and not the name of its own file,
/// else the desktop entry it says it comes from, else the terminal it was typed into. A command
/// line program names nothing a person reads, so the centre says where it came from instead.
pub(crate) fn program_sender_name(
    program: &str,
    said: &str,
    desktop_entry: Option<&str>,
) -> String {
    let program_name = program.rsplit('/').next().unwrap_or(program);
    match tilo_desktop_surface::notifications::sender_words(said) {
        Some(words) if !words.eq_ignore_ascii_case(program_name) => words,
        Some(_) => entry_name(desktop_entry)
            .unwrap_or_else(|| crate::i18n::t("notification-sender-terminal")),
        // A path is the program naming itself by its file: its words, never the path.
        None if said.trim().starts_with('/') => tilo_desktop_search::names::humanized(said.trim()),
        None => entry_name(desktop_entry)
            .unwrap_or_else(|| crate::i18n::t("notification-sender-terminal")),
    }
}

/// The name a desktop entry gives itself, when the machine has that entry.
fn entry_name(desktop_id: Option<&str>) -> Option<String> {
    let id = desktop_id?.trim();
    if id.is_empty() {
        return None;
    }
    desktop_entry::cache::cached_scan(&entry_locales())
        .find(id)
        .map(|entry| entry.name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// What the window showing `app_id` and calling itself `title` is called: the window's own title
/// and the app it shows ("Groceries — Notes"). A window with no title of its own is the app alone,
/// and a title that is already the app's name is not said twice.
pub(crate) fn window_name(app_id: &str, title: &str) -> String {
    ui::window_name::named(&app_name(app_id), title)
}

/// The app's name as this device's app lists last read it: the installed package's name in the
/// person's interface language, or the machine's desktop entry.
pub(crate) fn listed_name(app_id: &str) -> Option<String> {
    let lists = crate::app_icons::app_lists()?;
    if let Some(app) = lists
        .applications
        .iter()
        .find(|app| app.id == app_id)
        .filter(|app| !app.title.trim().is_empty())
    {
        return Some(app.title.clone());
    }
    lists
        .host_apps
        .iter()
        .find(|app| app.desktop_id == app_id)
        .map(|app| app.name.trim())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// The name an app asks under: its own name where this device knows the app,
/// else the id it gives for itself. A question about an app shows the
/// claimant, and never a name made up for one the device does not know.
pub(crate) fn claimant(app_id: &str) -> String {
    listed_name(app_id).unwrap_or_else(|| app_id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_apps_own_name_never_reads_as_an_id() {
        // The registry names the apps Tilo carries; an app it does not carry is written from its
        // id, never shown as one.
        assert_eq!(app_name("so.tilo.file-explorer"), "Files");
        assert_eq!(app_name("org.example.TextEditor"), "Text Editor");
        assert_eq!(app_name("org.example.file-explorer"), "File Explorer");
        assert_eq!(app_name("org.example.Terminal"), "Terminal");
        assert_eq!(app_name("org.example.Krita.desktop"), "Krita");
        assert_eq!(app_name("  "), "");
    }

    #[test]
    fn a_window_is_the_app_and_its_own_title_never_an_id() {
        assert_eq!(
            window_name("so.tilo.notes", "Groceries"),
            "Groceries — Notes",
            "the window's own title, then the app it shows"
        );
        assert_eq!(
            window_name("so.tilo.file-explorer", "Downloads"),
            "Downloads — Files"
        );
        assert_eq!(
            window_name("org.example.Terminal", "quinten@host: ~"),
            "quinten@host: ~ — Terminal",
            "a window claiming no app id the device knows still reads as words"
        );
    }

    #[test]
    fn a_sender_with_no_installed_app_is_never_named_by_its_path() {
        // A process the session proved, with no app behind it, names itself;
        // the bar and the banner say its name, never the file it runs from.
        assert_eq!(
            program_sender_name("/usr/bin/notify-send", "Notes", None),
            "Notes",
            "the name its sender gives is the app's own word"
        );
        assert_eq!(
            program_sender_name("/usr/bin/notify-send", "a\u{7}b", None),
            "ab",
            "a name with control characters shows none of them"
        );
        assert_eq!(
            program_sender_name("/usr/bin/notify-send", "/usr/bin/notify-send", None),
            "Notify Send",
            "a program that gives its path as its name is named by its file"
        );
    }

    /// A command line sender that names itself by its program (`notify-send`) names nothing a
    /// person reads: the centre says where it came from. A desktop entry the sender points at
    /// names it instead, when the machine has that entry.
    #[test]
    fn a_command_line_sender_is_the_terminal_or_its_desktop_entry() {
        assert_eq!(
            program_sender_name("/usr/bin/notify-send", "notify-send", None),
            "Terminal",
            "a program naming itself by its file is a command run in a terminal"
        );
        assert_eq!(
            program_sender_name("/usr/bin/notify-send", "", None),
            "Terminal",
            "a program naming nothing came from the terminal too"
        );
        assert_eq!(
            program_sender_name(
                "/usr/bin/notify-send",
                "notify-send",
                Some("no.such.Entry.desktop")
            ),
            "Terminal",
            "an entry the machine does not have names nothing"
        );
    }

    #[test]
    fn a_window_with_no_title_is_the_app_alone() {
        assert_eq!(window_name("so.tilo.notes", ""), "Notes");
        assert_eq!(window_name("so.tilo.notes", "   "), "Notes");
        assert_eq!(
            window_name("so.tilo.notes", "Notes"),
            "Notes",
            "the app's name is not said twice"
        );
    }
}
