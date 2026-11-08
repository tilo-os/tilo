//! The person's language, as every surface of the session speaks it.
//!
//! The session's words are catalogue keys resolved on the thread that draws,
//! in the language the toolkit's manager selected. That selection is the
//! person's — the same setting Settings writes and every app follows
//! ([`ui::i18n::follow_configured_language`]) — so a language chosen while the
//! session runs, here or on their other device, reaches these surfaces on the
//! pass after it is written: the manager takes the new one on the drawing
//! thread, the packs that came with it are read with it, and the surfaces
//! whose words are kept between frames lay them out again.
//!
//! A string no translation has still reads in the source language, that
//! string alone: the fallback is the manager's, per message.

use std::cell::Cell;

/// The language the session's surfaces are drawn in, followed from the person's settings.
pub(crate) struct Language {
    /// The settings version the language was last followed at, so the document is read again only
    /// when it changed.
    seen: Cell<Option<settings_store::SettingsStamp>>,
}

impl Language {
    pub(crate) fn new() -> Self {
        Self {
            seen: Cell::new(None),
        }
    }

    /// Follow the person's language when their settings changed since the last pass. Returns
    /// whether the words on screen are now resolved in another language: the caller lays out what
    /// it keeps between frames.
    pub(crate) fn follow(&self) -> bool {
        let stamp = settings_store::stamp();
        if !settings_store::document_changed(self.seen.get().is_some(), self.seen.get(), stamp) {
            return false;
        }
        self.seen.set(stamp);
        let before = ui::I18nManager::revision();
        ui::i18n::follow_configured_language();
        ui::I18nManager::revision() != before
    }
}

impl crate::Session {
    /// Speak the person's language in the session's own surfaces: the manager's selection follows
    /// their settings (see [`Language`]), and what the chrome keeps between frames is laid out
    /// again, so its words are resolved in the language now selected.
    pub(crate) fn follow_language(&self) {
        if self.language.follow() {
            self.shared.chrome.borrow_mut().relayout();
        }
    }
}
