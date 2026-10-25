//! The workspace's emoji as every framework window offers them to its views
//! ([`ui_core::workspace_emoji`]): the room's emoji record, held for the
//! window and registered against its wake scope, so `:name:` draws the same
//! picture in any app, natively and in a browser.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use tilo_workspace_emoji::{OWNER, WorkspaceEmoji as Record, workspace_emoji as actions};
use ui_core::workspace_emoji::{CustomEmoji, EmojiSource, Small, WorkspaceEmoji, make_small};

use super::workspace::{WorkspaceRecord, WorkspaceState};

// The record checks what the toolkit makes small: one limit for both.
const _: () =
    assert!(tilo_workspace_emoji::MAX_IMAGE_BYTES == ui_core::workspace_emoji::MAX_IMAGE_BYTES);
const _: () = assert!(tilo_workspace_emoji::MAX_EMOJI == ui_core::workspace_emoji::MAX_EMOJI);

/// The most a chosen file may be before it is made small.
const MAX_CHOSEN_BYTES: usize = 8 * 1024 * 1024;

/// A window's source of the workspace's emoji.
pub(crate) struct WindowEmoji {
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
}

impl WindowEmoji {
    pub(crate) fn new(state: Rc<WorkspaceState>) -> Rc<Self> {
        Rc::new(Self {
            state,
            record: RefCell::default(),
            read: RefCell::default(),
            choosing: RefCell::default(),
            waiting: RefCell::default(),
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
    /// a frame, from the window's refresh.
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
                    false
                }
                Ok(ui_core::portal::ContentsOutcome::Cancelled)
                | Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
            });
        for file in chosen {
            let small = make_small(&file.bytes);
            if small == Small::Pending {
                self.waiting.borrow_mut().push(file);
                continue;
            }
            let stem = file
                .name
                .rsplit_once('.')
                .map_or(file.name.as_str(), |(stem, _)| stem);
            let added = self
                .record()
                .and_then(|record| record.get().free_name(stem))
                .ok_or_else(|| "the workspace's emoji are unavailable".to_owned())
                .and_then(|name| self.add_small(&name, small));
            if let Err(error) = added {
                tracing::warn!(%error, file = %file.name, "the chosen picture was not added as an emoji");
            }
        }
        // Neither the chooser's answer nor the image tools' arrival wakes
        // the window, and nothing else may change in it meanwhile: it asks
        // for the next refresh itself until the pictures are in.
        if !self.choosing.borrow().is_empty() || !self.waiting.borrow().is_empty() {
            self.state.wake();
        }
    }

    fn add_small(&self, name: &str, small: Small) -> Result<(), String> {
        let Small::Ready(png) = small else {
            return Err(small
                .refusal()
                .unwrap_or_else(|| "the picture was not made small".into()));
        };
        let record = self
            .record()
            .ok_or_else(|| "the workspace's emoji are unavailable".to_owned())?;
        record
            .invoke(&Record::ADD_EMOJI.with(actions::AddEmoji {
                name: name.to_owned(),
                image: tilo_workspace_emoji::EmojiImage(png).to_base64(),
            }))
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
            return Err("you may not add emoji to this workspace".into());
        }
        self.add_small(name, make_small(image))
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        let record = self
            .record()
            .ok_or_else(|| "the workspace's emoji are unavailable".to_owned())?;
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
        self.choosing.borrow_mut().push(request.send_contents());
        self.state.wake();
    }
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
}
