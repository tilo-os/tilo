//! `conversation`: what a conversation is made of, once for every app that has one.
//!
//! Chat and Messages both talk to people, and stay two apps because their primary object differs:
//! Chat's is a **channel** (a room with a history, a membership and a mood), Messages' is a
//! **person** (a private thread that may arrive by SMS, Signal or WhatsApp). Neither is a subset of
//! the other.
//!
//! What they do *not* differ in is the mechanics underneath — when a day starts, what time a line
//! was said, whether it arrived, what happens to the draft you were typing, which key sends, what
//! an attachment does when you touch it. Each app used to invent those separately, which is why the
//! same gesture behaved differently in the two windows. This module is that substrate, made of
//! small, separately usable pieces, because the two apps paint differently: Chat composes the whole
//! window immediately, Messages builds a widget tree.
//!
//! | Piece | What it settles |
//! |---|---|
//! | [`day`] | Day boundaries, the divider that marks one, the local clock a line carries |
//! | [`band`] | The band both of a conversation's dividers stand in: a centred caption, ruled out to both sides |
//! | [`delivery`] | One delivery-state vocabulary — the same words, glyphs and tones in both apps |
//! | [`attachment`] | The ladder an attachment opens through: preview → full size → save → open the saved copy |
//! | [`draft`] | A draft kept per conversation, and the line that shows it wherever the conversation is listed |
//! | [`composer`] | Enter sends, Shift+Enter breaks the line, Escape lets go — the same rule in both |
//! | [`list`] | [`ConversationList`]: the message list itself, for a widget-tree app |
//! | [`unread`] | Where the reader's unread lines start: the "New messages" divider |
//! | [`jump`] | The pill that takes the reader back to what is unread or to what is now |
//!
//! A host that adopts a piece keeps its own identity: a channel still shows authors and avatars, a
//! private thread still shows one person and their transport.

pub mod attachment;
pub mod band;
pub mod composer;
pub mod day;
pub mod delivery;
pub mod draft;
pub mod earlier;
pub mod jump;
pub mod list;
mod list_component;
pub use list_component::{
    ConversationEntryProps, ConversationListProps, DescribedConversationList,
};
pub mod unread;

pub use attachment::{
    AttachmentAction, AttachmentFace, AttachmentKind, AttachmentSummary, SavedCopy, UploadProgress,
    full_size_rect,
};
pub use composer::{ComposerIntent, ComposerRule};
pub use day::{
    ConversationClock, ConversationDay, DayBreaks, DayDivider, DayDividerFace, DayDividerProps,
    day_break_indices,
};
pub use delivery::{DeliveryFace, DeliveryState, DeliveryStep};
pub use draft::{DRAFT_PREVIEW_CHARS, DraftBook, line as draft_line, preview as draft_preview};
pub use earlier::EarlierUnavailableFace;
pub use jump::{Jump, JumpFace, pill_reserve};
pub use list::{
    ConversationLine, ConversationList, ConversationRole, ListCache, ListFace, ListWork,
    bubble_colors, run_breaks,
};
pub use unread::{UnreadDivider, UnreadDividerFace, UnreadDividerProps};

/// Resolve one of the conversation's labels in the desktop's language.
///
/// The words live in the toolkit's catalogue with every other widget's, one
/// namespace per module (`conversation-`), so a language that ships the toolkit
/// ships these too and an app can override any key in its own catalogue.
pub(crate) fn t(key: &str) -> String {
    crate::i18n::widget_format(key, &[])
}

#[cfg(test)]
mod tests;
