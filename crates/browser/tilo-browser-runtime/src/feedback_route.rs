//! Which of the page's peers carries a desktop's problem reports. Reports
//! go to one channel of one room (a web host's home workspace), whichever
//! room the person is in: a desktop in another workspace (or the person's
//! own, peerless desktop) hands its reports to a peer the page keeps in that room, and
//! the windows a report is about stay the desktop's.

use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    /// Desktop peer to the peer carrying its reports, where they differ.
    static THROUGH: RefCell<HashMap<u32, u32>> = RefCell::new(HashMap::new());
}

/// Peer `desktop`'s reports go through peer `carrier`.
pub(crate) fn send_through(desktop: u32, carrier: u32) {
    THROUGH.with_borrow_mut(|through| {
        if desktop == carrier {
            through.remove(&desktop);
        } else {
            through.insert(desktop, carrier);
        }
    });
}

/// The peer carrying peer `desktop`'s reports: itself unless another does.
pub(crate) fn carrier(desktop: u32) -> u32 {
    THROUGH.with_borrow(|through| through.get(&desktop).copied().unwrap_or(desktop))
}

/// The desktop peers whose reports peer `carrier` carries for them.
pub(crate) fn desktops(carrier: u32) -> Vec<u32> {
    THROUGH.with_borrow(|through| {
        let mut desktops: Vec<u32> = through
            .iter()
            .filter(|(_, through)| **through == carrier)
            .map(|(desktop, _)| *desktop)
            .collect();
        desktops.sort_unstable();
        desktops
    })
}

/// The desktop whose windows peer `carrier`'s reports are about: the first
/// it carries reports for, else itself.
pub(crate) fn desktop_of(carrier: u32) -> u32 {
    desktops(carrier).first().copied().unwrap_or(carrier)
}

/// Peer `id` went away: no desktop sends through it, nor it through another.
pub(crate) fn forget(id: u32) {
    THROUGH.with_borrow_mut(|through| {
        through.retain(|desktop, carrier| *desktop != id && *carrier != id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_desktop_in_another_room_sends_through_the_homes_peer() {
        assert_eq!(
            carrier(1),
            1,
            "a desktop carries its own reports by default"
        );
        send_through(1, 2);
        assert_eq!(carrier(1), 2, "the home's peer carries them once named");
        assert_eq!(desktops(2), vec![1]);
        assert_eq!(desktop_of(2), 1, "reports are about the desktop's windows");
        assert_eq!(desktop_of(1), 1);
        forget(2);
        assert_eq!(carrier(1), 1, "a carrier that leaves takes nothing with it");
        assert!(desktops(2).is_empty());
        send_through(3, 4);
        forget(3);
        assert_eq!(desktop_of(4), 4, "nor does a desktop that leaves");
        send_through(5, 5);
        assert_eq!(carrier(5), 5);
        assert!(desktops(5).is_empty());
    }
}
