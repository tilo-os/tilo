//! What an action leaves for the frame after it, and the replies the app is owed: the driver
//! settles them as an idle window does, so whoever reads what an action did reads all of it.

use super::Driver;

/// What [`Driver::settle_if_owed`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwedSettle {
    /// It ran frames until nothing was owed: the frame shows all of it.
    Framed,
    /// Nothing was owed, and it laid nothing out; `moved` when the invocations it dispatched
    /// changed the document.
    Unframed { moved: bool },
}

impl Driver {
    /// Run the frame an action asks for, then frames as an idle live window does until every
    /// reply the app is owed ([`crate::wake::WakeHandle::owe`]) has landed and a frame took it
    /// up: what the action caused off the UI thread (a worker's answer the app asked for in reply
    /// to it) is applied before the action answers, so its effects are the action's and not the
    /// next one's. Waits for wakes, never polls.
    pub fn settle_owed(&mut self) {
        let handle = self.wake_handle();
        handle.settle(|| {
            self.relayout();
            let before = crate::doc::settle_mark(&self.app.state.doc);
            self.take_up_acted();
            // As the live window idles: a frame after the idle pass only when it changed the
            // document or something asked for one.
            if crate::doc::settle_mark(&self.app.state.doc) != before
                || self.frame_schedule_demands_frame()
            {
                self.relayout();
            }
        });
    }

    /// What an action leaves for the frame after it, taken up as the live window's idle pass takes
    /// it up, without a frame of everything the view shows: the invocations it submitted are
    /// dispatched, the jobs running are polled, the record's writes are offered its writer and the
    /// owner models a worker woke are served. Frames run ([`Self::settle_owed`]) only while the
    /// app is owed a reply or one landed and woke it. An action that only changed the document
    /// costs that change: whoever reads the view next lays it out.
    pub fn settle_if_owed(&mut self) -> OwedSettle {
        let before = crate::doc::settle_mark(&self.app.state.doc);
        self.take_up_acted();
        let moved = crate::doc::settle_mark(&self.app.state.doc) != before;
        let owes = self.wake_handle().owes();
        if !owes && !self.app.wake_pending() {
            return OwedSettle::Unframed { moved };
        }
        self.settle_owed();
        OwedSettle::Framed
    }

    /// Take up what an action left for the frame after it, laying nothing out: the invocations it
    /// submitted are dispatched to the document, the jobs running are polled, the record's writes
    /// are offered its writer and the owner models a worker woke are served. A wake that came
    /// meanwhile is taken by the frame's pump: whoever next lays the view out takes up what it
    /// brought.
    pub fn take_up_acted(&mut self) {
        let _ = self.app.dispatch_control_invocations();
        let _ = self.app.poll_model_invocations();
        self.tick_writes();
        if self.has_pending_owner_models()
            && let Err(error) = self.service_pending_owner_models()
        {
            tracing::warn!(
                ?error,
                "a woken owner model was refused; retry remains pending"
            );
        }
    }

    fn frame_schedule_demands_frame(&self) -> bool {
        let schedule = self.frame_schedule();
        schedule.needs_redraw
            || schedule.needs_animation_frame
            || schedule
                .next_wake_deadline
                .is_some_and(|wake| wake <= crate::time::now())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::mpsc;

    use taffy::{NodeId, Size, Style, TaffyTree};

    use crate::app::driver::Driver;
    use crate::state::State;
    use crate::theme::Theme;
    use crate::widget::{TaffyTreeExt, Widget};

    /// A root that starts a read on another thread on its first sync or layout, as Settings starts
    /// a device read for the pane it builds, and takes its result in at a sync.
    struct Reader {
        in_layout: bool,
        rect: vello::kurbo::Rect,
        release: std::cell::RefCell<Option<mpsc::Sender<()>>>,
        landed: Option<mpsc::Receiver<u32>>,
        value: Option<u32>,
        syncs: usize,
        dirty: bool,
        layout_dirty: bool,
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

        crate::rect_geometry!(measured);
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
            let (release, check) = mpsc::channel::<()>();
            let (answer, landed) = mpsc::channel();
            let wake = crate::wake::WakeHandle::new();
            std::thread::spawn(move || {
                let _ = check.recv();
                let _ = answer.send(7);
                wake.wake();
            });
            self.release.replace(Some(release));
            self.landed = Some(landed);
        }
    }

    /// A bare root's read in flight costs the idle driver no frame; its landing wakes exactly one,
    /// which takes the result in. The same whether the read starts in a sync or in layout.
    #[test]
    fn a_read_in_flight_frames_nothing_until_it_lands_then_once() {
        for (case, in_layout) in [
            ("a read started in sync", false),
            ("a read started in layout", true),
        ] {
            read_in_flight(case, in_layout);
        }
    }

    fn read_in_flight(case: &str, in_layout: bool) {
        // The expects below name no case: a failure's output ends with this line.
        eprintln!("case: {case}");
        let reader = Reader {
            in_layout,
            rect: vello::kurbo::Rect::ZERO,
            release: std::cell::RefCell::new(None),
            landed: None,
            value: None,
            syncs: 0,
            dirty: true,
            layout_dirty: true,
        };
        let mut driver = Driver::for_root(State::new(), reader, (200, 100), Theme::dark());
        let mut settled = false;
        for _ in 0..50 {
            let schedule = driver.frame_schedule();
            if !driver.poll_frame().expect("a frame lays out") {
                settled = true;
                break;
            }
            eprintln!("framed for {schedule:?}");
        }
        assert!(settled, "{case}: the window comes to rest after mounting");
        assert!(
            driver.root::<Reader>().landed.is_some(),
            "{case}: the first sync started the read"
        );
        for _ in 0..20 {
            assert!(
                !driver.poll_frame().expect("a frame lays out"),
                "{case}: an idle window with a read in flight frames nothing"
            );
        }
        let syncs = driver.root::<Reader>().syncs;
        // Taken through the root without a frame: the landing is the frame this looks for.
        driver
            .root::<Reader>()
            .release
            .borrow_mut()
            .take()
            .expect("the read waits")
            .send(())
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !driver.frame_schedule().needs_redraw {
            assert!(
                std::time::Instant::now() < deadline,
                "{case}: the landing wakes the window"
            );
            std::thread::yield_now();
        }
        assert!(
            driver.poll_frame().expect("a frame lays out"),
            "{case}: the landing asks for a frame"
        );
        assert!(
            !driver.poll_frame().expect("a frame lays out"),
            "{case}: one wake, one frame"
        );
        assert_eq!(
            driver.root::<Reader>().value,
            Some(7),
            "{case}: the woken frame took the result in"
        );
        assert!(
            driver.root::<Reader>().syncs > syncs,
            "{case}: it synced to take it in"
        );
    }
}
