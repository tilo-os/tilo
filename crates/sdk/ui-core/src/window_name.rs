//! What a window is called to the person: the app it shows, and the title the window
//! carries beside it ("Groceries — Notes"). An app never titles its window
//! (docs/design-language.md, Words and structure): the chrome names the app, and the
//! window's title is the one it hands the bar ([`crate::window_toolbar`]).

/// The name of the window showing `app` and calling itself `title`: the app alone where
/// the window has no title of its own, or calls itself what it shows; the title alone
/// where the app has no name; both, the title first, otherwise.
#[must_use]
pub fn named(app: &str, title: &str) -> String {
    let title = title.trim();
    if title.is_empty() || title == app {
        app.to_owned()
    } else if app.is_empty() {
        title.to_owned()
    } else {
        format!("{title} — {app}")
    }
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
}
