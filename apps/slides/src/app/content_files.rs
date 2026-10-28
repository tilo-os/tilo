//! Decks read from and kept as content: a `.pptx` the room holds (an
//! attachment, a shared file, another app's export) opened into a new deck,
//! and the deck kept as `.pptx` content another app or an agent reads,
//! attaches or sends, with no file or dialog in between.

use ui::framework::{EffectAnswer, EffectCx, Effected};

use super::{DeckRecord, MAX_PRESENTATION_FILE_BYTES, SlidesApp};
use crate::actions;

impl SlidesApp {
    /// Replace a new, untouched deck with the `.pptx` at `content` (a
    /// `tilo.content:<hash>` address) and answer its outline. A deck someone
    /// wrote is never replaced: the import is refused instead.
    pub(super) fn import_pptx_content(
        &mut self,
        content: &str,
        cx: &EffectCx<DeckRecord, Self>,
    ) -> EffectAnswer {
        let hash = ui::tilo_actions::content_hash(content.trim())
            // A plan that carried the content gives its bare hash.
            .or_else(|| {
                let bare = content.trim();
                (bare.len() == 64 && bare.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then_some(bare)
            })
            .ok_or_else(|| format!("`{content}` names no content: give `tilo.content:<hash>`"))?
            .to_ascii_lowercase();
        if !cx.live().get().is_untouched() {
            return Err(
                "this deck has slides someone wrote, which an import would replace: open a new \
                 deck and import there"
                    .into(),
            );
        }
        let bytes = self
            .content
            .read(&hash)
            .ok_or_else(|| format!("content `{hash}` is not on this device"))?;
        if bytes.len() > MAX_PRESENTATION_FILE_BYTES {
            return Err("the presentation is larger than 64 MiB".into());
        }
        let deck = crate::pptx::read_pptx(&bytes, &self.content)
            .map_err(|error| format!("content `{hash}` is not a PowerPoint deck: {error}"))?;
        cx.apply(&actions::replace_deck_action(&deck));
        self.show_opened_deck();
        Ok(Effected::value(crate::outline::of_deck(&deck)))
    }

    /// Keep the deck as a `.pptx` in the content store and answer its
    /// address.
    pub(super) fn export_pptx_content(&self, cx: &EffectCx<DeckRecord, Self>) -> EffectAnswer {
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
