//! Decks read from and kept as content: a `.pptx` the workspace holds (an
//! attachment, a shared file, another app's export) opened into a new deck,
//! and the deck kept as `.pptx` content another app or an agent reads,
//! attaches or sends, with no file or dialog in between.

use ui::framework::{EffectAnswer, EffectCx, Effected};

use super::store::{Ask, Store};
use super::{DeckRecord, MAX_PRESENTATION_FILE_BYTES};
use crate::actions;

impl Store {
    /// Replace a new, untouched deck with the `.pptx` at `content` (a
    /// `tilo.content:<reference>` address) and answer its outline. A deck someone
    /// wrote is never replaced: the import is refused instead.
    pub(super) fn import_pptx_content(
        &mut self,
        content: &str,
        cx: &EffectCx<DeckRecord>,
    ) -> EffectAnswer {
        let reference = ui::tilo_actions::named_reference(content).ok_or_else(|| {
            format!("`{content}` names no content: give `tilo.content:<reference>`")
        })?;
        if !cx.live().get().is_untouched() {
            return Err(
                "this deck has slides someone wrote, which an import would replace: open a new \
                 deck and import there"
                    .into(),
            );
        }
        let bytes = self
            .content
            .read(reference)
            .ok_or_else(|| format!("content `{reference}` is not on this device"))?;
        if bytes.len() > MAX_PRESENTATION_FILE_BYTES {
            return Err("the presentation is larger than 64 MiB".into());
        }
        let deck = crate::pptx::read_pptx(&bytes, &self.content)
            .map_err(|error| format!("content `{reference}` is not a PowerPoint deck: {error}"))?;
        cx.apply(&actions::replace_deck_action(&deck));
        self.ask(Ask::ShowOpenedDeck);
        Ok(Effected::value(crate::outline::of_deck(&deck)))
    }

    /// Keep the deck as a `.pptx` in the content store and answer its
    /// address.
    pub(super) fn export_pptx_content(&self, cx: &EffectCx<DeckRecord>) -> EffectAnswer {
        let deck = crate::deck::mirror(&cx.live().get());
        let bytes = crate::pptx::write_pptx(&deck, &self.content)
            .map_err(|error| format!("the deck could not be written: {error}"))?;
        let hash = self
            .content
            .put(&bytes)
            .map_err(|error| format!("the deck could not be kept: {error}"))?;
        Effected::content(&hash)
    }
}
