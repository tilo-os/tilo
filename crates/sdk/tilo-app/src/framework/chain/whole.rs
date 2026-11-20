//! Holding every part of a chain whose views list all of it
//! ([`Chained::HELD_WHOLE`](super::Chained::HELD_WHOLE)) without holding
//! up the window.
//!
//! Each earlier part this device keeps is decoded from its bytes as it is
//! opened: about a second for a part of a thousand issues on a page. A
//! window that opened every one of them as it was built showed nothing until
//! all were decoded, and a click in that time waited for all of it. A window
//! instead opens them a turn at a time: it builds with the head, and each
//! earlier part is opened in a later frame of its own, after the frames and
//! input waiting meanwhile, the newest first. Until then a part is waited for
//! as one on its way from the workspace ([`PartArrival::Arriving`]), and
//! arrives the same way: the view refreshes with it, and an action naming an
//! item it holds waits for it rather than being refused.
//!
//! A chain no window holds (a test's, a read answered without a window) has
//! no frames to take turns in, and opens them all at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ui_core::resource::ResourceOpenError;
use ui_core::wake::WakeHandle;

use super::{ChainCore, PartArrival};

/// The turns a window's chain opens its earlier parts in: a turn comes once
/// the window was woken for it, from a task after the one that asked, so
/// the frame that asked is drawn first.
pub(super) struct Turns {
    /// The window the chain is held in; `None` outside one.
    wake: Option<WakeHandle>,
    /// Whether a turn came since the last part opened.
    came: Arc<AtomicBool>,
    /// Whether a turn was asked for and has not come yet.
    asked: std::cell::Cell<bool>,
}

impl Turns {
    /// The turns of the window being built now, if one is.
    pub(super) fn of_running_window() -> Self {
        let wake = WakeHandle::new();
        Self {
            wake: (!wake.is_detached()).then_some(wake),
            came: Arc::default(),
            asked: std::cell::Cell::new(false),
        }
    }

    /// Whether the chain is held in a window, which takes turns.
    fn windowed(&self) -> bool {
        self.wake.as_ref().is_some_and(WakeHandle::is_active)
    }

    /// Take the turn that came, if one did.
    fn take(&self) -> bool {
        let came = self.came.swap(false, Ordering::AcqRel);
        if came {
            self.asked.set(false);
        }
        came
    }

    /// Ask for the next turn: the window is woken for it from a task of its
    /// own, after this one.
    fn ask(&self) {
        let Some(wake) = self.wake.clone() else {
            return;
        };
        if self.asked.replace(true) {
            return;
        }
        let came = Arc::clone(&self.came);
        let waking = wake.clone();
        let next = move || {
            came.store(true, Ordering::Release);
            waking.wake();
        };
        // Where nothing works in the background, the next turn comes with
        // the window's next frame.
        if ui_core::background::Background::current()
            .spawn("chain-turn", next)
            .is_err()
        {
            self.came.store(true, Ordering::Release);
            wake.wake();
        }
    }
}

impl ChainCore {
    /// Hold every part before the head that is neither held nor asked for:
    /// a chain whose views list all of it. In a window, a part this device
    /// keeps and has not opened waits for a turn of its own.
    pub(super) fn hold_earlier(&self) {
        let missing: Vec<u64> = {
            let parts = self.parts.borrow();
            let waiting = self.waiting.borrow();
            let Some(&head) = parts.keys().next_back() else {
                return;
            };
            (0..head)
                .filter(|index| !parts.contains_key(index) && !waiting.contains_key(index))
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        let kept = if self.turns.windowed() {
            self.workspace.store.held_parts(&self.chain)
        } else {
            Vec::new()
        };
        let mut queued = false;
        for index in missing {
            let checkout = self.checkout(index);
            if self.workspace.store.is_ready(&checkout.id) {
                if let Err(reason) = self.hold(index) {
                    tracing::warn!(kind = self.part.kind, part = index, %reason, "earlier part not held");
                }
                continue;
            }
            if kept.binary_search(&index).is_ok() {
                self.queued.borrow_mut().insert(index);
                self.waiting
                    .borrow_mut()
                    .insert(index, PartArrival::Arriving);
                queued = true;
                continue;
            }
            // A part before the head was started before it, so it exists
            // somewhere: one not on this device is asked for, never started
            // here empty.
            if matches!(
                self.workspace.store.try_open_resource(&checkout),
                Err(ResourceOpenError::MissingResource)
            ) {
                self.workspace.store.want(checkout.id);
                continue;
            }
            if let Err(reason) = self.hold(index) {
                tracing::warn!(kind = self.part.kind, part = index, %reason, "earlier part not held");
            }
        }
        if queued {
            self.moved();
            self.turns.ask();
        }
    }

    /// Open the newest earlier part waiting for its turn, once one came.
    /// Whether one opened.
    pub(super) fn open_in_turn(&self) -> bool {
        if self.queued.borrow().is_empty() || !self.workspace.store.takes_writes() {
            return false;
        }
        if !self.turns.take() {
            self.turns.ask();
            return false;
        }
        let Some(index) = self.queued.borrow_mut().pop_last() else {
            return false;
        };
        self.waiting.borrow_mut().remove(&index);
        if let Err(reason) = self.hold(index) {
            tracing::warn!(kind = self.part.kind, part = index, %reason, "earlier part not held");
        }
        if !self.queued.borrow().is_empty() {
            self.turns.ask();
        }
        self.moved();
        true
    }

    /// Whether part `index` is kept here and waits for its turn to open.
    pub(super) fn waits_its_turn(&self, index: u64) -> bool {
        self.queued.borrow().contains(&index)
    }
}
