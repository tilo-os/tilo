//! What a window is called to the person: the app it shows, and the title the window
//! carries beside it ("Groceries — Notes"). An app never titles its window
//! (docs/design-language.md, Words and structure): the chrome names the app, and the
//! window's title is the one it hands the bar ([`crate::window_toolbar`]).

/// The name of the window showing `app` and calling itself `title`: the app alone where
/// the window has no title of its own, or names the app itself under any spelling
/// (`chat` is `Chat`); the title alone where the app has no name; both, the title first,
/// otherwise.
#[must_use]
pub fn named(app: &str, title: &str) -> String {
    let title = title.trim();
    if title.is_empty() || names_app(app, title) {
        app.to_owned()
    } else if app.is_empty() {
        title.to_owned()
    } else {
        format!("{title} — {app}")
    }
}

/// Whether `title` is `app`'s own name rather than a place inside it: the same words
/// however they are spelled, so a window that names its app is that app alone.
fn names_app(app: &str, title: &str) -> bool {
    title.eq_ignore_ascii_case(app.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_its_title_and_the_app_showing_it() {
        assert_eq!(named("Notes", "Groceries"), "Groceries — Notes");
        assert_eq!(named("Notes", ""), "Notes");
        assert_eq!(named("Notes", "   "), "Notes");
        assert_eq!(named("Notes", " Notes "), "Notes");
        assert_eq!(named("", "Groceries"), "Groceries");
        assert_eq!(named("", ""), "");
    }

    /// A window that names its app, however the app is spelled there, is that app alone:
    /// the app's name is said once, never beside a slug or an id.
    #[test]
    fn a_window_that_names_its_app_says_it_once() {
        assert_eq!(named("Chat", "chat"), "Chat");
        assert_eq!(named("Chat", "CHAT"), "Chat");
        assert_eq!(named("Files", "files "), "Files");
        assert_eq!(named("Chat", "design"), "design — Chat");
    }
}
