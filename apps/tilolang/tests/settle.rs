//! A host's frames and a window's, until the host owes nothing: the loop a session runs for a
//! window, without its wait.
//!
//! A host says what it owes ([`TilolangHost::owes_work`]): an instance opening, a change being
//! applied, a window with a frame coming. A frame taken while it owes any of them shows what the
//! workspace held before it, so a test settles and never takes a quiet moment for a settled host:
//! under load the quiet moment comes before the work.

use std::sync::{Arc, Condvar, Mutex};

use tilolang_host::TilolangHost;
use ui::headless::FrameHarness;

/// The longest a host may take to be done with what it owes before the test says so: a failure,
/// not a wait — nothing is settled by letting time pass.
const LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The frame a window's own motion is given after the host owes nothing, and how many of them: a
/// glide or an entrance under way has the time it runs in, and one that never ends is no wait.
const MOTION_SLICE: std::time::Duration = std::time::Duration::from_millis(16);
const MOTION: u8 = 16;

/// What wakes the host: its work off the frames (an index built from the record, the whole search
/// index read) is ready to be taken in.
#[derive(Default)]
pub struct Woken {
    woken: Mutex<bool>,
    signal: Condvar,
}

impl Woken {
    /// The host's waker: what its work off the frames calls when a result waits.
    pub fn waker(self: &Arc<Self>) -> tilolang_tilo::background::Waker {
        let woken = Arc::clone(self);
        Arc::new(move || woken.mark())
    }

    fn mark(&self) {
        *self
            .woken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.signal.notify_all();
    }

    /// Whether the host was woken since this was last taken: its frames have something to take
    /// in.
    fn take(&self) -> bool {
        std::mem::take(
            &mut *self
                .woken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Wait until the host is woken, at most `timeout`: the frames have nothing to take in until
    /// then.
    pub fn wait(&self, timeout: std::time::Duration) {
        let _ = self.signal.wait_timeout_while(
            self.woken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            timeout,
            |woken| !*woken,
        );
    }
}

/// The harness window's frame, and whether a frame drawn now would still show something the last
/// one did not: a window is read at rest only once it took in what the instance sent it, laid it
/// out and drew it.
pub fn framed(window: &mut FrameHarness) -> bool {
    window.relayout();
    window.frame_schedule().needs_redraw
}

/// The host's frames and the window's, until it owes nothing and the window has drawn what it was
/// sent.
pub fn settle(host: &mut TilolangHost, woken: &Woken, draw: impl FnMut() -> bool) {
    until(host, woken, draw, "the window settles", |_| Some(()));
}

/// The host's frames and `draw`'s until `done` reads what the test waits for — `waited` names it,
/// for a failure. `draw` is the window's frame: it takes in the patches the host sent it and says
/// whether its tree owes another. `done` is only asked of a host that owes nothing, so a window is
/// never read before its first frame.
///
/// Nothing is settled by letting time pass: a host that owes nothing draws nothing more until its
/// door, a window or a wake reaches it, so the frame read then is the frame the next would be. A
/// window's own motion — a glide, an entrance — is given the slice it runs in; one that never
/// ends, a caret's blink, does not hold the test.
///
/// # Panics
/// What was waited for did not happen within [`LIMIT`]: the host still owes work.
pub fn until<T>(
    host: &mut TilolangHost,
    woken: &Woken,
    mut draw: impl FnMut() -> bool,
    waited: &str,
    mut done: impl FnMut(&mut TilolangHost) -> Option<T>,
) -> T {
    let began = std::time::Instant::now();
    // The turns in a row the host had nothing to do: a window that was just drawn may have sent
    // it something the turn after reads, so one turn of its own is not rest.
    let mut rested = 0_u8;
    // The frames in a row the window asked for itself after the host last had work: a glide or an
    // entrance under way finishes in them, and a window that never rests (a caret's blink) is
    // read as it is once they are spent.
    let mut motion = 0_u8;
    loop {
        assert!(
            began.elapsed() < LIMIT,
            "{} did not happen in {}s: the host still owes {:?}",
            waited,
            began.elapsed().as_secs(),
            host.owed()
        );
        let mut changed = host.turn();
        let owes_a_frame = draw();
        // A wake that came while the frame ran: the work it names is taken in now, not when it
        // wakes the host again.
        changed |= woken.take();
        if changed || host.owes_work() {
            rested = 0;
            motion = 0;
            if !changed {
                // Work off the frame, which wakes the host when its result waits.
                woken.wait(LIMIT.saturating_sub(began.elapsed()));
            }
            continue;
        }
        rested = rested.saturating_add(1);
        if rested < 2 {
            continue;
        }
        if owes_a_frame && motion < MOTION {
            motion = motion.saturating_add(1);
            std::thread::sleep(MOTION_SLICE);
            continue;
        }
        if let Some(found) = done(host) {
            return found;
        }
        // Nothing owed and nothing changed: what the test waits for comes on the instance's
        // connection, and nothing off the frame wakes it.
        std::thread::sleep(MOTION_SLICE);
    }
}
