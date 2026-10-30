//! The clipboard's history: recording what is copied, feeding it to Quick
//! Settings, and pasting a chosen copy back into the app in front.
//!
//! A copy is kept as one format's bytes, content-addressed, and written
//! through the device's settings document, so it survives the session: what
//! the person pinned is still there tomorrow, and nothing lives in a file
//! format of the clipboard's own.
//!
//! Reading a selection never holds up the loop: the app that owns it answers
//! when it gets round to it, so a copy is taken as its bytes arrive
//! ([`Compositor::pull_selection`]). Neither does pasting: the copy goes onto
//! the clipboard and the app in front is told to paste with its own chord,
//! so every app pastes the way it expects (a terminal with Ctrl+Shift+V).

use std::cell::{Cell, RefCell};

use std::path::Path;

use settings_store::clipboard_history::{HISTORY, PASTE, StoredCopy, StoredHistory, StoredPaste};
use settings_store::paste_shortcut::{self, PasteShortcut};
use tilo_compositor::clipboard_history::{Copy, History, Pull, Pulled, kept_mime};
use tilo_compositor::{Compositor, InputEvent, Selection};
use tilo_quick_settings::hosted::ClipboardCopy;
use ui::dnd::CopiedFiles;

use super::clipboard_files;

/// The clipboard's history and the read of the selection filling it.
pub(crate) struct Clipboard {
    history: RefCell<History>,
    /// The selection read in flight: the change it was started for, what it
    /// offered, and the bytes so far.
    reading: RefCell<Option<Reading>>,
    /// The selection change the history last looked at.
    seen: Cell<u64>,
    /// Copies quick settings chose, waiting for the loop that has the
    /// compositor.
    asks: RefCell<Vec<Ask>>,
    /// The view last fed to quick settings, so an unchanged history is not
    /// fed again.
    fed: RefCell<Option<tilo_quick_settings::ClipboardView>>,
    /// The files the selection holds, when what was copied last is a list of
    /// files: what a paste into a folder takes.
    files: RefCell<Option<CopiedFiles>>,
    /// The paste of files in flight, as far as its journal got.
    paste: RefCell<Option<StoredPaste>>,
}

/// One read of the selection.
struct Reading {
    change: u64,
    /// What the selection offered when the read started: a copy is judged by
    /// every format it offered, so a secret marker is seen.
    mimes: Vec<String>,
    pull: Pull,
}

/// What quick settings asked of the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Put a copy back on the clipboard and paste it into the app in front.
    Paste(String),
}

impl Clipboard {
    /// The history this device kept, newest first.
    pub(crate) fn new() -> Self {
        let mut history = History::default();
        history.restore(HISTORY.get().kept().into_iter().filter_map(copy_of));
        Self {
            history: RefCell::new(history),
            reading: RefCell::new(None),
            seen: Cell::new(0),
            asks: RefCell::new(Vec::new()),
            fed: RefCell::new(None),
            files: RefCell::new(None),
            // A paste the last session was in the middle of is finished here.
            paste: RefCell::new(PASTE.get()),
        }
    }

    /// Take the selection as the apps set it: start reading what it offers,
    /// and keep a copy once its bytes are all here.
    pub(crate) fn follow(&self, compositor: &mut Compositor) {
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
            // A secret or unkeepable copy takes the files with it: a paste
            // must not paste what the person copied before it.
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
                tracing::debug!(%error, "the selection could not be read");
                self.seen.set(change);
            }
        }
    }

    /// Keep the copy of a read that finished.
    fn finish_reading(&self) {
        // The read is taken out while it is looked at: the history is saved
        // below, and a borrow held across that would be a second one.
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

    /// The history as quick settings lists it, as of `now_ms`.
    #[must_use]
    pub(crate) fn view(&self, now_ms: u64) -> tilo_quick_settings::ClipboardView {
        let copies: Vec<ClipboardCopy> = self
            .history
            .borrow()
            .copies()
            .map(|copy| ClipboardCopy {
                key: copy.key(),
                mime: copy.mime.clone(),
                // A copy of files is named by its first file, so the row
                // says what was copied rather than only that it was files.
                text: copy.text().map(str::to_owned).or_else(|| first_name(copy)),
                created_ms: copy.created_ms,
                pinned: copy.pinned,
                from: copy.from.clone(),
            })
            .collect();
        tilo_quick_settings::hosted::clipboard_view(&copies, now_ms)
    }

    /// The view to feed quick settings, when it changed since the last feed.
    pub(crate) fn changed_view(&self, now_ms: u64) -> Option<tilo_quick_settings::ClipboardView> {
        let view = self.view(now_ms);
        let mut fed = self.fed.borrow_mut();
        (*fed != Some(view.clone())).then(|| {
            *fed = Some(view.clone());
            view
        })
    }

    /// Pin or unpin the copy `key` names. Returns whether the history
    /// changed.
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

    /// Clear the history, keeping what the person pinned. How many copies
    /// went.
    pub(crate) fn clear(&self) -> usize {
        let cleared = self.history.borrow_mut().clear();
        if cleared > 0 {
            self.save();
        }
        cleared
    }

    /// Paste the copied files into `destination`, as their journal says:
    /// the plan is written down before the first file is touched, and the
    /// loop carries it out a file at a time ([`Self::step_files`]).
    pub(crate) fn paste_files(&self, destination: &Path) {
        if self.paste.borrow().is_some() {
            tracing::debug!("a paste of files is still going on");
            return;
        }
        let Some(files) = self.files.borrow().clone() else {
            tracing::debug!("the clipboard holds no files to paste");
            return;
        };
        let started = clipboard_files::begin(&files, destination, ui::time::unix_now_ms());
        *self.paste.borrow_mut() = started;
    }

    /// Carry out one file of the paste in flight, and write its journal.
    pub(crate) fn step_files(&self) {
        let mut paste = self.paste.borrow_mut();
        let Some(open) = paste.as_mut() else {
            return;
        };
        let done = clipboard_files::step(open);
        if done {
            *paste = None;
        }
        clipboard_files::save(paste.as_ref());
    }

    /// Queue what quick settings asked for, for the loop that has the
    /// compositor.
    pub(crate) fn ask(&self, ask: Ask) {
        self.asks.borrow_mut().push(ask);
    }

    /// Carry out what quick settings asked: put a copy back on the clipboard
    /// and paste it into the app in front.
    pub(crate) fn serve_asks(&self, compositor: &mut Compositor) {
        for ask in std::mem::take(&mut *self.asks.borrow_mut()) {
            match ask {
                Ask::Paste(key) => {
                    self.paste(compositor, &key);
                }
            }
        }
    }

    /// Put the copy `key` names on the clipboard and have the app in front
    /// paste it. False when there is no such copy.
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

    /// Make the app in front paste, with the chord it pastes with. A hosted
    /// surface holding the keyboard takes the chord as it is: the app in
    /// front is a window, and the session's own surfaces are closed by then.
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
                tracing::debug!(%error, keycode, pressed, "the paste chord was not sent");
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
        if let Err(error) = HISTORY.set(stored) {
            tracing::warn!(%error, "the clipboard history was not saved");
        }
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

/// The copy a stored one holds; `None` for one whose payload did not
/// survive.
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
