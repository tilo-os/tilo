//! What an application is called to the person, and what a window of it is
//! called: one answer for every surface that names an app or a window — the
//! window bars, the strips and their tabs, the switcher, the overview, the
//! window menus, the shell's published actions and search's rows. A surface
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
    window_name_of(app_id, &app_name(app_id), title)
}

/// [`window_name`] for a host that names the app itself — a session reading
/// the device's app lists, whose name for it the registry need not know.
#[must_use]
pub fn window_name_of(app_id: &str, app: &str, title: &str) -> String {
    if is_name_of(app_id, app, title) {
        return app.to_owned();
    }
    ui::window_name::named(app, title)
}

/// The title a surface shows for the window `app_id` beside the app's own
/// mark: the window's own title, or the app's name where the title is only
/// the app under another spelling (`chat` is `Chat`) — said once, never the
/// name joined to a slug. A surface with the app's mark already beside the
/// title does not repeat the app's name in it.
#[must_use]
pub fn window_title(app_id: &str, title: &str) -> String {
    window_title_of(app_id, &app_name(app_id), title)
}

/// [`window_title`] for a host that names the app itself.
#[must_use]
pub fn window_title_of(app_id: &str, app: &str, title: &str) -> String {
    if title.trim().is_empty() || is_name_of(app_id, app, title) {
        app.to_owned()
    } else {
        title.trim().to_owned()
    }
}

/// Whether `said` is a name of the app rather than a place in it: the name
/// the app is listed under, the id written as words or the id itself, which
/// is what a window bar says for an app run without its list
/// ("so.tilo.file-explorer" is "Files" everywhere, and "File Explorer" in
/// its own empty window).
#[must_use]
pub fn is_apps_own_name(app_id: &str, said: &str) -> bool {
    is_name_of(app_id, &app_name(app_id), said)
}

/// Whether `said` is the app `app_id` under any spelling, where a device
/// names the app `app`: the name it is listed under, the registry's name,
/// the id written as words, the id itself, or the id's last part (`chat`,
/// `file-explorer`), which is the slug a launch or a client announces. A
/// host that resolves names itself (a session reading the device's app
/// lists) asks this, so a window that names its app — `chat`, `Chat` or
/// `so.tilo.chat` — is never said twice and never shows an id.
#[must_use]
pub fn is_name_of(app_id: &str, app: &str, said: &str) -> bool {
    let id = package_id(app_id);
    let last = id.rsplit('.').next().unwrap_or(id);
    let said = ui::search::fold_search_text(said.trim());
    !said.is_empty()
        && (said == ui::search::fold_search_text(app.trim())
            || said == ui::search::fold_search_text(&app_name(app_id))
            || said == ui::search::fold_search_text(&humanized(app_id))
            || said == ui::search::fold_search_text(last)
            || said == ui::search::fold_search_text(id))
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

    tilo_test_env::scenario! {
        fn a_row_is_named_by_its_app_and_title_never_by_an_id;

        phase an_apps_own_name_never_reads_as_an_id;
        {
            assert_eq!(app_name("so.tilo.file-explorer"), "Files");
            assert_eq!(app_name("org.example.TextEditor"), "Text Editor");
            assert_eq!(app_name("org.example.file-explorer"), "File Explorer");
            assert_eq!(app_name("org.example.Terminal"), "Terminal");
            assert_eq!(app_name("org.example.Krita.desktop"), "Krita");
            assert_eq!(app_name("  "), "");
        }

        phase a_program_named_by_its_path_is_named_by_the_program;
        {
            // A sender with no installed app behind it is named by its file:
            // never by the path it runs from.
            assert_eq!(app_name("/usr/bin/notify-send"), "Notify Send");
            assert_eq!(app_name("/usr/bin/gnome-text-editor"), "Gnome Text Editor");
            assert_eq!(humanized("/opt/apps/thing"), "Thing");
            assert_eq!(humanized("/"), "/", "a path with no file name stays itself");
        }

        phase an_apps_own_name_in_its_window_bar_is_not_a_title;
        {
            // A window bar says the app's own words for it when the window holds
            // no place: an app run without its list falls back to the id's words.
            assert!(is_apps_own_name("so.tilo.file-explorer", "Files"));
            assert!(is_apps_own_name("so.tilo.file-explorer", "file explorer"));
            assert!(is_apps_own_name("so.tilo.file-explorer", "file-explorer"));
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

        phase a_name_a_device_lists_is_the_apps_own_too;
        {
            // A session names an app the device lists it under, which the
            // registry need not know: the window saying that name is the app
            // alone, never the name joined to the app's other spelling.
            assert!(is_name_of("so.tilo.file-explorer", "Dateien", "dateien"));
            assert!(is_name_of("so.tilo.chat", "Chat", "chat"));
            assert!(is_name_of("so.tilo.chat", "Chat", "so.tilo.chat"));
            assert!(!is_name_of("so.tilo.chat", "Chat", "design"));
            assert_eq!(
                window_name_of("so.tilo.file-explorer", "Dateien", "dateien"),
                "Dateien"
            );
            assert_eq!(
                window_name_of("so.tilo.file-explorer", "Dateien", "Downloads"),
                "Downloads — Dateien"
            );
        }

        phase a_host_apps_desktop_entry_is_the_app_it_opens;
        {
            assert_eq!(app_name("host:so.tilo.notes.desktop"), "Notes");
            assert_eq!(app_name("host:org.example.Editor.desktop"), "Editor");
        }

        phase a_window_is_the_app_and_its_own_title_never_an_id;
        {
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

        phase a_window_with_no_title_is_the_app_alone;
        {
            assert_eq!(window_name("so.tilo.notes", ""), "Notes");
            assert_eq!(window_name("so.tilo.notes", "   "), "Notes");
            assert_eq!(
                window_name("so.tilo.notes", "Notes"),
                "Notes",
                "the app's name is not said twice"
            );
        }

        phase a_title_beside_the_apps_mark_never_repeats_the_app;
        {
            // A surface with the app's mark beside the title shows the title
            // alone, and a title that is the app under another spelling is the
            // app's name: never "chat", never "Groceries — Notes" beside the
            // Notes mark.
            assert_eq!(window_title("so.tilo.chat", "chat"), "Chat");
            assert_eq!(window_title("so.tilo.notes", "Groceries"), "Groceries");
            assert_eq!(
                window_title("so.tilo.notes", ""),
                "Notes",
                "a window with no title of its own is its app"
            );
            assert_eq!(
                window_title_of("so.tilo.file-explorer", "Dateien", "dateien"),
                "Dateien"
            );
        }
    }
}
