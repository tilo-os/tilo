//! Frames the GPU has not finished drawing yet.
//!
//! The page draws on animation frames, and a browser keeps asking for them
//! while the GPU still works on earlier frames: a slow GPU (a software one,
//! a small device's) then queues frame after frame, and what the person did
//! reaches the screen later and later, seconds behind a dragged window. A
//! mount draws while fewer than [`IN_FLIGHT`] of its frames are unfinished;
//! otherwise it keeps what changed for the frame after the GPU catches up,
//! which then draws all of it at once.
//!
//! A completion is a promise from the browser, and a promise can go missing:
//! a device lost under memory pressure, a driver that never answers. Waiting
//! on it for ever freezes the page — no frame, no status, nothing to show the
//! person why. So a frame that has been out for longer than [`LOST_AFTER`]
//! stops holding the page back, the mount draws without it, and says how long
//! it waited. A slow GPU is not a lost one: however many frames it leaves
//! out, the page keeps drawing and counting them, and only the device's own
//! word (WebGPU's `lost`, or an error from it) takes it for lost.
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ui::time::{Duration, Instant};

/// The frames that may be submitted and not yet done. Two keep the CPU's
/// next frame overlapping the GPU's last one, so a GPU that keeps up with
/// the display never holds a frame, however late the browser reports work
/// done.
pub const IN_FLIGHT: usize = 2;

/// How long a submitted frame may go unreported before it stops holding the
/// next one back. A page drawing a megapixel through a software rasteriser
/// takes about half a second for a frame, and [`IN_FLIGHT`] of them are
/// allowed out, so a page that is merely slow already has an outstanding
/// frame about a second old whenever a third would queue — the bound has to
/// sit several frame times past that, or it fires on healthy drawing and
/// gives up the pacing it is part of. A GPU that keeps up reports in
/// milliseconds.
pub const LOST_AFTER: Duration = Duration::from_secs(6);

/// What a mount's frame loop may do with the frame it is about to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// The GPU has room for this frame.
    Draw,
    /// The GPU is still drawing earlier frames, and said so recently enough.
    Wait,
    /// A frame's completion is later than [`LOST_AFTER`]: this frame is drawn
    /// without waiting for it. `frame` is the one the GPU owes, `waiting` how
    /// long it has been out, and `stalls` how many frames this has happened
    /// to since the GPU last reported one.
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
    /// What the frame loop may do with the frame it has ready, at `now`.
    pub fn verdict(&mut self, now: Instant) -> Pacing {
        self.match_completions();
        if self.outstanding.len() < IN_FLIGHT {
            return Pacing::Draw;
        }
        let Some(&(frame, handed)) = self.outstanding.front() else {
            return Pacing::Draw;
        };
        let waiting = now.saturating_duration_since(handed);
        if waiting < LOST_AFTER {
            return Pacing::Wait;
        }
        // The GPU owes a frame it should have reported long ago: draw without
        // it rather than wait for ever. It stays owed, so a completion that
        // does come back is still matched to it.
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
        Done(Arc::clone(&self.finished))
    }

    /// Frames the GPU left out since it last reported one.
    #[must_use]
    pub fn stalls(&self) -> u32 {
        self.stalls
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

/// What the mount logs when it stops waiting for a frame's completion: which
/// frame the GPU owes, how long it has been out, and how many it has left out
/// since it last reported one.
#[must_use]
pub fn late_note(frame: u64, waiting: Duration, stalls: u32) -> String {
    format!(
        "Browser runtime: frame {frame} was handed to the GPU {:.1} s ago and it has not reported it drawn; drawing without it ({stalls} left out since it last reported one)",
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
            match verdict {
                Pacing::Wait => return verdict,
                Pacing::Draw | Pacing::Late { .. } => {}
            }
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

    #[test]
    fn keys_typed_while_the_gpu_is_behind_reach_the_next_frame_together() {
        let mut page = Loop::default();
        page.input('a');
        page.animation_frame(FRAME);
        page.input('b');
        page.animation_frame(FRAME);
        assert_eq!(
            page.drawn,
            [vec!['a'], vec!['b']],
            "two frames may be in flight"
        );
        // The GPU takes many display frames for each: keys keep coming.
        for key in ['c', 'd', 'e', 'f'] {
            page.input(key);
            page.animation_frame(FRAME);
        }
        assert_eq!(page.drawn.len(), 2, "no frame queues behind a busy GPU");
        assert_eq!(
            page.notes,
            Vec::<String>::new(),
            "a GPU a frame behind is late, not lost"
        );
        page.gpu_finishes_one();
        page.animation_frame(FRAME);
        page.animation_frame(FRAME);
        assert_eq!(
            page.drawn,
            [vec!['a'], vec!['b'], vec!['c', 'd', 'e', 'f']],
            "exactly one following frame draws every key held for it"
        );
        page.gpu_finishes_one();
        page.gpu_finishes_one();
        page.animation_frame(FRAME);
        assert_eq!(page.drawn.len(), 3, "nothing pending draws nothing");
        page.input('g');
        page.animation_frame(FRAME);
        assert_eq!(
            page.drawn.last(),
            Some(&vec!['g']),
            "no key is ever dropped"
        );
    }

    #[test]
    fn a_gpu_that_keeps_up_never_holds_a_frame() {
        let typed = "every key gets its own frame";
        let mut page = Loop::default();
        for key in typed.chars() {
            page.input(key);
            page.animation_frame(FRAME);
            // Done is reported late, after the next frame was submitted.
            if page.gpu.len() > 1 {
                page.gpu_finishes_one();
            }
        }
        assert_eq!(page.drawn.len(), typed.len());
        assert_eq!(page.notes, Vec::<String>::new());
    }

    /// A GPU that never reports a frame done — a completion lost for good, a
    /// software rasteriser starved of the CPU: the page keeps presenting, and
    /// each frame it stops waiting for is counted and said with the frame's
    /// number and how long it had been out. Slow is not lost: no number of
    /// them makes the page give the device up.
    #[test]
    fn a_gpu_that_never_reports_keeps_the_page_drawing_and_says_so() {
        let mut page = Loop::default();
        // A person keeps working — every frame has something to draw — and the
        // GPU never reports one of them done, for two minutes.
        for _ in 0..7_500 {
            page.input('x');
            page.animation_frame(FRAME);
        }
        assert!(
            page.drawn.len() >= 10,
            "a page whose GPU never answers keeps presenting: {} frames, {:?}",
            page.drawn.len(),
            page.notes.first()
        );
        assert_eq!(
            usize::try_from(page.pacing.stalls()).ok(),
            Some(page.notes.len()),
            "every frame the GPU left out was counted and said"
        );
        let first = page.notes.first().expect("the first stall was said");
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
            "no stall says the device is lost: {:?}",
            page.notes.last()
        );
        // The GPU comes back and reports what it owes: the count starts again.
        while !page.gpu.is_empty() {
            page.gpu_finishes_one();
        }
        page.input('y');
        page.animation_frame(FRAME);
        assert_eq!(page.pacing.stalls(), 0, "a GPU that reports is working");
    }

    /// A GPU a second and a half behind is waited for, not given up on: the
    /// bound sits several frame times past what a page drawing a megapixel
    /// takes, so only a completion that never comes reaches it.
    #[test]
    fn a_gpu_that_answers_late_is_still_waited_for() {
        let mut page = Loop::default();
        for step in 0..400 {
            page.input('c');
            page.animation_frame(FRAME);
            // About every second and a half, as a software rasteriser under
            // load reports: late, but never silent.
            if step % 94 == 93 {
                page.gpu_finishes_one();
            }
        }
        assert_eq!(page.notes, Vec::<String>::new(), "no frame was given up on");
        assert_eq!(page.pacing.stalls(), 0);
    }
}
