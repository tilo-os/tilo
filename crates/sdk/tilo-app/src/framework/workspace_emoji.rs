//! The workspace's emoji as every framework window offers them to its views
//! ([`ui_core::workspace_emoji`]): the room's emoji record, held for the
//! window and registered against its wake scope, so `:name:` draws the same
//! picture in any app, natively and in a browser.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use tilo_workspace_emoji::{OWNER, WorkspaceEmoji as Record, workspace_emoji as actions};
use ui_core::workspace_emoji::{
    Adding, CustomEmoji, EmojiSource, Small, WorkspaceEmoji, make_small,
};

use super::workspace::{WorkspaceRecord, WorkspaceState};

// The record checks what the toolkit makes small: one limit for both.
const _: () =
    assert!(tilo_workspace_emoji::MAX_IMAGE_BYTES == ui_core::workspace_emoji::MAX_IMAGE_BYTES);
const _: () = assert!(tilo_workspace_emoji::MAX_EMOJI == ui_core::workspace_emoji::MAX_EMOJI);

/// The most a chosen file may be before it is made small.
const MAX_CHOSEN_BYTES: usize = 8 * 1024 * 1024;

/// A window's source of the workspace's emoji.
pub(crate) struct WindowEmoji {
    /// This source, for what settles after the call that asked for it.
    me: std::rc::Weak<Self>,
    state: Rc<WorkspaceState>,
    /// The record, once a view asked for it: a window that draws no emoji
    /// opens none.
    record: RefCell<Option<WorkspaceRecord<Record>>>,
    /// The emoji as last read, with the record's revision then.
    read: RefCell<Option<(u64, Rc<WorkspaceEmoji>)>>,
    /// Pictures the person chose, waiting for the chooser's answer.
    choosing: RefCell<Vec<ui_core::portal::ContentsChannel>>,
    /// Pictures chosen whose making small waits for the image tools.
    waiting: RefCell<Vec<ui_core::portal::PickedContents>>,
    /// What became of the last picture chosen, and how many were settled.
    added: RefCell<(u64, Option<Adding>)>,
}

impl WindowEmoji {
    pub(crate) fn new(state: Rc<WorkspaceState>) -> Rc<Self> {
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            state,
            record: RefCell::default(),
            read: RefCell::default(),
            choosing: RefCell::default(),
            waiting: RefCell::default(),
            added: RefCell::default(),
        })
    }

    fn record(&self) -> Option<WorkspaceRecord<Record>> {
        if let Some(record) = self.record.borrow().as_ref() {
            return Some(record.clone());
        }
        let record = self.state.open::<Record>(OWNER)?;
        *self.record.borrow_mut() = Some(record.clone());
        Some(record)
    }

    /// Take up what the chooser answered and add each picture chosen: once
    /// a frame, from the window's refresh. The chooser's answer and the
    /// image tools' arrival each wake the window, so the refresh that takes
    /// them up comes without anything else changing in it.
    pub(crate) fn settle(&self) {
        if self.choosing.borrow().is_empty() && self.waiting.borrow().is_empty() {
            return;
        }
        let mut chosen = std::mem::take(&mut *self.waiting.borrow_mut());
        self.choosing
            .borrow_mut()
            .retain(|channel| match channel.try_recv() {
                Ok(ui_core::portal::ContentsOutcome::Picked(files)) => {
                    chosen.extend(files);
                    false
                }
                Ok(ui_core::portal::ContentsOutcome::Failed(error)) => {
                    tracing::warn!(%error, "no picture was chosen for an emoji");
                    self.settled(Adding::Failed(error));
                    false
                }
                Ok(ui_core::portal::ContentsOutcome::Cancelled)
                | Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
            });
        for file in chosen {
            let small = make_small(&file.bytes);
            if small == Small::Pending {
                // Asked again on each refresh that finds them missing: a
                // failed fetch wakes too, and the next asks for them anew.
                if let Some(window) = self.state.wake() {
                    ui_core::workspace_emoji::wake_when_tools_are_in(window);
                }
                self.waiting.borrow_mut().push(file);
                continue;
            }
            let stem = file
                .name
                .rsplit_once('.')
                .map_or(file.name.as_str(), |(stem, _)| stem);
            let Some((record, name)) = self
                .record()
                .and_then(|record| Some((record.clone(), record.get().free_name(stem)?)))
            else {
                self.failed(&file.name, unavailable());
                continue;
            };
            let Small::Ready(png) = small else {
                let refusal = small.refusal();
                self.failed(&file.name, refusal.unwrap_or_default());
                continue;
            };
            // The record's store may be in use by whatever runs this
            // refresh: the emoji is added once it is free, and settled then.
            let me = self.me.clone();
            let file = file.name;
            record.invoke_then(&add_emoji(&name, png), move |added| {
                let Some(me) = me.upgrade() else {
                    return;
                };
                match added {
                    Ok(()) => me.settled(Adding::Added(name)),
                    Err(error) => me.failed(&file, error.to_string()),
                }
            });
        }
    }

    fn failed(&self, file: &str, error: String) {
        tracing::warn!(%error, %file, "the chosen picture was not added as an emoji");
        self.settled(Adding::Failed(error));
    }

    fn settled(&self, adding: Adding) {
        let mut added = self.added.borrow_mut();
        *added = (added.0.wrapping_add(1), Some(adding));
    }

    fn add_small(&self, name: &str, small: Small) -> Result<(), String> {
        let Small::Ready(png) = small else {
            return Err(small
                .refusal()
                .unwrap_or_else(|| "the picture was not made small".into()));
        };
        let record = self.record().ok_or_else(unavailable)?;
        record
            .invoke(&add_emoji(name, png))
            .map_err(|error| error.to_string())
    }
}

impl EmojiSource for WindowEmoji {
    fn emoji(&self) -> Rc<WorkspaceEmoji> {
        let Some(record) = self.record() else {
            return Rc::default();
        };
        // Read as view state first: a view drawing them is built again
        // when they change.
        let state = record.get();
        let revision = record.revision();
        if let Some((read, emoji)) = self.read.borrow().as_ref()
            && *read == revision
        {
            return Rc::clone(emoji);
        }
        let emoji = Rc::new(WorkspaceEmoji::new(
            state
                .emoji
                .iter()
                .map(|(name, emoji)| CustomEmoji {
                    name: name.clone(),
                    image: Arc::from(emoji.image.bytes()),
                })
                .collect(),
        ));
        *self.read.borrow_mut() = Some((revision, Rc::clone(&emoji)));
        emoji
    }

    fn revision(&self) -> u64 {
        self.record().map_or(0, |record| record.revision())
    }

    fn can_manage(&self) -> bool {
        self.record().is_some_and(|record| record.may("add-emoji"))
    }

    fn add(&self, name: &str, image: &[u8]) -> Result<(), String> {
        if !self.can_manage() {
            return Err(ui_core::i18n::I18nManager::lookup_plain(
                "workspace-emoji-not-allowed",
            ));
        }
        self.add_small(name, make_small(image))
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        let record = self.record().ok_or_else(unavailable)?;
        let state = record.get();
        let emoji = state
            .get(name)
            .ok_or_else(|| format!("there is no emoji :{name}:"))?;
        // One's own is removed as such; anyone else's is moderation, which
        // the room's roles decide.
        let removal = if emoji.added_by == record.me() {
            Record::REMOVE_EMOJI.with(actions::RemoveEmoji { name: name.into() })
        } else {
            Record::REMOVE_ANY_EMOJI.with(actions::RemoveAnyEmoji { name: name.into() })
        };
        record.invoke(&removal).map_err(|error| error.to_string())
    }

    fn choose_and_add(&self) {
        if !self.can_manage() {
            return;
        }
        let lookup = ui_core::i18n::I18nManager::lookup_plain;
        let request = ui_core::portal::PickRequest::open(
            &lookup("workspace-emoji-choose-title"),
            &lookup("workspace-emoji-choose-accept"),
        )
        .filter(
            ui_core::portal::FileFilter::new(&lookup("workspace-emoji-image-files"))
                .glob("*.png")
                .glob("*.jpg")
                .glob("*.jpeg")
                .glob("*.webp")
                .mimetype("image/png")
                .mimetype("image/jpeg")
                .mimetype("image/webp"),
        )
        .max_contents_bytes(MAX_CHOSEN_BYTES)
        .thread_name("workspace-emoji-choose");
        // The chooser wakes the window it was asked in when it answers:
        // this one, whichever window's code the press ran in.
        let _window = self
            .state
            .wake()
            .and_then(|window| window.enter_if_open().ok());
        self.choosing.borrow_mut().push(request.send_contents());
    }

    fn last_added(&self) -> (u64, Option<Adding>) {
        self.added.borrow().clone()
    }
}

/// The action adding `png` as the emoji `name`.
fn add_emoji(name: &str, png: Vec<u8>) -> tilo_resource::Invoke<Record> {
    Record::ADD_EMOJI.with(actions::AddEmoji {
        name: name.to_owned(),
        image: tilo_workspace_emoji::EmojiImage(png).to_base64(),
    })
}

/// Why nothing of the workspace's emoji can change: its record is not open.
fn unavailable() -> String {
    ui_core::i18n::I18nManager::lookup_plain("workspace-emoji-unavailable")
}

/// Register the workspace's emoji for the window whose wake scope `window`
/// names, held in `state`. The registry holds it weakly: the window keeps
/// the source, and what the window held goes with it.
pub(crate) fn register(
    window: &ui_core::wake::WakeHandle,
    state: Rc<WorkspaceState>,
) -> Rc<WindowEmoji> {
    let source = WindowEmoji::new(state);
    ui_core::workspace_emoji::register(
        window,
        Rc::new(Held(Rc::downgrade(&source))) as Rc<dyn EmojiSource>,
    );
    source
}

/// A window's source as the registry holds it: none once the window closed.
struct Held(std::rc::Weak<WindowEmoji>);

impl EmojiSource for Held {
    fn emoji(&self) -> Rc<WorkspaceEmoji> {
        self.0
            .upgrade()
            .map_or_else(Rc::default, |source| source.emoji())
    }

    fn revision(&self) -> u64 {
        self.0.upgrade().map_or(0, |source| source.revision())
    }

    fn can_manage(&self) -> bool {
        self.0.upgrade().is_some_and(|source| source.can_manage())
    }

    fn add(&self, name: &str, image: &[u8]) -> Result<(), String> {
        self.0
            .upgrade()
            .ok_or_else(|| "the window closed".to_owned())?
            .add(name, image)
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        self.0
            .upgrade()
            .ok_or_else(|| "the window closed".to_owned())?
            .remove(name)
    }

    fn choose_and_add(&self) {
        if let Some(source) = self.0.upgrade() {
            source.choose_and_add();
        }
    }

    fn last_added(&self) -> (u64, Option<Adding>) {
        self.0
            .upgrade()
            .map_or((0, None), |source| source.last_added())
    }
}
