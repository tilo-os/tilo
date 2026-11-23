//! The Emoji tab: the workspace's own emoji, which every app of the workspace draws for `:name:` .
//! They are one resource of the workspace, kept by Chat ([`tilo_app::tilo_workspace_emoji::OWNER`])
//! and followed here through the device's daemon ([`FollowedResource`]), as the person sitting
//! here. Adding and removing are the resource's own actions, which the daemon runs as the person
//! and decides by the workspace's grants on Chat's node, so the tab, the apps and the person's
//! other devices see the one resource, and the tab decides nothing of its own.
//! The picture is chosen in the session's own file chooser ([`FilePickerState`]) and added from what
//! it answers, as a window's emoji picker adds one.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;

use tilo_app::tilo_workspace_emoji::{
    EmojiImage, OWNER, WorkspaceEmoji as Record, workspace_emoji as actions,
};
use tilo_session_core::desktop::people::PeopleNames;
use tilo_session_core::desktop::workspace_settings::{self as domain, emoji_pane};
use tilo_desktop_model::{Layout, UserId, Workspace};
use tilo_session_client::daemon_documents::DaemonShell;
use tilo_session_core::desktop::i18n::{t, t_args};
use tilo_sheets::SheetState as _;
use tilo_sheets::file_picker::{FileAnswer, FileMode, FilePickerState, FileRequest, FileType};
use tilo_sheets::workspace_settings as sheet;
use ui::workspace_emoji::{CustomEmoji, Small, WorkspaceEmoji, make_small};

use crate::followed_resource::FollowedResource;

/// The record's action adding an emoji.
const ADD: &str = emoji_pane::ADD;
/// The record's action removing an emoji one added oneself.
const REMOVE_OWN: &str = emoji_pane::REMOVE_OWN;
/// The record's action removing anyone's emoji, as moderation.
const REMOVE_ANY: &str = emoji_pane::REMOVE_ANY;

/// The most a chosen file may be before it is made small: a file past this is not a picture an
/// emoji keeps.
const MAX_CHOSEN_BYTES: u64 = 8 * 1024 * 1024;

/// The pictures an emoji keeps, as the chooser lists them.
const PICTURES: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// The workspace's emoji as the Emoji tab holds them.
pub(crate) struct Emoji {
    /// The device's daemon, where the emoji are read and their actions run.
    daemon: DaemonShell,
    /// Wakes the session's loop when a change to the emoji arrives.
    wake: tilo_service::Wake,
    /// The emoji, once the tab showed them: the workspace, the person and the grants they were
    /// followed for.
    open: RefCell<Option<Open>>,
    /// What the tab last drew: the record's revision and what this said of it. The tab is shown
    /// again at each change.
    shown: RefCell<Option<Seen>>,
    /// Why the last emoji was not added or removed.
    notice: RefCell<Option<emoji_pane::EmojiNotice>>,
    /// The chooser's request while the person picks a picture.
    choosing: RefCell<Option<String>>,
    /// Counts what this changed itself: the notice, the chooser opening or closing.
    version: Cell<u64>,
    /// Counts the chooser's requests, so each one names itself.
    requests: Cell<u64>,
}

/// The emoji as the tab follows them.
struct Open {
    workspace: String,
    person: ui::permissions::MemberId,
    /// The grants they were followed with: a workspace whose grants changed has them followed
    /// again.
    policy: tilo_action_policy::WorkspacePolicy,
    record: FollowedResource<Record>,
}

/// What the tab drew at one frame.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Seen {
    revision: u64,
    version: u64,
}

impl Emoji {
    /// The workspace's emoji as the device's daemon reads them, waking the session's loop through
    /// `wake` when a change lands.
    pub(crate) fn new(daemon: DaemonShell, wake: tilo_service::Wake) -> Self {
        Self {
            daemon,
            wake,
            open: RefCell::default(),
            shown: RefCell::default(),
            notice: RefCell::default(),
            choosing: RefCell::default(),
            version: Cell::new(0),
            requests: Cell::new(0),
        }
    }

    /// The Emoji tab as it stands for `person`: `tree`'s workspace in front,
    /// and what each person is called.
    pub(crate) fn pane(
        &self,
        tree: &Layout,
        person: &UserId,
        names: &PeopleNames,
    ) -> sheet::EmojiPane {
        let Some(workspace) = tree.active_workspace() else {
            return self.not_available();
        };
        let Some(record) = self.record(Some(workspace), person) else {
            return self.not_available();
        };
        let revision = record.revision();
        let state = record.get();
        let me = record.me();
        let may_add = record.may(ADD);
        let pictures = WorkspaceEmoji::new(
            state
                .emoji
                .iter()
                .map(|(name, emoji)| CustomEmoji {
                    name: name.clone(),
                    image: Arc::from(emoji.image.bytes()),
                })
                .collect(),
        );
        let entries: Vec<sheet::Emoji> = state
            .emoji
            .iter()
            .map(|(name, added)| sheet::Emoji {
                name: name.clone(),
                added_by: added_by(names, workspace, me, &added.added_by),
                picture: pictures
                    .image(name)
                    .map(|image| sheet::Picture { image, revision }),
                remove: Self::would_remove(&record, &added.added_by, me).map(|_| {
                    t_args(
                        "workspace-settings-emoji-remove",
                        &[("name", name.as_str().into())],
                    )
                }),
            })
            .collect();
        let count = entries.len();
        *self.shown.borrow_mut() = Some(Seen {
            revision,
            version: self.version.get(),
        });
        sheet::EmojiPane {
            count: t_args("workspace-settings-emoji-count", &[("count", count.into())]),
            help: t("workspace-settings-emoji-help"),
            add: t("workspace-settings-emoji-add"),
            can_add: may_add && self.choosing.borrow().is_none(),
            notice: self.notice.borrow().as_ref().map(notice),
            emoji: entries,
            empty: (
                t("workspace-settings-emoji-empty-title"),
                if may_add {
                    t("workspace-settings-emoji-empty-detail")
                } else {
                    t("workspace-settings-emoji-empty-detail-viewer")
                },
            ),
            footnote: (!may_add).then(|| t("workspace-settings-emoji-editors-add")),
        }
    }

    /// The tab with nothing to show here: no workspace in front, or its emoji could not be opened
    /// on this device.
    fn not_available(&self) -> sheet::EmojiPane {
        *self.shown.borrow_mut() = Some(Seen {
            revision: 0,
            version: self.version.get(),
        });
        sheet::EmojiPane {
            count: t_args("workspace-settings-emoji-count", &[("count", 0.into())]),
            help: t("workspace-settings-emoji-help"),
            add: t("workspace-settings-emoji-add"),
            empty: (
                t("workspace-settings-emoji-empty-title"),
                t("workspace-settings-emoji-unavailable"),
            ),
            ..sheet::EmojiPane::default()
        }
    }

    /// Whether the tab would show something else now: the record moved, or
    /// what this says of it changed. `false` while the record is not open,
    /// which the tab does when it is first shown.
    pub(crate) fn moved(&self, tree: &Layout, person: &UserId) -> bool {
        let Some(workspace) = tree.active_workspace() else {
            return false;
        };
        let held = self.open.borrow();
        let Some(open) = held
            .as_ref()
            .filter(|open| open.workspace == workspace.id().to_string())
            .filter(|open| open.person.person_id() == Some(person.as_str()))
        else {
            return false;
        };
        let revision = open.record.revision();
        let now = Seen {
            revision,
            version: self.version.get(),
        };
        self.shown.borrow().as_ref() != Some(&now)
    }

    /// The emoji of `workspace` as `person`, followed here when they are not yet, and followed
    /// again when the workspace's grants changed, so what the tab offers follows them.
    fn record(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
    ) -> Option<FollowedResource<Record>> {
        let workspace = workspace?;
        let workspace_id = workspace.id().to_string();
        let author = ui::permissions::MemberId::person(person.clone());
        let held = self.open.borrow();
        let current = held.as_ref().filter(|open| {
            open.workspace == workspace_id
                && open.person == author
                && open.policy == workspace.policy
        });
        if let Some(open) = current {
            return Some(open.record.clone());
        }
        drop(held);
        let id = collab_types::workspace_document_id(tilo_app::tilo_workspace_emoji::KIND);
        let record = FollowedResource::<Record>::follow(
            &self.daemon,
            (&workspace_id, OWNER, &id),
            person.as_str(),
            Some(workspace.policy.clone()),
            Arc::clone(&self.wake),
        );
        *self.open.borrow_mut() = Some(Open {
            workspace: workspace_id,
            person: author,
            policy: workspace.policy.clone(),
            record: record.clone(),
        });
        Some(record)
    }

    /// How `person` may remove an emoji `added_by` added: their own by the plain removal, anyone's
    /// as a moderator, when the grants let them.
    fn would_remove(
        record: &FollowedResource<Record>,
        added_by: &tilo_app::tilo_resource::MemberId,
        me: &tilo_app::tilo_resource::MemberId,
    ) -> Option<emoji_pane::Removal> {
        if added_by == me && record.may(REMOVE_OWN) {
            return Some(emoji_pane::Removal::Own);
        }
        record
            .may(REMOVE_ANY)
            .then_some(emoji_pane::Removal::Moderation)
    }

    /// Take up what the person asked of the tab.
    pub(crate) fn asked(
        &self,
        effect: &sheet::WorkspaceSettingsEffect,
        tree: &Layout,
        person: &UserId,
        picker: Option<&FilePickerState>,
    ) {
        let workspace = tree.active_workspace();
        match effect {
            sheet::WorkspaceSettingsEffect::AddEmoji => self.choose(workspace, person, picker),
            sheet::WorkspaceSettingsEffect::RemoveEmoji { name } => {
                self.remove(workspace, person, name);
            }
            sheet::WorkspaceSettingsEffect::DismissEmojiNotice => self.say(None),
            _ => {}
        }
    }

    /// Open the chooser for a picture to add, when the workspace's grants let the person add one
    /// and no other sheet is asking for a file.
    fn choose(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
        picker: Option<&FilePickerState>,
    ) {
        let (Some(workspace), Some(picker)) = (workspace, picker) else {
            return;
        };
        let allowed = self
            .record(Some(workspace), person)
            .is_some_and(|record| record.may(ADD));
        if !allowed || self.choosing.borrow().is_some() || picker.request().is_some() {
            return;
        }
        self.say(None);
        let requests = self.requests.get().wrapping_add(1);
        self.requests.set(requests);
        let handle = format!("workspace-emoji-{requests}");
        let lookup = t;
        let home = tilo_paths::home();
        let start = home.clone().unwrap_or_else(|| PathBuf::from("/"));
        picker.ask(Some(FileRequest {
            handle: handle.clone(),
            title: lookup("workspace-emoji-choose-title"),
            mode: FileMode::Open,
            accept_label: Some(lookup("workspace-emoji-choose-accept")),
            filters: vec![FileType {
                label: lookup("workspace-emoji-image-files"),
                extensions: PICTURES
                    .iter()
                    .map(|extension| (*extension).to_owned())
                    .collect(),
            }],
            initial_dir: start.clone(),
            roots: vec![start],
            home,
            ..FileRequest::default()
        }));
        picker.presentation().open(false);
        *self.choosing.borrow_mut() = Some(handle);
        self.bump();
    }

    /// Remove the emoji `name` as the workspace's grants let the person: their own by the plain
    /// removal, anyone's as a moderator.
    fn remove(&self, workspace: Option<&Workspace>, person: &UserId, name: &str) {
        let Some(record) = self.record(workspace, person) else {
            return;
        };
        let Some(added_by) = record.get().get(name).map(|emoji| emoji.added_by.clone()) else {
            return;
        };
        let removal = match Self::would_remove(&record, &added_by, record.me()) {
            Some(emoji_pane::Removal::Own) => {
                Record::REMOVE_EMOJI.with(actions::RemoveEmoji { name: name.into() })
            }
            Some(emoji_pane::Removal::Moderation) => {
                Record::REMOVE_ANY_EMOJI.with(actions::RemoveAnyEmoji { name: name.into() })
            }
            None => return,
        };
        let refusal = record
            .invoke(&removal)
            .err()
            .map(|error| emoji_pane::EmojiNotice::not_removed(name, error));
        self.say(refusal);
    }

    /// Take up what came of the chooser: the picture added, or the tab saying why not.
    pub(crate) fn settle(&self, tree: &Layout, person: &UserId, picker: Option<&FilePickerState>) {
        self.answer(tree.active_workspace(), person, picker);
    }

    /// Take up the chooser's answer, once the person gave one.
    fn answer(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
        picker: Option<&FilePickerState>,
    ) {
        let Some(handle) = self.choosing.borrow().clone() else {
            return;
        };
        let Some(picker) = picker else {
            return;
        };
        // Another sheet asked the chooser since: this choice is over, and what it answered is not
        // ours to take.
        if picker
            .request()
            .is_none_or(|request| request.handle != handle)
        {
            self.choosing.borrow_mut().take();
            self.bump();
            return;
        }
        let Some(answer) = picker.take_answer() else {
            return;
        };
        self.choosing.borrow_mut().take();
        picker.presentation().close();
        self.bump();
        let FileAnswer::Chosen(paths) = &answer else {
            return;
        };
        for path in paths {
            self.add(workspace, person, path);
        }
    }

    /// Add the picture at `path` as one of the workspace's emoji.
    fn add(&self, workspace: Option<&Workspace>, person: &UserId, path: &std::path::Path) {
        let Some(workspace) = workspace else {
            return;
        };
        let file = path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let bytes = match std::fs::metadata(path) {
            Ok(metadata) if metadata.len() > MAX_CHOSEN_BYTES => {
                self.say(Some(emoji_pane::EmojiNotice::not_chosen(t(
                    "workspace-settings-emoji-too-large",
                ))));
                return;
            }
            Ok(_) => std::fs::read(path)
                .inspect_err(|error| {
                    tracing::warn!(%error, path = %path.display(), "a chosen emoji could not be read");
                    })
                .ok(),
            Err(_) => tilo_failure::ignored!(
                "the person is told the file cannot be read",
                None
            ),
        };
        let Some(bytes) = bytes else {
            self.say(Some(emoji_pane::EmojiNotice::not_chosen(t(
                "workspace-settings-emoji-unreadable",
            ))));
            return;
        };
        let small = make_small(&bytes);
        if let Err(why) = self.add_small(workspace, person, &file, &small) {
            tracing::warn!(file = %file, why = ?why.detail, "the chosen picture was not added as an emoji");
            self.say(Some(why));
        }
    }

    /// Add `small` , the picture of `file` , as one of the workspace's emoji.
    fn add_small(
        &self,
        workspace: &Workspace,
        person: &UserId,
        file: &str,
        small: &Small,
    ) -> Result<(), emoji_pane::EmojiNotice> {
        let Small::Ready(png) = small else {
            return Err(emoji_pane::EmojiNotice::not_added(
                file,
                emoji_pane::small_refusal(small).unwrap_or_default(),
            ));
        };
        let record = self
            .record(Some(workspace), person)
            .ok_or_else(|| emoji_pane::EmojiNotice::unavailable(file))?;
        let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
        let name = record
            .get()
            .free_name(stem)
            .ok_or_else(|| emoji_pane::EmojiNotice::unnamed(file))?;
        let added = record.invoke(&Record::ADD_EMOJI.with(actions::AddEmoji {
            name,
            image: EmojiImage(png.clone()).to_base64(),
        }));
        added.map_err(|error| emoji_pane::EmojiNotice::not_added(file, error.to_string()))
    }

    /// Say why the last emoji was not added or removed, or nothing.
    fn say(&self, notice: Option<emoji_pane::EmojiNotice>) {
        if *self.notice.borrow() == notice {
            return;
        }
        *self.notice.borrow_mut() = notice;
        self.bump();
    }

    /// Something the tab says changed: it is shown again on the next frame.
    fn bump(&self) {
        self.version.set(self.version.get().wrapping_add(1));
    }
}

/// Who added an emoji, as the person reads it.
fn added_by(
    names: &PeopleNames,
    workspace: &Workspace,
    me: &tilo_app::tilo_resource::MemberId,
    id: &tilo_app::tilo_resource::MemberId,
) -> String {
    if id == me {
        return t("workspace-settings-emoji-added-by-you");
    }
    t_args(
        "workspace-settings-emoji-added-by",
        &[(
            "name",
            domain::member_row_name(names, workspace, me.id(), id).into(),
        )],
    )
}

/// The tab's notice of what did not happen.
fn notice(notice: &emoji_pane::EmojiNotice) -> sheet::Notice {
    sheet::Notice {
        title: notice.title.clone(),
        detail: notice.detail.clone(),
    }
}
