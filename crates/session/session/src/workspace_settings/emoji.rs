//! The Emoji tab: the workspace's own emoji, which every app of the
//! workspace draws for `:name:`.
//!
//! They are one record of the room, kept by Chat
//! ([`tilo_app::tilo_workspace_emoji::OWNER`]) and opened here the way every
//! app of the room opens it ([`WorkspaceRecord`]): in the store this device's
//! apps keep, as the person sitting here. Adding and removing are the
//! record's own actions, decided by the room's grants on Chat's node — the
//! decision the app windows and the action broker take — so the tab, the
//! apps and the person's other devices hold the one record, and the tab
//! decides nothing of its own.
//!
//! The picture is chosen in the session's own file chooser
//! ([`FilePickerSeat`]) and added from what it answers, as a window's emoji
//! picker adds one.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;

use tilo_action_policy::Standing;
use tilo_app::framework::WorkspaceRecord;
use tilo_app::tilo_workspace_emoji::{
    EmojiImage, OWNER, WorkspaceEmoji as Record, workspace_emoji as actions,
};
use tilo_desktop::collaborators::PeopleNames;
use tilo_desktop::workspace_settings::{self as domain, emoji_pane};
use tilo_desktop_model::{DesktopTree, UserId, Workspace};
use tilo_desktop_surface::i18n::{t, t_args};
use tilo_sheets::Seat as _;
use tilo_sheets::file_picker::{FileAnswer, FileMode, FilePickerSeat, FileRequest, FileType};
use tilo_sheets::workspace_settings as sheet;
use ui::permissions::{LivePermissions, PermissionContext};
use ui::record::RecordOwners;
use ui::record::workspace::WorkspaceRecords;
use ui::workspace_emoji::{CustomEmoji, Small, WorkspaceEmoji, make_small, wake_when_tools_are_in};

/// How workspace settings' history names where a change came from.
const SETTINGS_APP: (&str, &str) = ("tilo-session", "Workspace settings");

/// The record's action adding an emoji.
const ADD: &str = emoji_pane::ADD;
/// The record's action removing an emoji one added oneself.
const REMOVE_OWN: &str = emoji_pane::REMOVE_OWN;
/// The record's action removing anyone's emoji, as moderation.
const REMOVE_ANY: &str = emoji_pane::REMOVE_ANY;

/// The most a chosen file may be before it is made small: a file past this
/// is not a picture an emoji keeps.
const MAX_CHOSEN_BYTES: u64 = 8 * 1024 * 1024;

/// The pictures an emoji keeps, as the chooser lists them.
const PICTURES: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// The workspace's emoji as the Emoji tab holds them.
pub(crate) struct Emoji {
    /// Where this device keeps its apps' records: the one store Chat keeps
    /// the record in too, which every app of the room opens.
    owners: RecordOwners,
    /// Wakes the session's loop when another replica's change arrives, and
    /// when the image tools a chosen picture waits for are in.
    wake: ui::WakeScope,
    /// The record, once the tab showed it: the workspace, the person and the
    /// grants it was opened for.
    open: RefCell<Option<Open>>,
    /// What the tab last drew: the record's revision and what this said of
    /// it. The tab is shown again at each change.
    shown: RefCell<Option<Seen>>,
    /// Why the last emoji was not added or removed.
    notice: RefCell<Option<emoji_pane::EmojiNotice>>,
    /// Chosen pictures whose making small waits for the image tools.
    waiting: RefCell<Vec<Waiting>>,
    /// The chooser's request while the person picks a picture.
    choosing: RefCell<Option<String>>,
    /// Counts what this changed itself: the notice, the chooser opening or
    /// closing, a picture waiting for the image tools.
    version: Cell<u64>,
    /// Counts the chooser's requests, so each one names itself.
    requests: Cell<u64>,
}

/// The record as the tab opened it.
struct Open {
    room: String,
    person: ui::permissions::UserId,
    /// The grants it was opened with: they decide its actions, so a
    /// workspace whose grants changed opens the record again.
    policy: tilo_action_policy::RoomPolicy,
    /// `None` where the record could not be opened here.
    record: Option<WorkspaceRecord<Record>>,
}

/// What the tab drew at one frame.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Seen {
    revision: u64,
    version: u64,
}

/// A chosen picture whose making small waits for the image tools.
struct Waiting {
    /// The file's name, as the tab names what it could not add.
    file: String,
    bytes: Vec<u8>,
}

impl Emoji {
    /// The workspace's emoji in the store rooted at `root`, waking the
    /// session's loop through `wake` when a change or the image tools land.
    pub(crate) fn new(root: PathBuf, wake: tilo_service::Wake) -> Self {
        let mut scope = ui::WakeScope::new();
        if let Err(error) = scope.bind(move |_| wake()) {
            tracing::warn!(%error, "the workspace's emoji are not woken");
        }
        Self {
            owners: RecordOwners::new(root),
            wake: scope,
            open: RefCell::default(),
            shown: RefCell::default(),
            notice: RefCell::default(),
            waiting: RefCell::default(),
            choosing: RefCell::default(),
            version: Cell::new(0),
            requests: Cell::new(0),
        }
    }

    /// The Emoji tab as it stands for `person`: `tree`'s workspace in front,
    /// and what each person is called.
    pub(crate) fn pane(
        &self,
        tree: &DesktopTree,
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

    /// The tab with nothing to show here: no workspace in front, or its
    /// emoji could not be opened on this device.
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
    pub(crate) fn moved(&self, tree: &DesktopTree, person: &UserId) -> bool {
        let Some(workspace) = tree.active_workspace() else {
            return false;
        };
        let held = self.open.borrow();
        let Some(open) = held
            .as_ref()
            .filter(|open| open.room == workspace.room().to_string())
            .filter(|open| open.person.0 == *person)
        else {
            return false;
        };
        let revision = open.record.as_ref().map_or(0, WorkspaceRecord::revision);
        let now = Seen {
            revision,
            version: self.version.get(),
        };
        self.shown.borrow().as_ref() != Some(&now)
    }

    /// The record of `workspace` as `person`, opened here when it is not
    /// yet, and opened again when the workspace's grants changed, so its
    /// decisions follow them.
    fn record(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
    ) -> Option<WorkspaceRecord<Record>> {
        let workspace = workspace?;
        let room = workspace.room().to_string();
        let author = ui::permissions::UserId(person.clone());
        let held = self.open.borrow();
        let current = held.as_ref().filter(|open| {
            open.room == room && open.person == author && open.policy == workspace.policy
        });
        if let Some(open) = current {
            return open.record.clone();
        }
        drop(held);
        let context = PermissionContext::from_standing(
            &room,
            Standing::person(workspace.policy.clone(), person.clone()),
        )
        .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
        let records = WorkspaceRecords::in_room(self.owners.clone(), room.clone(), author.clone());
        let record = WorkspaceRecord::<Record>::open(
            &records,
            OWNER,
            SETTINGS_APP,
            Some(LivePermissions::fixed(context)),
            Some(self.wake.handle()),
        );
        *self.open.borrow_mut() = Some(Open {
            room,
            person: author,
            policy: workspace.policy.clone(),
            record: record.clone(),
        });
        record
    }

    /// How `person` may remove an emoji `added_by` added: their own by the
    /// plain removal, anyone's as a moderator, when the grants let them.
    fn would_remove(
        record: &WorkspaceRecord<Record>,
        added_by: &str,
        me: &str,
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
        tree: &DesktopTree,
        person: &UserId,
        picker: Option<&FilePickerSeat>,
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

    /// Open the chooser for a picture to add, when the room's grants let the
    /// person add one and no other sheet is asking for a file.
    fn choose(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
        picker: Option<&FilePickerSeat>,
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
        let lookup = ui::i18n::I18nManager::lookup_plain;
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

    /// Remove the emoji `name` as the room's grants let the person: their
    /// own by the plain removal, anyone's as a moderator.
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
            .map(|error| emoji_pane::EmojiNotice::not_removed(name, error.to_string()));
        self.say(refusal);
    }

    /// Take up what came of the chooser, and the pictures that waited for
    /// the image tools: each one added, or the tab saying why not.
    pub(crate) fn settle(
        &self,
        tree: &DesktopTree,
        person: &UserId,
        picker: Option<&FilePickerSeat>,
    ) {
        let workspace = tree.active_workspace();
        self.answer(workspace, person, picker);
        self.retry(workspace, person);
    }

    /// Take up the chooser's answer, once the person gave one.
    fn answer(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
        picker: Option<&FilePickerSeat>,
    ) {
        let Some(handle) = self.choosing.borrow().clone() else {
            return;
        };
        let Some(picker) = picker else {
            return;
        };
        // Another sheet asked the chooser since: this choice is over, and
        // what it answered is not ours to take.
        if picker
            .request()
            .is_none_or(|request| request.handle != handle)
        {
            self.choosing.borrow_mut().take();
            self.waiting.borrow_mut().clear();
            self.bump();
            return;
        }
        let Some(answer) = picker.take_answer() else {
            return;
        };
        self.choosing.borrow_mut().take();
        picker.ask(None);
        picker.presentation().close();
        picker.presentation().gone();
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
            Ok(_) => std::fs::read(path).ok(),
            Err(_) => None,
        };
        let Some(bytes) = bytes else {
            self.say(Some(emoji_pane::EmojiNotice::not_chosen(t(
                "workspace-settings-emoji-unreadable",
            ))));
            return;
        };
        let small = make_small(&bytes);
        if small == Small::Pending {
            self.waiting.borrow_mut().push(Waiting { file, bytes });
            wake_when_tools_are_in(&self.wake.handle());
            self.bump();
            return;
        }
        if let Err(why) = self.add_small(workspace, person, &file, &small) {
            tracing::warn!(file = %file, why = ?why.detail, "the chosen picture was not added as an emoji");
            self.say(Some(why));
        }
    }

    /// Make small again the pictures that waited for the image tools, and
    /// add the ones that are pictures now.
    fn retry(&self, workspace: Option<&Workspace>, person: &UserId) {
        let Some(workspace) = workspace else {
            return;
        };
        if self.waiting.borrow().is_empty() {
            return;
        }
        let mut left = Vec::new();
        let mut refusal = None;
        for waiting in std::mem::take(&mut *self.waiting.borrow_mut()) {
            let small = make_small(&waiting.bytes);
            if small == Small::Pending {
                left.push(waiting);
                continue;
            }
            if let Err(why) = self.add_small(workspace, person, &waiting.file, &small) {
                tracing::warn!(file = %waiting.file, why = ?why.detail, "the chosen picture was not added as an emoji");
                refusal = Some(why);
            }
        }
        if !left.is_empty() {
            wake_when_tools_are_in(&self.wake.handle());
        }
        *self.waiting.borrow_mut() = left;
        if let Some(refusal) = refusal {
            self.say(Some(refusal));
        }
    }

    /// Add `small`, the picture of `file`, as one of the workspace's emoji.
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
fn added_by(names: &PeopleNames, workspace: &Workspace, me: &str, id: &str) -> String {
    if id == me {
        return t("workspace-settings-emoji-added-by-you");
    }
    t_args(
        "workspace-settings-emoji-added-by",
        &[(
            "name",
            domain::member_row_name(names, workspace, me, id).into(),
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
