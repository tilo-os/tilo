//! The clipboard's history: recording what is copied, publishing it to the
//! person's device panel, and pasting a chosen copy back into the app in
//! front.
//!
//! A copy is kept as one format's bytes, content-addressed, and written
//! through the device's settings document, so it survives the session: what
//! the person pinned is still there tomorrow, and nothing lives in a file
//! format of the clipboard's own.
//!
//! What was copied is personal, so the copies are published as a reading of
//! the clipboard's own service that only granted callers read: the panel
//! that lists them, and the session's own surfaces. Any other app, script or
//! agent is refused it as it is refused an action it has no grant for.
//!
//! Reading a selection never holds up the loop: the app that owns it answers
//! when it gets round to it, so a copy is taken as its bytes arrive
//! ([`Compositor::pull_selection`]). Neither does pasting: the copy goes onto
//! the clipboard and the app in front is told to paste with its own chord,
//! so every app pastes the way it expects (a terminal with Ctrl+Shift+V).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use std::path::Path;

use crate::platform::fabric::FabricLink;
use crate::platform::fabric::{ClipboardEntry, ClipboardPayload, DeviceId, FabricStamp};
use serde_json::{Map, Value};
use settings_store::clipboard_history::{HISTORY, StoredCopy, StoredHistory};
use settings_store::paste_shortcut::{self, PasteShortcut};
use tilo_actions::{ActionOutput, DoorError, ResourceSpec};
use tilo_compositor::clipboard_history::{Copy, History, Pull, Pulled, TEXT_MIME, kept_mime};
use tilo_compositor::{Compositor, InputEvent, Selection};
use tilo_quick_settings::hosted::ClipboardCopy;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use ui::dnd::CopiedFiles;

pub(crate) mod paste;
use crate::controls::CLIPBOARD_RESOURCE;
use crate::i18n::t;

/// The clipboard's history and the read of the selection filling it.
pub(crate) struct Clipboard {
    history: RefCell<History>,
    /// The selection read in flight: the change it was started for, what it offered, and the bytes
    /// so far.
    reading: RefCell<Option<Reading>>,
    /// The selection change the history last looked at.
    seen: Cell<u64>,
    /// When what the session itself put on the clipboard comes back off it,
    /// when it does: an invitation's link is a credential, and stays there
    /// only long enough to be pasted to the person it is for.
    kept_until: Cell<Option<Instant>>,
    /// Copies quick settings chose, waiting for the loop that has the compositor.
    asks: RefCell<Vec<Ask>>,
    /// Moves whenever what the history keeps changed: what a reader of its reading knows it has to
    /// read again.
    generation: Cell<u64>,
    /// The files the selection holds, when what was copied last is a list of files: what a paste
    /// into a folder takes.
    files: RefCell<Option<CopiedFiles>>,
    /// The paste of files in flight, on its own thread, or the paste a session that stopped left
    /// behind to be undone.
    paste: RefCell<Option<paste::Paste>>,
    /// Whether what was copied here since the last share still has to be offered to the person's
    /// other devices.
    unshared: Cell<bool>,
    /// When the fabric was last read. The fabric is a whole snapshot of every device, so it is read
    /// a few times a second rather than every turn of the loop, and at once when something was
    /// copied here.
    synced_at: Cell<Option<Instant>>,
    /// What wakes the session's loop when a paste on its own thread is over.
    wake: tilo_service::Wake,
}

/// One read of the selection.
struct Reading {
    change: u64,
    /// What the selection offered when the read started: a copy is judged by every format it
    /// offered, so a secret marker is seen.
    mimes: Vec<String>,
    pull: Pull,
}

/// What quick settings asked of the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Put a copy back on the clipboard and paste it into the app in front.
    Paste(String),
    /// Put `bytes` on the clipboard for the person to paste somewhere else,
    /// without keeping them, for as long as `kept_for` says.
    Copy {
        mime: String,
        bytes: Vec<u8>,
        kept_for: Duration,
    },
}

impl Clipboard {
    /// The history this device kept, newest first.
    pub(crate) fn new(wake: tilo_service::Wake) -> Self {
        let mut history = History::default();
        history.restore(HISTORY.get().kept().into_iter().filter_map(copy_of));
        Self {
            history: RefCell::new(history),
            reading: RefCell::new(None),
            seen: Cell::new(0),
            kept_until: Cell::new(None),
            asks: RefCell::new(Vec::new()),
            generation: Cell::new(0),
            files: RefCell::new(None),
            // A paste the last session was in the middle of is undone here.
            paste: RefCell::new(paste::Paste::unfinished(&wake)),
            unshared: Cell::new(true),
            synced_at: Cell::new(None),
            wake,
        }
    }

    /// Take the selection as the apps set it: start reading what it offers,
    /// and keep a copy once its bytes are all here.
    pub(crate) fn follow(&self, compositor: &mut Compositor) {
        self.take_back_when_due(compositor);
        self.finish_reading();
        if self.reading.borrow().is_some() {
            return;
        }
        let change = compositor.selection_changes(Selection::Clipboard);
        if change == self.seen.get() {
            return;
        }
        let mimes = compositor.selection_formats(Selection::Clipboard);
        let Some(mime) = kept_mime(&mimes) else {
            self.seen.set(change);
            // A secret or unkeepable copy takes the files with it: a paste must not paste what the
            // person copied before it.
            self.files.take();
            return;
        };
        match compositor.pull_selection(Selection::Clipboard, mime) {
            Ok(pull) => {
                *self.reading.borrow_mut() = Some(Reading {
                    change,
                    mimes,
                    pull,
                });
            }
            Err(error) => {
                tracing::warn!(%error, "the selection could not be read");
                self.seen.set(change);
            }
        }
    }

    /// Keep the copy of a read that finished.
    fn finish_reading(&self) {
        // The read is taken out while it is looked at: the history is saved below, and a borrow
        // held across that would be a second one.
        let finished = {
            let Some(mut reading) = self.reading.take() else {
                return;
            };
            match reading.pull.drain() {
                Pulled::Pending => {
                    *self.reading.borrow_mut() = Some(reading);
                    return;
                }
                Pulled::Ready(bytes) => Some((
                    reading.change,
                    std::mem::take(&mut reading.mimes),
                    reading.pull.mime().to_owned(),
                    bytes,
                )),
                Pulled::Lost => {
                    self.seen.set(reading.change);
                    self.files.take();
                    None
                }
            }
        };
        let Some((change, mimes, mime, bytes)) = finished else {
            return;
        };
        self.seen.set(change);
        let now = ui::time::unix_now_ms();
        let files = ui::dnd::is_file_mime(&mime)
            .then(|| ui::dnd::copied_files(&mime, &String::from_utf8_lossy(&bytes)))
            .filter(|files| !files.paths.is_empty());
        let kept = self.history.borrow_mut().record(&mimes, &mime, bytes, now);
        *self.files.borrow_mut() = files;
        if kept {
            self.save();
        }
    }

    /// The copies the history keeps, newest first, as its reading lists them: what the person can
    /// paste again, without their bytes.
    #[must_use]
    pub(crate) fn copies(&self) -> Vec<ClipboardCopy> {
        self.history
            .borrow()
            .copies()
            .map(|copy| ClipboardCopy {
                key: copy.key(),
                mime: copy.mime.clone(),
                // A copy of files is named by its first file, so the row says what was copied
                // rather than only that it was files.
                text: copy.text().map(str::to_owned).or_else(|| first_name(copy)),
                created_ms: copy.created_ms,
                pinned: copy.pinned,
                from: copy.from.clone(),
            })
            .collect()
    }

    /// Which of the history's changes a reader has already read: moves whenever what the history
    /// keeps does.
    #[must_use]
    pub(crate) const fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// Pin or unpin the copy `key` names. Returns whether the history changed.
    pub(crate) fn pin(&self, key: &str, pinned: bool) -> bool {
        let changed =
            Copy::id_of_key(key).is_some_and(|id| self.history.borrow_mut().pin(id, pinned));
        if changed {
            self.save();
        }
        changed
    }

    /// Remove the copy `key` names.
    pub(crate) fn remove(&self, key: &str) -> bool {
        let changed = Copy::id_of_key(key).is_some_and(|id| self.history.borrow_mut().remove(id));
        if changed {
            self.save();
        }
        changed
    }

    /// Clear the history, keeping what the person pinned. How many copies went.
    pub(crate) fn clear(&self) -> usize {
        let cleared = self.history.borrow_mut().clear();
        if cleared > 0 {
            self.save();
        }
        cleared
    }

    /// Paste the copied files into `destination` on a thread of its own, as `conflict` says when a
    /// name is taken: the staging, the syncing and the receipt are [`clipboard_files`]', and the
    /// loop only asks whether it is over ([`Self::step_files`]).
    pub(crate) fn paste_files(&self, destination: &Path, conflict: paste::Conflict) {
        if self.paste.borrow().is_some() {
            tracing::debug!("a paste of files is still going on");
            return;
        }
        let Some(files) = self.files.borrow().clone() else {
            tracing::debug!("the clipboard holds no files to paste");
            return;
        };
        let paste = paste::Paste::start(
            &files,
            destination,
            conflict,
            ui::time::unix_now_ms(),
            &self.wake,
        );
        if let Some(paste) = paste {
            *self.paste.borrow_mut() = Some(paste);
        }
    }

    /// What the paste that just ended has to tell the person, if anything.
    pub(crate) fn step_files(&self) -> Vec<crate::platform::notice::Notice> {
        let mut paste = self.paste.borrow_mut();
        if !paste.as_ref().is_some_and(paste::Paste::finished) {
            return Vec::new();
        }
        let Some(mut done) = paste.take() else {
            return Vec::new();
        };
        drop(paste);
        crate::notices::paste_notice(&done.outcome())
            .into_iter()
            .collect()
    }

    /// Share what was copied here with the person's other devices, and take what they copied there:
    /// `link` is the fabric, when this session runs on one. Returns whether the history changed.
    /// What another device shared joins the history marked with the device it came from, and is
    /// never offered back: a copy belongs to the device it was made on.
    pub(crate) fn sync_fabric(&self, link: &dyn FabricLink) -> bool {
        let Some((local, _)) = link.local_device_fabric_identity() else {
            return false;
        };
        let now = Instant::now();
        let due = self
            .synced_at
            .get()
            .is_none_or(|at| now.duration_since(at) >= SYNC_EVERY);
        if !due && !self.unshared.get() {
            return false;
        }
        self.synced_at.set(Some(now));
        let shared: Vec<ClipboardEntry> = link
            .device_fabric()
            .merged_clipboard()
            .entries()
            .cloned()
            .collect();
        let mut changed = false;
        {
            let mut history = self.history.borrow_mut();
            // Newest first, so the front of the history is what was copied last there too.
            for entry in shared.iter().rev() {
                if entry.origin == local {
                    continue;
                }
                let (mime, bytes) = payload_of(&entry.payload);
                let mut copy = Copy::new(&mime, bytes, entry.created_unix_ms);
                copy.pinned = entry.pinned;
                copy.from = Some(entry.origin.as_str().to_owned());
                changed |= history.import(copy);
            }
        }
        if changed {
            self.save();
        }
        if self.unshared.replace(false) {
            let entries = self.shareable(&local);
            if !entries.is_empty() {
                link.share_clipboard(entries);
            }
        }
        changed
    }

    /// What was copied here, as entries the fabric carries: a copy that came from another device is
    /// theirs, and a list of files names paths that only mean something on this device.
    fn shareable(&self, local: &DeviceId) -> Vec<ClipboardEntry> {
        let mut entries = Vec::new();
        for copy in self.history.borrow().copies() {
            if copy.from.is_some() {
                continue;
            }
            let payload = match copy.mime.as_str() {
                TEXT_MIME => match copy.text() {
                    Some(text) => ClipboardPayload::Text(text.to_owned()),
                    None => continue,
                },
                mime if SHARED_MIMES.contains(&mime) => ClipboardPayload::Binary {
                    mime: copy.mime.clone(),
                    bytes: copy.bytes.clone(),
                },
                _ => continue,
            };
            let Ok(mut entry) = ClipboardEntry::shareable(
                payload,
                local.clone(),
                copy.created_ms,
                FabricStamp {
                    counter: copy.created_ms,
                    device: local.clone(),
                },
            ) else {
                continue;
            };
            entry.pinned = copy.pinned;
            entries.push(entry);
        }
        entries
    }

    /// Queue what quick settings asked for, for the loop that has the compositor.
    pub(crate) fn ask(&self, ask: Ask) {
        self.asks.borrow_mut().push(ask);
    }

    /// Carry out what quick settings asked: put a copy back on the clipboard and paste it into the
    /// app in front, or put what a surface hands the person on it.
    pub(crate) fn serve_asks(&self, compositor: &mut Compositor) {
        for ask in std::mem::take(&mut *self.asks.borrow_mut()) {
            match ask {
                Ask::Paste(key) => {
                    self.paste(compositor, &key);
                }
                Ask::Copy {
                    mime,
                    bytes,
                    kept_for,
                } => {
                    self.copy_for_person(compositor, &mime, bytes, Some(kept_for));
                }
            }
        }
    }

    /// Put the copy `key` names on the clipboard and have the app in front paste it. False when
    /// there is no such copy.
    pub(crate) fn paste(&self, compositor: &mut Compositor, key: &str) -> bool {
        let offer = {
            let history = self.history.borrow();
            let Some(copy) = history.find(key) else {
                return false;
            };
            (copy.mime.clone(), copy.bytes.clone())
        };
        if let Err(error) = compositor.set_selection(Selection::Clipboard, vec![offer]) {
            tracing::warn!(%error, "the copy could not be put on the clipboard");
            return false;
        }
        // The session's own offer is not a copy the person made.
        self.seen
            .set(compositor.selection_changes(Selection::Clipboard));
        Self::paste_into_focus(compositor);
        true
    }

    /// Offer `mime` and `bytes` on the clipboard for the person to paste somewhere else, without
    /// keeping them: a dictated transcript lives in the dictation's private draft until they
    /// discard it, not in the clipboard's history, and an invitation's link is a credential.
    /// `kept_for` is how long a credential stays there before the session takes it back, when
    /// nothing of the person's has copied since. False when the clipboard did not take it.
    pub(crate) fn copy_for_person(
        &self,
        compositor: &mut Compositor,
        mime: &str,
        bytes: Vec<u8>,
        kept_for: Option<Duration>,
    ) -> bool {
        if let Err(error) =
            compositor.set_selection(Selection::Clipboard, vec![(mime.to_owned(), bytes)])
        {
            tracing::warn!(%error, "the transcript could not be put on the clipboard");
            return false;
        }
        // The session's own offer is not a copy the person made.
        self.seen
            .set(compositor.selection_changes(Selection::Clipboard));
        self.kept_until
            .set(kept_for.and_then(|kept| Instant::now().checked_add(kept)));
        true
    }

    /// Take back what the session put on the clipboard, once the time it was put there for is up
    /// and it is still what is there: a copy of the person's own made since keeps its place.
    fn take_back_when_due(&self, compositor: &mut Compositor) {
        let Some(due) = self.kept_until.get() else {
            return;
        };
        if Instant::now() < due {
            return;
        }
        self.kept_until.set(None);
        if compositor.selection_changes(Selection::Clipboard) != self.seen.get() {
            return;
        }
        if let Err(error) = compositor.set_selection(Selection::Clipboard, Vec::new()) {
            tracing::warn!(%error, "the link could not be taken off the clipboard");
            return;
        }
        self.seen
            .set(compositor.selection_changes(Selection::Clipboard));
    }

    /// Make the app in front paste, with the chord it pastes with. A hosted surface holding the
    /// keyboard takes the chord as it is: the app in front is a window, and the session's own
    /// surfaces are closed by then.
    fn paste_into_focus(compositor: &mut Compositor) {
        let Some(window) = compositor.focused_window() else {
            return;
        };
        let ids: Vec<&str> = window.app_id.as_deref().into_iter().collect();
        let section = settings_store::load_section(paste_shortcut::SECTION);
        let overrides = paste_shortcut::overrides_from_section(&section);
        let chord = if ids.is_empty() {
            PasteShortcut::CtrlV
        } else {
            paste_shortcut::resolve_ids(&ids, &overrides)
        };
        let (ctrl, shift) = match chord {
            PasteShortcut::CtrlV => (true, false),
            PasteShortcut::CtrlShiftV => (true, true),
        };
        let press = |compositor: &mut Compositor, keycode: u32, pressed: bool| {
            if let Err(error) = compositor.input(InputEvent::Key { keycode, pressed }) {
                tracing::warn!(%error, keycode, pressed, "the paste chord was not sent");
            }
        };
        if ctrl {
            press(compositor, KEY_LEFTCTRL, true);
        }
        if shift {
            press(compositor, KEY_LEFTSHIFT, true);
        }
        press(compositor, KEY_V, true);
        press(compositor, KEY_V, false);
        if shift {
            press(compositor, KEY_LEFTSHIFT, false);
        }
        if ctrl {
            press(compositor, KEY_LEFTCTRL, false);
        }
    }

    /// Write the history through this device's settings document.
    fn save(&self) {
        let stored = StoredHistory {
            copies: self
                .history
                .borrow()
                .copies()
                .map(|copy| {
                    StoredCopy::of(
                        &copy.mime,
                        &copy.bytes,
                        copy.created_ms,
                        copy.pinned,
                        copy.from.as_deref(),
                    )
                })
                .collect(),
        };
        self.unshared.set(true);
        self.generation.set(self.generation.get().wrapping_add(1));
        if let Err(error) = HISTORY.set(stored) {
            tracing::warn!(%error, "the clipboard history was not saved");
        }
    }
}

/// The clipboard as the session's services publish it: what the person can paste again, which only
/// a granted caller reads, and the action pasting copied files into a folder.
pub(crate) struct Served {
    clipboard: Rc<Clipboard>,
    /// The history's generation last reported as what its reading shows.
    reported: Cell<u64>,
}

impl Served {
    /// The service publishing `clipboard` .
    pub(crate) fn new(clipboard: Rc<Clipboard>) -> Self {
        Self {
            reported: Cell::new(clipboard.generation()),
            clipboard,
        }
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![clipboard_resource()]
    }

    /// The copies, newest first: personal, so only a grant the workspace gives reads them.
    fn read(&self, resource: &str) -> Option<Value> {
        (resource == CLIPBOARD_RESOURCE).then(|| reading(&self.clipboard.copies()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        match decode::<ClipboardAction>(&self.resources(), resource, action, arguments)? {
            ClipboardAction::Paste { copy } => self.clipboard.ask(Ask::Paste(copy)),
            ClipboardAction::Pin { copy, pinned } => {
                self.clipboard.pin(&copy, pinned);
            }
            ClipboardAction::Remove { copy } => {
                self.clipboard.remove(&copy);
            }
            ClipboardAction::Clear => {
                self.clipboard.clear();
            }
            ClipboardAction::PasteFiles {
                destination,
                conflict,
            } => {
                let Some(chosen) = paste::Conflict::parse(conflict.as_deref()) else {
                    return Err(DoorError::InvalidArguments(format!(
                        "{:?} is not a paste conflict policy: keep-both, skip or replace",
                        conflict.as_deref().unwrap_or_default()
                    )));
                };
                self.clipboard.paste_files(Path::new(&destination), chosen);
            }
        }
        Ok(Started::Done(ActionOutput::default()))
    }

    fn poll(&mut self) -> bool {
        let generation = self.clipboard.generation();
        if self.reported.get() == generation {
            return false;
        }
        self.reported.set(generation);
        true
    }
}

/// What the clipboard resource offers: pasting a copy again, what the person does with the history,
/// and pasting the copied files into a folder.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.clipboard")]
pub(crate) enum ClipboardAction {
    /// Put a copy back on the clipboard and paste it into the app in front
    #[effect(own_device)]
    Paste {
        /// The copy's key, as the history's rows carry it
        #[param("The copy")]
        copy: String,
    },
    /// Keep a copy at the front of the history, or let it go with the rest
    #[effect(own_device, idempotent)]
    Pin {
        /// The copy's key, as the history's rows carry it
        #[param("The copy")]
        copy: String,
        /// Whether to keep it
        #[param("Whether to keep it")]
        pinned: bool,
    },
    /// Remove one copy from the history
    #[effect(own_device, idempotent)]
    Remove {
        /// The copy's key, as the history's rows carry it
        #[param("The copy")]
        copy: String,
    },
    /// Clear the history, keeping what the person pinned
    #[effect(own_device, idempotent)]
    Clear,
    /// Paste the copied files into a folder, as a copy, or a move after a cut
    #[effect(own_device, label = "Paste files")]
    PasteFiles {
        /// The folder to paste them into
        #[param("The folder")]
        destination: String,
        /// What to do when a name is already there: keep both (the default), skip it, or put what
        /// is there in the Trash
        #[param("What a taken name gets")]
        conflict: Option<String>,
    },
}

/// What the person copied, and pasting it into a folder.
fn clipboard_resource() -> ResourceSpec {
    offer::<ClipboardAction>(
        ResourceSpec::new(
            CLIPBOARD_RESOURCE,
            tilo_action_policy::roles::SESSION_KIND,
            t("session-clipboard"),
        )
        .with_granted_reading(),
    )
}

/// How often the fabric is read when nothing was copied here.
const SYNC_EVERY: Duration = Duration::from_millis(500);

/// The formats that mean the same thing on every device: pictures and formatted text. A file list
/// is a copy of paths, which a paste on another device could not open.
const SHARED_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "text/html"];

/// What a shared item's format and bytes are here.
fn payload_of(payload: &ClipboardPayload) -> (String, Vec<u8>) {
    match payload {
        ClipboardPayload::Text(text) => (TEXT_MIME.to_owned(), text.as_bytes().to_vec()),
        ClipboardPayload::Binary { mime, bytes } => (mime.clone(), bytes.clone()),
    }
}

/// The first file a copy of files names.
fn first_name(copy: &Copy) -> Option<String> {
    if !ui::dnd::is_file_mime(&copy.mime) {
        return None;
    }
    let files = ui::dnd::copied_files(&copy.mime, copy.utf8()?);
    let name = files.paths.first()?.file_name()?;
    Some(name.to_string_lossy().into_owned())
}

/// The copy a stored one holds; `None` for one whose payload did not survive.
fn copy_of(stored: StoredCopy) -> Option<Copy> {
    let bytes = stored.payload();
    if bytes.is_empty() {
        return None;
    }
    let mut copy = Copy::new(&stored.mime, bytes, stored.created_ms);
    copy.pinned = stored.pinned;
    copy.from = stored.from;
    Some(copy)
}

/// evdev codes of the paste chord.
const KEY_LEFTCTRL: u32 = 29;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_V: u32 = 47;

/// Offer the files `files` names on the desktop's clipboard, as a file manager does: the copy a
/// person pastes into any app, and which the clipboard's next read takes in as their own copy.
pub(crate) fn offer_files(compositor: &mut Compositor, files: &CopiedFiles) {
    let offer = vec![
        (
            ui::dnd::COPIED_FILES_MIME.to_owned(),
            ui::dnd::copied_files_list(&files.paths, files.cut).into_bytes(),
        ),
        (
            ui::dnd::URI_LIST_MIME.to_owned(),
            ui::dnd::uri_list(&files.paths).into_bytes(),
        ),
    ];
    if let Err(error) = compositor.set_selection(Selection::Clipboard, offer) {
        tracing::warn!(%error, "the copied files could not be put on the clipboard");
    }
}
