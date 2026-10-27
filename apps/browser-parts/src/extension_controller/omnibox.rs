//! The address's keyword for the window's extensions: which running
//! extension what is typed goes to, what it is told, and what Enter asks of
//! it ([`crate::extension_omnibox`]).

use ui::webview::SharedEngine;

use super::ExtensionController;
use crate::effects::ExtensionRequest;
use crate::extension_omnibox::{Keyword, OmniboxRow};

impl ExtensionController {
    /// The running extension whose keyword `typed` starts with, then a
    /// space, and what follows it.
    fn addressed(&self, typed: &str) -> Option<(Keyword, String)> {
        let running = || {
            self.installed
                .iter()
                .filter(|extension| extension.enabled)
                .filter_map(|extension| Some((extension, extension.keyword.as_deref()?)))
        };
        let (index, text) =
            crate::extension_omnibox::split(typed, running().map(|(_, keyword)| keyword))?;
        let (extension, keyword) = running().nth(index)?;
        Some((
            Keyword {
                id: extension.id.clone(),
                name: extension.name.clone(),
                keyword: keyword.to_owned(),
            },
            text.to_owned(),
        ))
    }

    /// What is typed in the address now. After an extension's keyword and a
    /// space, what follows is the extension's: it hears of each change, and
    /// of the person leaving. Returns `Some` with whether its rows changed
    /// while the address is the extension's, `None` while it is not.
    pub fn omnibox_typed(&mut self, typed: &str, engine: &SharedEngine) -> Option<bool> {
        if !self.active {
            return None;
        }
        let addressed = self.addressed(typed);
        let changed = self.omnibox.typed(
            typed,
            addressed
                .as_ref()
                .map(|(keyword, text)| (keyword, text.as_str())),
            engine,
        );
        addressed.map(|_| changed)
    }

    /// The extension typed to after its keyword, and its rows for the
    /// address: what Enter does first, then its suggestions.
    pub fn omnibox_rows(&self) -> Option<(&Keyword, Vec<OmniboxRow>)> {
        self.omnibox.rows()
    }

    /// Enter on `typed` in the address: when it follows an extension's
    /// keyword, what Enter asks of that extension, which then acts as
    /// [`ExtensionRequest::Send`] asks.
    pub fn omnibox_enter(&mut self, typed: &str) -> Option<ExtensionRequest> {
        if !self.active {
            return None;
        }
        let (keyword, text) = self.addressed(typed)?;
        self.omnibox.entered(typed);
        Some(ExtensionRequest::Send(keyword.id, text))
    }
}
