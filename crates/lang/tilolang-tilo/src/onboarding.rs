//! Setup (`apps/tl/onboarding`) as the session runs it: its package, compiled from the sources
//! this build ships ([`compile`]). Setup is trusted: the session draws it before the desktop
//! exists and above every application, over the session's `setup` reading, so it always runs this
//! build's copy and never a package a person installed or bound in its place.

use crate::tilolang::Compiled;

/// Setup's package id.
pub const SETUP_APP: &str = "so.tilo.onboarding";

/// The view that draws setup over the session's `setup` reading.
pub const SETUP_VIEW: &str = "setup";

/// What setup reads when it first runs on a device with no identity: its first page, of its four
/// steps (choose, connect or create, protect, done), the count the ceremony gives a run that
/// offers no media codecs step. A page that draws setup starts it here.
pub const FIRST_RUN: &str = r#"{"step":"intro","total_steps":4}"#;

/// Every file of setup's package, by the name the package knows it by.
const FILES: &[(&str, &str)] = &[(
    "app.tl",
    include_str!("../../../../apps/tl/onboarding/app.tl"),
)];

/// The package's ids lock.
const IDS: &str = include_str!("../../../../apps/tl/onboarding/app.ids");

/// Setup's translations, by locale, travelling in its module as an installed app's do.
const CATALOGUES: [(&str, &str); 1] = [(
    "de-DE",
    include_str!("../../../../apps/tl/onboarding/locales/de-DE.ftl"),
)];

/// Setup's package, compiled over the session's and the platform's descriptions. Compiled once
/// per process.
///
/// # Errors
/// The package's diagnostics, rendered with their file names: every build of setup is checked
/// against them, so this fails loudly in a test.
pub fn compile() -> Result<Compiled, String> {
    static COMPILED: std::sync::OnceLock<Result<Compiled, String>> = std::sync::OnceLock::new();
    COMPILED
        .get_or_init(|| crate::keyboard::shipped("apps/tl/onboarding", FILES, IDS, &CATALOGUES))
        .clone()
}

#[cfg(test)]
mod tests {
    /// The setup this build ships compiles over the session it runs in: a first login never comes
    /// up without it.
    #[test]
    fn the_shipped_setup_compiles() {
        let compiled = super::compile().unwrap_or_else(|error| panic!("{error}"));
        assert!(
            compiled
                .meta
                .views
                .iter()
                .any(|view| view.name == super::SETUP_VIEW),
            "the setup view is in the package"
        );
    }

    /// Setup's words promise only what is true: the code comes from `tilo device add` on the other
    /// device and is typed here (no link), a host desktop's setup replaces neither the Linux login
    /// nor an online account and claims no signing it does not do, and no typed glyph stands in
    /// for layout.
    #[test]
    fn setups_words_say_what_is_true() {
        let source = super::FILES[0].1;
        let words: Vec<&str> = source.split('"').skip(1).step_by(2).collect();
        let said = |what: &str| words.iter().any(|line| line.contains(what));
        assert!(said("run tilo device add. It prints a code: type it here."));
        assert!(
            !words
                .iter()
                .any(|line| line.contains("code") && line.to_lowercase().contains("link")),
            "a code is never called a link"
        );
        let host = words
            .iter()
            .find(|line| line.contains("already signed in to this computer"))
            .expect("the host desktop's introduction");
        assert!(host.contains("does not replace your Linux login or create an online account"));
        assert!(host.contains("not all cryptographically signed"));
        for glyph in ['•', '·', '→', '—', '✓', '★'] {
            assert!(
                !words.iter().any(|line| line.contains(glyph)),
                "{glyph:?} used as layout"
            );
        }
    }
}
