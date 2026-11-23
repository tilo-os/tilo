//! The room at the MCP server: how many connections it serves at once, counted as one whichever
//! way a connection came in, and what the connection past them is told.
//!
//! A connection takes a [`Place`] before the API serves it and gives it back when it ends. A
//! program of this login, and one inside an environment, takes it as the API accepts it.
//!
//! Who connects is decided first. A program of another account, one inside a sandbox, one
//! outside the environment whose API it reached and a device that is not the person's are
//! refused as that, whether or not the API has room: only a connection the API would serve is
//! told that it is full.
//!
//! Sixteen, and not more, for what a connection costs the session. Each one keeps two threads
//! of the compositor's process for as long as it lasts, the one that serves it and the one that
//! reads it, and may have sixteen requests working and sixty-four working or waiting
//! ([`tilo_mcp`]). All of them are worked on the one runtime thread and the eight
//! blocking threads every connection shares (`tilo_rt`), and what they ask of the session
//! itself is answered on its loop's pass, between frames. More connections would wait behind
//! those same threads and that same pass, beside the person's own frames. A runtime that runs
//! without a desktop serves sixty-four in a process that draws nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Connections served at once; one more is told [`NO_ROOM`] and closed.
const MAX_CONNECTIONS: usize = 16;

/// What a connection past [`MAX_CONNECTIONS`] is told, in the words the runtime's own endpoint
/// tells one past its bound (`tilo_runtime::daemon`): what is full, that nothing it asked ran,
/// and when to connect again. It names the bound, which is the same for everyone, and nothing
/// of who holds the places.
pub(super) const NO_ROOM: &str = "the MCP server is serving 16 connections, as many as it serves \
     at once, so this one is closed and nothing it asked ran; connect again once one of them has \
     closed";
const _: () = assert!(MAX_CONNECTIONS == 16, "NO_ROOM names the bound");

/// The API's places: one count for this login's programs, its environments' and the person's
/// other devices.
#[derive(Clone, Default)]
pub(crate) struct Room {
    taken: Arc<AtomicUsize>,
}

impl Room {
    /// Take a place, when one is free. Whoever asks at once, no more than [`MAX_CONNECTIONS`]
    /// hold one.
    pub(crate) fn take(&self) -> Option<Place> {
        let took = self
            .taken
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |taken| {
                (taken < MAX_CONNECTIONS).then(|| taken.saturating_add(1))
            });
        took.is_ok().then(|| Place {
            taken: Arc::clone(&self.taken),
        })
    }
}

/// One connection's place at the API, given back when it is dropped: when the connection
/// ended, or was never served.
pub(crate) struct Place {
    taken: Arc<AtomicUsize>,
}

impl Drop for Place {
    fn drop(&mut self) {
        self.taken.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The API's accept loops and the endpoint that hears the person's other devices take
    /// places from threads of their own: however many ask at once, each place is held once, and
    /// one given back is there for the next.
    #[test]
    fn each_place_is_held_once_however_many_ask_at_once() {
        let room = Room::default();
        let (tell, took) = std::sync::mpsc::channel();
        for _ in 0..MAX_CONNECTIONS.saturating_mul(4) {
            let (room, tell) = (room.clone(), tell.clone());
            std::thread::spawn(move || tell.send(room.take()));
        }
        drop(tell);
        // Every thread has asked once the last of them let go of its sender.
        let mut held: Vec<Place> = took.iter().flatten().collect();
        assert_eq!(held.len(), MAX_CONNECTIONS, "as many as the server serves");
        assert!(room.take().is_none(), "and no more");
        drop(held.pop());
        let next = room.take();
        assert!(next.is_some(), "a place given back is taken again");
        assert!(room.take().is_none(), "and only that one");
    }
}
