//! Moves each output's hardware cursor plane from a thread of its own.
//!
//! The legacy cursor ioctl takes its CRTC's modeset lock, and a blocking
//! commit on the presenter thread holds that lock for as long as the commit
//! runs. Waking the displays is such a commit, and on a DP sink whose link
//! training times out it ran for seconds — while pointer motion, which moved
//! the plane inline from the compositor loop's input dispatch, waited on the
//! same lock and froze the whole desktop with it. The loop now only records
//! where the plane belongs; the output's mover thread carries the newest
//! position to the kernel, so a stalled CRTC stalls only its own cursor.

use std::sync::{Arc, Condvar, Mutex, PoisonError};

use tilo_lock::MutexRecover as _;

/// Repositions one output's cursor plane in the kernel. May block for as long
/// as a commit on that CRTC holds its lock.
pub trait PlaneMover: Send + Sync {
    /// Put the plane's top-left at `(x, y)` in output pixels.
    fn move_plane(&self, x: i32, y: i32);
}

/// Whether an output's cursor plane may take legacy cursor ioctls yet, and
/// where the pointer last asked it to be.
///
/// An atomic output's CRTC is lit by a nonblocking modeset. A legacy cursor
/// ioctl on it before that modeset completed is an atomic commit of its own
/// against a CRTC the driver is still setting up: amdgpu then reads the scan
/// position of a display stream that does not exist yet and oopses, killing
/// the thread that asked while it holds the device's modeset locks. The next
/// ioctl that takes those locks, from any thread, oopses too. So nothing
/// touches the cursor plane until the output is proven up, which binds it
/// ([`Self::bind`]) and moves it to where the pointer went meanwhile.
#[derive(Default)]
pub struct CursorGate {
    state: Mutex<GateState>,
}

#[derive(Default)]
struct GateState {
    bound: bool,
    /// The last position the pointer asked for, in output pixels.
    last: Option<(i32, i32)>,
}

impl CursorGate {
    /// Whether the plane is bound and may be written.
    pub fn bound(&self) -> bool {
        self.state.lock_recover().bound
    }

    /// Mark the plane bound or not, without moving it.
    pub fn set_bound(&self, bound: bool) {
        self.state.lock_recover().bound = bound;
    }

    /// The pointer wants the plane at `(x, y)`: remembered either way, so a
    /// plane bound later starts there. `true` when the plane may be moved now.
    pub fn request(&self, x: i32, y: i32) -> bool {
        let mut state = self.state.lock_recover();
        state.last = Some((x, y));
        state.bound
    }

    /// The plane was just bound: open the gate and `place` it where the
    /// pointer last asked, under the gate, so a move requested after this
    /// one reaches the kernel after it.
    pub fn bind(&self, place: impl FnOnce((i32, i32))) {
        let mut state = self.state.lock_recover();
        state.bound = true;
        if let Some(position) = state.last {
            place(position);
        }
    }
}

/// The newest position not yet handed to the kernel, latest wins: a cursor
/// that waited out a stall goes straight to where the pointer is now rather
/// than replaying the path it took.
#[derive(Default)]
struct Slot {
    latest: Option<(i32, i32)>,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    slot: Mutex<Slot>,
    wake: Condvar,
}

enum Route {
    Thread(Arc<Shared>),
    /// The mover thread could not be started; moving inline keeps the cursor
    /// alive, with the loop exposed to the CRTC's lock as before.
    Inline(Arc<dyn PlaneMover>),
}

/// The loop's handle on one output's mover thread. Dropping it stops the
/// thread without waiting for an ioctl in flight, which then closes the
/// mover's card fd when it returns.
pub struct PlaneMoveWorker {
    route: Route,
}

impl PlaneMoveWorker {
    /// Start `output`'s mover thread.
    pub fn spawn(output: &str, mover: Arc<dyn PlaneMover>) -> Self {
        let shared = Arc::new(Shared::default());
        let thread_shared = Arc::clone(&shared);
        let thread_mover = Arc::clone(&mover);
        let spawned = std::thread::Builder::new()
            .name(format!("tilo-cursor-{output}"))
            .spawn(move || run(&thread_shared, thread_mover.as_ref()));
        match spawned {
            Ok(_) => Self {
                route: Route::Thread(shared),
            },
            Err(error) => {
                tracing::warn!(
                    output,
                    %error,
                    "DRM cursor: mover thread did not start; moving the plane on the compositor loop"
                );
                Self {
                    route: Route::Inline(mover),
                }
            }
        }
    }

    /// Ask for the plane at `(x, y)`; returns without touching the kernel.
    pub fn move_to(&self, x: i32, y: i32) {
        match &self.route {
            Route::Thread(shared) => {
                shared.slot.lock_recover().latest = Some((x, y));
                shared.wake.notify_one();
            }
            Route::Inline(mover) => mover.move_plane(x, y),
        }
    }
}

impl Drop for PlaneMoveWorker {
    fn drop(&mut self) {
        if let Route::Thread(shared) = &self.route {
            shared.slot.lock_recover().closed = true;
            shared.wake.notify_one();
        }
    }
}

fn run(shared: &Shared, mover: &dyn PlaneMover) {
    loop {
        let next = {
            let mut slot = shared.slot.lock_recover();
            loop {
                // A removed output's plane is gone with it: drop what it
                // still owed rather than moving a CRTC that now belongs to
                // whatever the next modeset gives it.
                if slot.closed {
                    break None;
                }
                if let Some(position) = slot.latest.take() {
                    break Some(position);
                }
                slot = shared
                    .wake
                    .wait(slot)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some((x, y)) = next else {
            return;
        };
        mover.move_plane(x, y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A cursor plane whose first move blocks until released, as the kernel
    /// blocks a cursor ioctl while a commit holds the CRTC.
    struct HeldCrtc {
        entered: mpsc::Sender<(i32, i32)>,
        release: Mutex<mpsc::Receiver<()>>,
        moved: Mutex<Vec<(i32, i32)>>,
    }

    impl PlaneMover for HeldCrtc {
        fn move_plane(&self, x: i32, y: i32) {
            let first = self.moved.lock_recover().is_empty();
            self.moved.lock_recover().push((x, y));
            let _ = self.entered.send((x, y));
            if first {
                let _ = self
                    .release
                    .lock_recover()
                    .recv_timeout(tilo_test_env::hang_bound());
            }
        }
    }

    fn held_crtc() -> (Arc<HeldCrtc>, mpsc::Receiver<(i32, i32)>, mpsc::Sender<()>) {
        let (entered, entered_rx) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let plane = Arc::new(HeldCrtc {
            entered,
            release: Mutex::new(release),
            moved: Mutex::new(Vec::new()),
        });
        (plane, entered_rx, release_tx)
    }

    #[test]
    fn pointer_motion_does_not_wait_for_a_crtc_a_commit_holds() {
        let (plane, entered, release) = held_crtc();
        let worker = PlaneMoveWorker::spawn("DP-2", plane.clone());
        worker.move_to(1, 1);
        assert_eq!(
            tilo_test_env::recv("the first move reaching the plane", &entered),
            (1, 1)
        );
        // The plane is now held, as a display-wake modeset holds it while DP
        // link training times out. Motion keeps arriving at input rate; each
        // must return to the loop at once. Inline, the first of these would
        // block until the hang bound and the release below would come late.
        for step in 2..=50 {
            worker.move_to(step, step);
        }
        release.send(()).expect("the held plane is still waiting");
        assert_eq!(
            tilo_test_env::recv("the move after the commit lets go", &entered),
            (50, 50),
            "once the commit lets go, the plane goes to the newest position"
        );
        drop(worker);
        assert_eq!(
            *plane.moved.lock_recover(),
            vec![(1, 1), (50, 50)],
            "positions queued behind the held CRTC coalesce into the newest one"
        );
    }

    #[test]
    fn a_removed_output_drops_the_moves_it_still_owed() {
        let (plane, entered, release) = held_crtc();
        let worker = PlaneMoveWorker::spawn("DP-2", plane.clone());
        worker.move_to(1, 1);
        assert_eq!(
            tilo_test_env::recv("the first move reaching the plane", &entered),
            (1, 1)
        );
        worker.move_to(9, 9);
        drop(worker);
        release.send(()).expect("the held plane is still waiting");
        // The thread exits and drops its handle on the plane; when every
        // other handle is ours, nothing more can move it.
        tilo_test_env::wait_until(
            "the mover thread to let go of a removed output's plane",
            || Arc::strong_count(&plane) == 1,
        );
        assert_eq!(*plane.moved.lock_recover(), vec![(1, 1)]);
    }

    #[test]
    fn an_unbound_plane_is_not_moved_and_starts_where_the_pointer_went() {
        let gate = CursorGate::default();
        assert!(
            !gate.request(10, 20),
            "a plane still modesetting is not moved"
        );
        assert!(!gate.request(30, 40));
        let mut placed = Vec::new();
        gate.bind(|position| placed.push(position));
        assert_eq!(placed, vec![(30, 40)], "bound where the pointer is now");
        assert!(gate.request(50, 60), "a bound plane follows the pointer");

        gate.set_bound(false);
        assert!(
            !gate.request(70, 80),
            "a CRTC modesetting again is left alone"
        );

        let untouched = CursorGate::default();
        let mut placed = Vec::new();
        untouched.bind(|position| placed.push(position));
        assert!(placed.is_empty(), "no pointer yet, nothing to place");
        assert!(untouched.bound());
    }
}
