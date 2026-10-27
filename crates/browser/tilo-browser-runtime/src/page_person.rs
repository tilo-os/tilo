//! Which of the page's peers speaks for its person: the first one started,
//! which installs the page's identity (and, once it joins a room, the
//! person's private conversations). A peer started beside it for a moment
//! (a lobby on another of the person's machines, to make a workspace there)
//! neither replaces what that one installed nor takes it away when it is
//! disposed.

use std::cell::Cell;

thread_local! {
    static SPEAKS: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Peer `id` started: whether it speaks for the page's person, which only
/// the first peer while none does.
pub(crate) fn claim(id: u32) -> bool {
    SPEAKS.with(|speaks| {
        if speaks.get().is_some() {
            return false;
        }
        speaks.set(Some(id));
        true
    })
}

/// Whether peer `id` speaks for the page's person.
pub(crate) fn speaks(id: u32) -> bool {
    SPEAKS.with(|speaks| speaks.get() == Some(id))
}

/// Peer `id` is disposed: whether what it installed for the page's person
/// goes with it, which only the peer that speaks for them installed.
pub(crate) fn release(id: u32) -> bool {
    SPEAKS.with(|speaks| {
        if speaks.get() != Some(id) {
            return false;
        }
        speaks.set(None);
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lobby_beside_the_room_leaves_the_persons_identity_alone() {
        assert!(claim(1), "the room's peer speaks for the person");
        assert!(!claim(2), "a lobby started beside it does not");
        assert!(speaks(1) && !speaks(2));
        assert!(!release(2), "disposing the lobby takes nothing away");
        assert!(speaks(1));
        assert!(release(1), "the room's peer leaving does");
        assert!(claim(3), "and the next peer speaks for the person");
        assert!(release(3));
    }
}
