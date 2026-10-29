//! The Emoji tab: the workspace's own emoji, which every app of the
//! workspace draws for `:name:`, each with its picture, its name and who
//! added it. Add emoji takes input for whoever the workspace's grants let
//! add one; Remove stands on each emoji the person may remove: their own,
//! or anyone's as a moderator. The shell decides both again.

use ui::icons::Icon;
use ui::widget::{Banner, Button, ButtonRole, ControlSize, Element, EmptyState, ListRow, Section};

use super::agents::column;
use super::general::{button, buttons, caption};
use super::{Parts, Settings, WorkspaceSettingsEffect};

/// An emoji's picture, decoded once by the shell.
#[derive(Clone)]
pub struct Picture {
    pub image: ui::vello::peniko::ImageData,
    /// Which decoding it is: two pictures with the same are the same.
    pub revision: u64,
}

impl PartialEq for Picture {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
    }
}

impl Eq for Picture {}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// One emoji of the workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Emoji {
    /// Its name, without colons.
    pub name: String,
    /// Who added it, as the person reads it.
    pub added_by: String,
    pub picture: Option<Picture>,
    /// Remove's name ("Remove :wave:"), where the person may remove it.
    pub remove: Option<String>,
}

/// Why the last emoji was not added or removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub detail: Option<String>,
}

/// Everything the tab shows, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmojiPane {
    pub count: String,
    pub help: String,
    pub add: String,
    /// Whether Add emoji takes a press: the person may add one, and no
    /// chooser is open already.
    pub can_add: bool,
    pub notice: Option<Notice>,
    pub emoji: Vec<Emoji>,
    /// What the tab says with no emoji: title and detail.
    pub empty: (String, String),
    /// Who may add emoji, for someone who may not.
    pub footnote: Option<String>,
}

/// The tab of `settings`.
pub(super) fn pane(settings: &Settings, parts: &Parts) -> Element {
    let pane = &settings.emoji;
    let mut groups: Vec<Element> = vec![
        Box::new(
            ListRow::new("ws-emoji-intro", pane.count.clone())
                .icon(Icon::Smile)
                .secondary(pane.help.clone())
                .two_line_secondary(),
        ),
        buttons(vec![
            button(parts, "ws-emoji-add", &pane.add, || {
                WorkspaceSettingsEffect::AddEmoji
            })
            .with_icon(Icon::SmilePlus)
            .with_enabled(pane.can_add),
        ]),
    ];
    if let Some(notice) = &pane.notice {
        let dismiss = parts.clone();
        let mut banner = Banner::error(notice.title.clone())
            .with_id("ws-emoji-notice")
            .dismissible()
            .on_dismiss(move || dismiss.ask(WorkspaceSettingsEffect::DismissEmojiNotice));
        if let Some(detail) = &notice.detail {
            banner = banner.with_message(detail.clone());
        }
        groups.push(Box::new(banner));
    }
    if pane.emoji.is_empty() {
        let (title, detail) = &pane.empty;
        groups.push(Box::new(
            EmptyState::new(title.clone())
                .with_icon(Icon::Smile)
                .with_message(detail.clone())
                .inline(),
        ));
    } else {
        groups.push(Box::new(Section::new(
            pane.emoji.iter().map(|emoji| row(emoji, parts)).collect(),
        )));
    }
    if let Some(note) = &pane.footnote {
        groups.push(caption(note));
    }
    column(groups)
}

/// One emoji: its picture, `:name:`, who added it, and Remove where the
/// person may.
fn row(emoji: &Emoji, parts: &Parts) -> Element {
    let mut row = ListRow::new(
        format!("ws-emoji-{}", emoji.name),
        format!(":{}:", emoji.name),
    )
    .secondary(emoji.added_by.clone());
    if let Some(picture) = &emoji.picture {
        row = row.thumbnail(picture.image.clone());
    }
    if let Some(remove) = &emoji.remove {
        let (press, name) = (parts.clone(), emoji.name.clone());
        row = row.trailing(Box::new(
            Button::icon_only(
                &format!("ws-emoji-remove-{}", emoji.name),
                Icon::Trash2,
                remove,
            )
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small)
            .on_click(move || {
                press.ask(WorkspaceSettingsEffect::RemoveEmoji { name: name.clone() })
            }),
        ));
    }
    Box::new(row)
}

/// The tab as a workspace's editor sees it, for captures: one emoji of
/// their own and one of someone else's.
pub(super) fn sample() -> EmojiPane {
    EmojiPane {
        count: "2 emoji".to_owned(),
        help: "Type :name: anywhere in the workspace to use one.".to_owned(),
        add: "Add emoji".to_owned(),
        can_add: true,
        notice: None,
        emoji: vec![
            Emoji {
                name: "party-parrot".to_owned(),
                added_by: "Added by you".to_owned(),
                picture: None,
                remove: Some("Remove :party-parrot:".to_owned()),
            },
            Emoji {
                name: "shipit".to_owned(),
                added_by: "Added by Ada Lovelace-Byron".to_owned(),
                picture: None,
                remove: None,
            },
        ],
        empty: (
            "No emoji yet".to_owned(),
            "Add a picture and everyone here can use it.".to_owned(),
        ),
        footnote: None,
    }
}

/// The tab after a picture could not be added, as a viewer sees it.
pub(super) fn sample_refused() -> EmojiPane {
    EmojiPane {
        can_add: false,
        notice: Some(Notice {
            title: "cat.png was not added".to_owned(),
            detail: Some("The picture is too detailed to keep as an emoji.".to_owned()),
        }),
        emoji: Vec::new(),
        footnote: Some("Editors can add emoji.".to_owned()),
        ..sample()
    }
}
