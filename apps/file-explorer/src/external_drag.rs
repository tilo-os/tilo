//! The drag Files is carrying, published for the host to hand to another window.

use std::path::PathBuf;
use std::sync::Mutex;
use ui::lock::MutexRecover as _;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalDrag {
    pub paths: Vec<PathBuf>,
}

#[derive(Default)]
struct Slot {
    active: Option<ExternalDrag>,
    handed_off: bool,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot {
    active: None,
    handed_off: false,
});

fn slot() -> std::sync::MutexGuard<'static, Slot> {
    SLOT.lock_recover()
}

pub fn begin(paths: Vec<PathBuf>) {
    let mut s = slot();
    s.active = Some(ExternalDrag { paths });
    s.handed_off = false;
}

pub fn end() {
    let mut s = slot();
    s.active = None;
    s.handed_off = false;
}

pub fn unclaimed() -> Option<ExternalDrag> {
    let s = slot();
    if s.handed_off {
        return None;
    }
    s.active.clone()
}

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

    #[test]
    fn external_drag_hand_off_is_claimed_once() {
        {
            let _g = SERIAL.lock_recover();
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

        {
            let _g = SERIAL.lock_recover();
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
