//! Frames the GPU has not finished drawing yet.
//!
//! The page draws on animation frames. A submission's completion is a promise
//! from the browser, and the browser can report it long after the frame it
//! stands for is on screen — longer still while the page hands nothing more
//! over. Measured in a browser rasterising through `SwiftShader`, a page that
//! stopped drawing after two frames saw the next completion 3–14 s later,
//! while the same page drawing on had every completion in 29–308 ms: opening
//! a window took 0.7–6 s while it waited, and 0.2–0.5 s drawing on.
//!
//! So a mount draws the frame it has ready and never stops for one it has not
//! been told about. What keeps a slow GPU's queue short is the browser's own
//! canvas: a surface with no image to hand out says so (an outdated surface, a
//! texture that timed out, which the frame loop already answers), and the
//! display takes frames no faster than it presents them. A frame the GPU
//! leaves unreported is counted and said once it has been out longer than
//! [`LOST_AFTER`], so a page whose GPU stopped answering still says so; a slow
//! GPU is not a lost one, and only the device's own word (WebGPU's `lost`, or
//! an error from it) takes it for lost.
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ui::time::{Duration, Instant};

/// How long a submitted frame may go unreported before the page says the GPU
/// is not answering. What was seen of a software rasteriser that does answer:
/// frames completed in 29–308 ms with the page drawing on, and in seconds
/// when the page stopped (which is what kept the answer from coming).
pub const LOST_AFTER: Duration = Duration::from_secs(6);

/// The most frames kept as owed: more than [`LOST_AFTER`] holds at twice the
/// display's rate, so a GPU that answers only seconds late is still matched to
/// every frame it finishes. A GPU that never answers leaves every frame here;
/// the list is a diagnostic, not a reason to grow.
const MOST_OWED: usize = 512;

/// What a mount's frame loop is told about the frame it is about to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// Nothing to report: the GPU is keeping up, or this is the first frame.
    Draw,
    /// The frame the GPU owes has been out longer than [`LOST_AFTER`] and it
    /// has not said it finished it. The frame is still drawn, and this is
    /// said. `frame` is the one the GPU owes, `waiting` how long it has been
    /// out, and `stalls` how many frames this has happened to since the GPU
    /// last reported one.
    Late {
        frame: u64,
        waiting: Duration,
        stalls: u32,
    },
}

/// Counts a mount's submitted frames against those its GPU finished, and the
/// ones it never reported at all.
#[derive(Default)]
pub struct FramePacing {
    /// The frames handed to the GPU and not reported done, oldest first, with
    /// the number each was given and when it was handed over.
    outstanding: VecDeque<(u64, Instant)>,
    /// Frames handed to the GPU so far: a frame's number is its place here.
    handed: u64,
    /// Completions the GPU reported, as the page counts them.
    finished: Arc<AtomicU64>,
    /// Completions this side has already matched to an outstanding frame.
    matched: u64,
    /// Frames drawn without their completion, since the GPU last reported
    /// one.
    stalls: u32,
}

impl FramePacing {
    /// What the frame loop is told before it draws its frame, at `now`: the
    /// frame is drawn either way — a page that stops for a completion is a
    /// page whose completion comes late — and a frame the GPU has owed for
    /// longer than [`LOST_AFTER`] is counted as left out and said.
    pub fn verdict(&mut self, now: Instant) -> Pacing {
        self.match_completions();
        let Some(&(frame, handed)) = self.outstanding.front() else {
            return Pacing::Draw;
        };
        let waiting = now.saturating_duration_since(handed);
        if waiting < LOST_AFTER {
            return Pacing::Draw;
        }
        // Counted and given up on rather than waited for; a completion that
        // does come back is still matched to its frame's number.
        self.outstanding.pop_front();
        self.stalls = self.stalls.saturating_add(1);
        Pacing::Late {
            frame,
            waiting,
            stalls: self.stalls,
        }
    }

    /// A frame was submitted at `now`: call the returned completion when the
    /// GPU finished its work.
    #[must_use]
    pub fn submitted(&mut self, now: Instant) -> Done {
        self.handed = self.handed.saturating_add(1);
        self.outstanding.push_back((self.handed, now));
        while self.outstanding.len() > MOST_OWED {
            self.outstanding.pop_front();
            self.stalls = self.stalls.saturating_add(1);
        }
        Done(Arc::clone(&self.finished))
    }

    /// Frames the GPU left out since it last reported one.
    #[must_use]
    pub fn stalls(&self) -> u32 {
        self.stalls
    }

    /// Frames the GPU still owes: what a silent GPU grows, up to
    /// [`MOST_OWED`].
    #[cfg(test)]
    pub(crate) fn owed(&self) -> usize {
        self.outstanding.len()
    }

    /// Match the completions the GPU reported to the frames they finish, and
    /// start counting stalls again: a GPU that reports anything is working,
    /// however late.
    fn match_completions(&mut self) {
        let done = self.finished.load(Ordering::Acquire);
        if done == self.matched {
            return;
        }
        self.matched = done;
        self.stalls = 0;
        while let Some(&(frame, _)) = self.outstanding.front() {
            if frame > done {
                break;
            }
            self.outstanding.pop_front();
        }
    }
}

/// One submitted frame's completion.
pub struct Done(Arc<AtomicU64>);

impl Done {
    pub fn finished(self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

/// What the mount says about a frame the GPU has owed longer than
/// [`LOST_AFTER`]: which frame it is, how long it has been out, and how many
/// it has left out since it last reported one.
#[must_use]
pub fn late_note(frame: u64, waiting: Duration, stalls: u32) -> String {
    format!(
        "Browser runtime: frame {frame} was handed to the GPU {:.1} s ago and it has not reported it drawn ({stalls} left out since it last reported one)",
        waiting.as_secs_f64(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mount's frame loop as the page runs it: input marks it pending and
    /// is laid out by the next frame drawn; every animation frame draws when
    /// something is pending and the pacing allows. `now` is the page's clock,
    /// which each turn of the loop moves on.
    struct Loop {
        pacing: FramePacing,
        now: Instant,
        pending: Vec<char>,
        drawn: Vec<Vec<char>>,
        /// The GPU's own frames, oldest first: what it has not finished yet.
        gpu: Vec<Done>,
        notes: Vec<String>,
    }

    impl Default for Loop {
        fn default() -> Self {
            let started = ui::time::now();
            Self {
                pacing: FramePacing::default(),
                now: started,
                pending: Vec::new(),
                drawn: Vec::new(),
                gpu: Vec::new(),
                notes: Vec::new(),
            }
        }
    }

    /// One display frame.
    const FRAME: Duration = Duration::from_millis(16);

    impl Loop {
        fn input(&mut self, key: char) {
            self.pending.push(key);
        }

        /// One animation frame, `after` the last: the pacing decides, the
        /// mount logs what it decided, and a drawn frame goes to the GPU.
        fn animation_frame(&mut self, after: Duration) -> Pacing {
            self.now = self.now.checked_add(after).unwrap_or(self.now);
            if self.pending.is_empty() {
                return Pacing::Draw;
            }
            let verdict = self.pacing.verdict(self.now);
            if let Pacing::Late {
                frame,
                waiting,
                stalls,
            } = verdict
            {
                self.notes.push(late_note(frame, waiting, stalls));
            }
            self.drawn.push(std::mem::take(&mut self.pending));
            self.gpu.push(self.pacing.submitted(self.now));
            verdict
        }

        fn gpu_finishes_one(&mut self) {
            if !self.gpu.is_empty() {
                self.gpu.remove(0).finished();
            }
        }
    }

    tilo_test_env::scenario! {
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "test arithmetic: the lint exempts the body of a test function, not the closure a phase is"
        )]
        fn frames_are_drawn_as_they_come_and_a_silent_gpu_is_said;

        phase every_frame_is_drawn_whatever_the_gpu_has_reported;
        // A GPU that never answers does not hold a frame back: what the person
        // did is drawn on the next frame drawn, every time.
        {
            let mut page = Loop::default();
            for key in ['a', 'b', 'c', 'd', 'e', 'f'] {
                page.input(key);
                page.animation_frame(FRAME);
            }
            assert_eq!(
                page.drawn,
                [vec!['a'], vec!['b'], vec!['c'], vec!['d'], vec!['e'], vec!['f']],
                "each key reaches its own frame, with no completion reported"
            );
            assert_eq!(
                page.notes,
                Vec::<String>::new(),
                "a GPU an animation frame behind is not overdue"
            );
        }

        phase a_completion_the_gpu_never_reports_is_said_once_it_is_overdue;
        // A GPU whose completion goes missing for good (a software rasteriser
        // starved of the CPU, a promise the browser drops): the page keeps
        // presenting, and says so once the frame it owes is overdue, naming
        // it, how long it waited, and how many that has happened to.
        {
            let mut page = Loop::default();
            for _ in 0..7_500 {
                page.input('x');
                page.animation_frame(FRAME);
            }
            assert_eq!(
                page.drawn.len(),
                7_500,
                "every frame with something to draw is drawn"
            );
            let first = page.notes.first().expect("the first overdue frame was said");
            assert!(
                first.starts_with("Browser runtime: frame 1 was handed to the GPU 6.0 s ago"),
                "the note names the frame and how long it waited: {first}"
            );
            assert!(
                first.contains("1 left out since it last reported one"),
                "and how many it has left out: {first}"
            );
            assert!(
                page.notes.iter().all(|note| !note.contains("lost")),
                "no overdue frame says the device is lost: {:?}",
                page.notes.last()
            );
            assert!(
                page.pacing.stalls() > 100,
                "the frames it left out are counted: {}",
                page.pacing.stalls()
            );
        }

        phase a_gpu_that_reports_keeps_the_count_at_nothing;
        {
            let mut page = Loop::default();
            for key in ['a', 'b', 'c', 'd'] {
                page.input(key);
                page.animation_frame(FRAME);
                // A GPU that keeps up, reported after the frame went out.
                page.gpu_finishes_one();
            }
            assert_eq!(page.drawn.len(), 4);
            assert_eq!(page.notes, Vec::<String>::new());
            assert_eq!(page.pacing.stalls(), 0);
        }

        phase a_gpu_that_answers_late_is_not_taken_for_lost;
        // A GPU about a second and a half behind still answers, one frame for
        // each it is handed: its frames are drawn as they come, and nothing is
        // counted against it.
        {
            let mut page = Loop::default();
            for _ in 0..400 {
                page.input('c');
                page.animation_frame(FRAME);
                // 94 frames at 16 ms is the second and a half it lags by.
                while page.gpu.len() > 94 {
                    page.gpu_finishes_one();
                }
            }
            assert_eq!(page.drawn.len(), 400, "every frame is drawn");
            assert_eq!(page.pacing.stalls(), 0, "a GPU that reports is working");
        }

        phase the_frames_a_silent_gpu_owes_do_not_grow_without_bound;
        {
            let mut page = Loop::default();
            for _ in 0..(MOST_OWED * 3) {
                page.input('x');
                page.animation_frame(FRAME);
            }
            assert!(
                page.pacing.owed() <= MOST_OWED,
                "what the silent GPU owes stays bounded: {}",
                page.pacing.owed()
            );
        }
    }
}
