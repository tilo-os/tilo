//! The wake scope of a root mounted bare ([`super::FrameHarness::for_root`]).
//!
//! A mounted component wakes its window through the component's own scope.
//! A bare root has none of its own, yet does the same work a live window's
//! root does: a read it starts on another thread wakes the window when it
//! lands, a frame it draws asks for the sync that reads on. The live
//! [`App`](crate::app::App) runs its frames (sync, layout, draw) and its input
//! in its wake scope; the harness runs a bare root's in this one, so those
//! wakes reach the next frame as they reach the live window's, instead of no
//! one.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::wake::{WakeScope, WakeScopeGuard};

pub(super) struct RootWake {
    scope: WakeScope,
    /// A wake arrived that no frame has taken up yet.
    woken: Arc<AtomicBool>,
}

impl RootWake {
    pub(super) fn new() -> Self {
        let mut scope = WakeScope::new();
        let woken = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        // A fresh scope is open and unbound.
        let _ = scope.bind(move |_| flag.store(true, Ordering::Release));
        Self { scope, woken }
    }

    /// Run what follows in the root's wake scope.
    pub(super) fn enter(&self) -> WakeScopeGuard {
        self.scope.enter()
    }

    /// Whether a wake is waiting for a frame.
    pub(super) fn pending(&self) -> bool {
        self.woken.load(Ordering::Acquire)
    }

    /// Take up the waiting wake, if any.
    pub(super) fn take(&self) -> bool {
        self.woken.swap(false, Ordering::AcqRel)
    }
}

impl Drop for RootWake {
    fn drop(&mut self) {
        self.scope.close();
    }
}

impl super::FrameHarness {
    /// Whether a wake waits for a frame: the mounted component's, or the
    /// bare root's.
    pub(super) fn wake_pending(&self) -> bool {
        self.component_wake
            .as_ref()
            .map_or_else(|| self.root_wake.pending(), |wake| wake.dirty.get())
    }

    /// Take up the wake waiting for this frame, if any.
    pub(super) fn take_wake(&self) -> bool {
        self.component_wake
            .as_ref()
            .map_or_else(|| self.root_wake.take(), |wake| wake.dirty.replace(false))
    }

    /// Enter the scope the root's wakes reach this harness through, as the
    /// live window enters its App's around sync and draw.
    pub(super) fn enter_wake(&self) -> Option<WakeScopeGuard> {
        match &self.component_wake {
            Some(wake) => wake.binding.lifetime.enter_if_open().ok(),
            None => Some(self.root_wake.enter()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::mpsc;

    use taffy::{NodeId, Size, Style, TaffyTree};

    use crate::state::State;
    use crate::theme::Theme;
    use crate::widget::{TaffyTreeExt, Widget};

    use super::super::Framed;

    /// A root that starts a read on another thread on its first sync or
    /// layout, as Settings starts a device read for the pane it builds, and
    /// takes its result in at a sync.
    struct Reader {
        in_layout: bool,
        rect: vello::kurbo::Rect,
        release: Option<mpsc::Sender<()>>,
        landed: Option<mpsc::Receiver<u32>>,
        value: Option<u32>,
        syncs: usize,
        dirty: bool,
    }

    impl Widget for Reader {
        fn sync(&mut self, _state: &crate::widget::AppModel, _path: &crate::widget::ModelPath) {
            self.syncs += 1;
            if let Some(landed) = &self.landed
                && let Ok(value) = landed.try_recv()
            {
                self.value = Some(value);
                self.landed = None;
                self.dirty = true;
            }
            if !self.in_layout {
                self.start();
            }
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            if self.in_layout {
                self.start();
            }
            let style = Style {
                size: Size::from_lengths(80.0, 40.0),
                ..Default::default()
            };
            taffy.widget_leaf(style, node_map, self)
        }

        crate::rect_geometry!();
        crate::redraw_flag!();

        fn name(&self) -> &'static str {
            "Reader"
        }
    }

    impl Reader {
        fn start(&mut self) {
            if self.landed.is_some() || self.value.is_some() {
                return;
            }
            let (release, gate) = mpsc::channel::<()>();
            let (answer, landed) = mpsc::channel();
            // Captured here, in the frame: the scope the root's frames run in.
            let wake = crate::wake::WakeHandle::new();
            std::thread::spawn(move || {
                let _ = gate.recv();
                let _ = answer.send(7);
                wake.wake();
            });
            self.release = Some(release);
            self.landed = Some(landed);
        }
    }

    /// A bare root's read in flight costs the idle harness no frame; its
    /// landing wakes exactly one, which takes the result in.
    #[test]
    fn a_read_in_flight_frames_nothing_until_it_lands_then_once() {
        read_in_flight(false);
    }

    /// The same for a read a root starts while it lays out (the pane it
    /// builds).
    #[test]
    fn a_read_started_in_layout_wakes_its_frame() {
        read_in_flight(true);
    }

    fn read_in_flight(in_layout: bool) {
        let reader = Reader {
            in_layout,
            rect: vello::kurbo::Rect::ZERO,
            release: None,
            landed: None,
            value: None,
            syncs: 0,
            dirty: true,
        };
        let mut root = Framed::new(State::new(), reader, (200, 100), Theme::dark());
        let harness = Framed::harness_mut(&mut root);
        let mut settled = false;
        for _ in 0..50 {
            let schedule = harness.frame_schedule();
            if !harness.poll_frame().expect("a frame lays out") {
                settled = true;
                break;
            }
            eprintln!("framed for {schedule:?}");
        }
        assert!(settled, "the window comes to rest after mounting");
        assert!(root.landed.is_some(), "the first sync started the read");
        let harness = Framed::harness_mut(&mut root);
        for _ in 0..20 {
            assert!(
                !harness.poll_frame().expect("a frame lays out"),
                "an idle window with a read in flight frames nothing"
            );
        }
        let syncs = root.syncs;
        root.release
            .take()
            .expect("the read waits")
            .send(())
            .unwrap();
        let harness = Framed::harness_mut(&mut root);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !harness.frame_schedule().needs_redraw {
            assert!(
                std::time::Instant::now() < deadline,
                "the landing wakes the window"
            );
            std::thread::yield_now();
        }
        assert!(harness.poll_frame().expect("a frame lays out"));
        assert!(
            !harness.poll_frame().expect("a frame lays out"),
            "one wake, one frame"
        );
        assert_eq!(root.value, Some(7), "the woken frame took the result in");
        assert!(root.syncs > syncs, "it synced to take it in");
    }
}
