//! `conversation`: what a conversation is made of, once for every app that has one.

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

pub use attachment::{
    AttachmentAction, AttachmentFace, AttachmentKind, AttachmentSummary, SavedCopy, UploadProgress,
    full_size_rect,
};
pub use band::{
    DividerInk, DividerParts, DividerTone, SectionDivider, SectionDividerFace, SectionDividerProps,
    unread_label,
};
pub use composer::{ComposerIntent, ComposerRule};
pub use day::{ConversationClock, ConversationDay, DayBreaks, day_break_indices};
pub use delivery::{DeliveryFace, DeliveryState, DeliveryStep};
pub use draft::{DRAFT_PREVIEW_CHARS, DraftBook, line as draft_line, preview as draft_preview};
pub use earlier::EarlierUnavailableFace;
pub use jump::{Jump, JumpFace, pill_reserve};
pub use list::{
    ConversationLine, ConversationList, ConversationRole, ListCache, ListFace, ListWork,
    bubble_colors, run_breaks,
};

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
