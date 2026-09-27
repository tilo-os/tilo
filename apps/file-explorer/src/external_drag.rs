//! The drag the file manager is carrying, published for its host.
//!
//! Inside the compositor, Files is a widget — not a Wayland client — so a
//! row dragged out of it cannot become a `wl_data_device` drag on its own.
//! The host can: when the pointer carries a Files drag over a foreign
//! client's window, the compositor starts a server-side drag offering the
//! file, and from then on the drop belongs to that client. This module is
//! the hand-off: Files says what it is carrying, the host claims it, and
//! Files learns on release that the drop was not its own to resolve.
//!
//! Process-global because the compositor is one process with one pointer:
//! at most one drag exists at a time, whichever Files tab started it.

use std::path::PathBuf;
use std::sync::Mutex;

/// What a Files drag carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalDrag {
    pub paths: Vec<PathBuf>,
}

#[derive(Default)]
struct Slot {
    active: Option<ExternalDrag>,
    /// Set once the host has taken the drag over to a client. Files then
    /// keeps drawing its ghost but must not act on the release.
    handed_off: bool,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot {
    active: None,
    handed_off: false,
});

fn slot() -> std::sync::MutexGuard<'static, Slot> {
    SLOT.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Files started carrying `paths`.
pub fn begin(paths: Vec<PathBuf>) {
    let mut s = slot();
    s.active = Some(ExternalDrag { paths });
    s.handed_off = false;
}

/// The drag ended on the Files side — dropped, cancelled, or handed off
/// and released. Clears everything.
pub fn end() {
    let mut s = slot();
    s.active = None;
    s.handed_off = false;
}

/// The drag Files is carrying and the host has **not** yet claimed.
pub fn unclaimed() -> Option<ExternalDrag> {
    let s = slot();
    if s.handed_off {
        return None;
    }
    s.active.clone()
}

/// The host claims the drag for a client. Returns what is carried, or
/// `None` if there is no drag or it was already claimed.
pub fn hand_off() -> Option<ExternalDrag> {
    let mut s = slot();
    if s.handed_off {
        return None;
    }
    let drag = s.active.clone()?;
    s.handed_off = true;
    drop(s);
    Some(drag)
}

/// Whether the host holds the current drag.
pub fn is_handed_off() -> bool {
    let s = slot();
    s.active.is_some() && s.handed_off
}

#[cfg(test)]
mod tests {
    use super::*;

    // The slot is process-global; these run under one lock so they cannot
    // interleave.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// A drag stays unclaimed until the host takes it and a new drag forgets the previous hand-off.
    #[test]
    fn external_drag_hand_off_is_claimed_once() {
        // --- phase: a_drag_is_unclaimed_until_the_host_takes_it ---
        {
            let _g = SERIAL
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            end();
            assert_eq!(unclaimed(), None);
            begin(vec![PathBuf::from("/w/photo.png")]);
            assert_eq!(
                unclaimed(),
                Some(ExternalDrag {
                    paths: vec![PathBuf::from("/w/photo.png")]
                })
            );
            assert!(!is_handed_off());
            let taken = hand_off().expect("the host can claim it once");
            assert_eq!(taken.paths, vec![PathBuf::from("/w/photo.png")]);
            assert!(is_handed_off());
            assert_eq!(unclaimed(), None, "a claimed drag is not offered again");
            assert_eq!(hand_off(), None, "nor claimed twice");
            end();
            assert!(!is_handed_off());
            assert_eq!(hand_off(), None);
        }

        // --- phase: a_new_drag_forgets_the_previous_hand_off ---
        {
            let _g = SERIAL
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            begin(vec![PathBuf::from("/a")]);
            let _ = hand_off();
            begin(vec![PathBuf::from("/b")]);
            assert!(!is_handed_off());
            assert_eq!(
                unclaimed().map(|d| d.paths),
                Some(vec![PathBuf::from("/b")])
            );
            end();
        }
    }
}
