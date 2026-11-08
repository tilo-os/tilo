//! The Emoji tab's words: the record's actions, how an emoji is removed, and what the person is
//! told when a picture is not added or an emoji is not removed. The workspace's emoji are one
//! record, kept by Chat and opened by every device the way the apps open it; the tab reads and acts
//! on that record, so nothing here decides anything.

use crate::i18n::{t, t_args};

/// The record's action adding an emoji.
pub const ADD: &str = "add-emoji";
/// The record's action removing an emoji one added oneself.
pub const REMOVE_OWN: &str = "remove-emoji";
/// The record's action removing anyone's emoji, as moderation.
pub const REMOVE_ANY: &str = "remove-any-emoji";

/// How an emoji is removed: one's own by the record's own removal, anyone else's as moderation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    Own,
    Moderation,
}

/// Something the person should know about the last add or remove: why it did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiNotice {
    pub title: String,
    pub detail: Option<String>,
}

impl EmojiNotice {
    /// No picture came from the chooser: why.
    #[must_use]
    pub fn not_chosen(why: impl Into<String>) -> Self {
        Self {
            title: t("workspace-settings-emoji-not-chosen"),
            detail: Some(why.into()),
        }
    }

    /// The chosen picture was not added: why.
    #[must_use]
    pub fn not_added(file: &str, why: impl Into<String>) -> Self {
        Self {
            title: t_args(
                "workspace-settings-emoji-not-added",
                &[("file", file.into())],
            ),
            detail: Some(why.into()),
        }
    }

    /// Nothing of the chosen file's name can name an emoji.
    #[must_use]
    pub fn unnamed(file: &str) -> Self {
        Self::not_added(file, t("workspace-settings-emoji-no-name"))
    }

    /// The chosen file was not added: the workspace's emoji are not available on this device.
    #[must_use]
    pub fn unavailable(file: &str) -> Self {
        Self::not_added(file, t("workspace-settings-emoji-unavailable"))
    }

    /// `name` was not removed: why.
    #[must_use]
    pub fn not_removed(name: &str, why: impl Into<String>) -> Self {
        Self {
            title: t_args(
                "workspace-settings-emoji-not-removed",
                &[("name", name.into())],
            ),
            detail: Some(why.into()),
        }
    }
}

/// Why a chosen picture could not be made one an emoji keeps, as the person reads it.
#[must_use]
pub fn small_refusal(small: &ui::workspace_emoji::Small) -> Option<String> {
    use ui::workspace_emoji::Small;
    match small {
        Small::Ready(_) => None,
        Small::Pending => Some(t("workspace-settings-emoji-tools-loading")),
        Small::NotAPicture => Some(t("workspace-settings-emoji-not-a-picture")),
        Small::TooLarge => Some(t("workspace-settings-emoji-too-detailed")),
    }
}
