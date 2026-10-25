//! `conversation`: what a conversation is made of, once for every app that has
//! one.
//!
//! Chat and Messages both talk to people, and they stay two apps because their
//! primary object differs: Chat's is a **channel** (a room with a history, a
//! membership and a mood) and Messages' is a **person** (a private thread that
//! may arrive by SMS, Signal or WhatsApp). Neither is a subset of the other.
//!
//! What they do *not* differ in is the mechanics underneath: when a day
//! starts, what time a line was said, whether it arrived, what happens to the
//! draft you were typing, which key sends, and what an attachment does when you
//! touch it. Each app used to invent those separately, which is why the same
//! gesture behaved differently in the two windows — Chat had a day divider for
//! the oldest loaded message and a clock with no date, Messages had no
//! timestamps at all and no keyboard.
//!
//! This module is that substrate. It is deliberately made of small, separately
//! usable pieces, because the two apps paint differently: Chat composes the
//! whole window immediately, Messages builds a widget tree.
//!
//! | Piece | What it settles |
//! |---|---|
//! | [`day`] | Day boundaries, the divider that marks one, the local clock a line carries |
//! | [`delivery`] | One delivery-state vocabulary — the same words, glyphs and tones in both apps |
//! | [`attachment`] | The ladder an attachment opens through: preview → full size → save → open the saved copy |
//! | [`draft`] | A draft kept per conversation, and the line that shows it wherever the conversation is listed |
//! | [`composer`] | Enter sends, Shift+Enter breaks the line, Escape lets go — the same rule in both |
//! | [`list`] | [`ConversationList`]: the message list itself, for a widget-tree app |
//!
//! A host that adopts a piece keeps its own identity: a channel still shows
//! authors and avatars, a private thread still shows one person and their
//! transport. What the person gets is a time on every message, a divider
//! whenever the day changes, one meaning for "delivered", a draft that is still
//! there tomorrow, and Enter that sends.

pub mod attachment;
pub mod composer;
pub mod day;
pub mod delivery;
pub mod draft;
pub mod list;

pub use attachment::{
    AttachmentAction, AttachmentFace, AttachmentKind, AttachmentSummary, SavedCopy, UploadProgress,
    full_size_rect,
};
pub use composer::{ComposerIntent, ComposerRule};
pub use day::{ConversationClock, ConversationDay, DayBreaks, DayDividerFace, day_break_indices};
pub use delivery::{DeliveryFace, DeliveryState, DeliveryStep};
pub use draft::{DRAFT_PREVIEW_CHARS, DraftBook, line as draft_line, preview as draft_preview};
pub use list::{
    ConversationLine, ConversationList, ConversationRole, ListFace, bubble_colors, run_breaks,
};

/// The module's own labels — the delivery vocabulary's words and the
/// attachment ladder's action names — in the toolkit's source language.
///
/// A toolkit module cannot put its strings in the crate's shared catalogue
/// without every widget paying for them, so they live beside the code that
/// paints them. An app may override any key in its own catalogue.
static CATALOG: crate::i18n::EmbeddedCatalog =
    crate::i18n::EmbeddedCatalog::new("en-US", &[("en-US", include_str!("locales/en-US.ftl"))])
        .with_system_locale();

/// Resolve one of this module's labels in the desktop's language.
pub(crate) fn t(key: &str) -> String {
    CATALOG.t(key)
}

#[cfg(test)]
mod tests;
