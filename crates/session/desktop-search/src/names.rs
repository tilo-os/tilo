//! What an application is called to the person, and what a window of it is
//! called: one answer for every surface that names an app or a window — the
//! window bars, the strips and their tabs, the switcher, the overview, the
//! window menus, the desk's published actions and search's rows. A surface
//! never shows an app id and never falls back to one: an id with no name
//! anywhere is written as words.

use crate::availability;

/// What the app `app_id` is called, from the names every desktop carries:
/// the registry's name for it, else its id written as words
/// (`org.example.file-explorer` → "File Explorer"). A host app's id is its
/// desktop entry (`host:so.tilo.notes.desktop`) and names the app it opens.
#[must_use]
pub fn app_name(app_id: &str) -> String {
    let id = package_id(app_id);
    if id.is_empty() {
        return String::new();
    }
    availability::name_for(id).map_or_else(|| humanized(id), str::to_owned)
}

/// What the window showing `app_id` and calling itself `title` is called:
/// the window's own title and the app it shows ("Groceries — Notes"). A
/// window with no title of its own is the app alone, and a title that is
/// already the app's name is not said twice.
#[must_use]
pub fn window_name(app_id: &str, title: &str) -> String {
    let app = app_name(app_id);
    if is_apps_own_name(app_id, title) {
        return app;
    }
    ui::window_name::named(&app, title)
}

/// Whether `said` is a name of the app rather than a place in it: the name
/// the app is listed under, the id written as words or the id itself, which
/// is what a window bar says for an app run without its list
/// ("so.tilo.file-explorer" is "Files" everywhere, and "File Explorer" in
/// its own empty window).
#[must_use]
pub fn is_apps_own_name(app_id: &str, said: &str) -> bool {
    let said = ui::search::fold_search_text(said.trim());
    !said.is_empty()
        && (said == ui::search::fold_search_text(&app_name(app_id))
            || said == ui::search::fold_search_text(&humanized(app_id))
            || said == ui::search::fold_search_text(package_id(app_id)))
}

/// The program an id names, where the id is the path of one: a process with
/// no installed app behind it is named by its program, and its path is never
/// shown.
fn file_name(id: &str) -> &str {
    if !id.starts_with('/') {
        return id;
    }
    id.rsplit('/').find(|part| !part.is_empty()).unwrap_or(id)
}

/// The package an id names: a host app's desktop entry (`host:` and
/// `.desktop` around it) is the same app as the package it installs.
fn package_id(app_id: &str) -> &str {
    let id = app_id.trim();
    let id = id.strip_prefix("host:").unwrap_or(id);
    id.strip_suffix(".desktop").unwrap_or(id)
}

/// An id written as words for a person: a program named by its file's name,
/// else the id's last dotted part without a `.desktop` suffix, its `-`/`_`
/// breaks made spaces, and a camel-case hump a space too —
/// "org.gnome.TextEditor" → "Text Editor", "so.tilo.file-explorer" →
/// "File Explorer", "/usr/bin/notify-send" → "Notify Send".
#[must_use]
pub fn humanized(app_id: &str) -> String {
    let id = file_name(package_id(app_id));
    if id.trim().is_empty() {
        return app_id.to_owned();
    }
    collaboration::history_source::authors::app_words(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_apps_own_name_never_reads_as_an_id() {
        assert_eq!(app_name("so.tilo.file-explorer"), "Files");
        assert_eq!(app_name("org.example.TextEditor"), "Text Editor");
        assert_eq!(app_name("org.example.file-explorer"), "File Explorer");
        assert_eq!(app_name("org.example.Terminal"), "Terminal");
        assert_eq!(app_name("org.example.Krita.desktop"), "Krita");
        assert_eq!(app_name("  "), "");
    }

    #[test]
    fn a_program_named_by_its_path_is_named_by_the_program() {
        // A sender with no installed app behind it is named by its file:
        // never by the path it runs from.
        assert_eq!(app_name("/usr/bin/notify-send"), "Notify Send");
        assert_eq!(app_name("/usr/bin/gnome-text-editor"), "Gnome Text Editor");
        assert_eq!(humanized("/opt/apps/thing"), "Thing");
        assert_eq!(humanized("/"), "/", "a path with no file name stays itself");
    }

    #[test]
    fn an_apps_own_name_in_its_window_bar_is_not_a_title() {
        // A window bar says the app's own words for it when the window holds
        // no place: an app run without its list falls back to the id's words.
        assert!(is_apps_own_name("so.tilo.file-explorer", "Files"));
        assert!(is_apps_own_name("so.tilo.file-explorer", "file explorer"));
        assert!(is_apps_own_name(
            "host:so.tilo.file-explorer.desktop",
            "File Explorer"
        ));
        assert!(!is_apps_own_name("so.tilo.file-explorer", "Downloads"));
        assert!(!is_apps_own_name("so.tilo.notes", ""));
        // A bar that writes the app's id is the app without its list, not a
        // title: "org.example.Only" is the same window as "Only".
        assert!(is_apps_own_name("org.example.Only", "org.example.Only"));
        assert!(is_apps_own_name(
            "host:org.example.Only.desktop",
            "org.example.Only"
        ));
        assert_eq!(window_name("org.example.Only", "org.example.Only"), "Only");
        assert_eq!(
            window_name("so.tilo.file-explorer", "File Explorer"),
            "Files"
        );
    }

    #[test]
    fn a_host_apps_desktop_entry_is_the_app_it_opens() {
        assert_eq!(app_name("host:so.tilo.notes.desktop"), "Notes");
        assert_eq!(app_name("host:org.example.Editor.desktop"), "Editor");
    }

    #[test]
    fn a_window_is_the_app_and_its_own_title_never_an_id() {
        assert_eq!(
            window_name("so.tilo.notes", "Groceries"),
            "Groceries — Notes",
            "the window's own title, then the app it shows"
        );
        assert_eq!(
            window_name("host:so.tilo.file-explorer.desktop", "Downloads"),
            "Downloads — Files"
        );
        assert_eq!(
            window_name("org.example.Terminal", "quinten@host: ~"),
            "quinten@host: ~ — Terminal",
            "a window claiming no app id the device knows still reads as words"
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
