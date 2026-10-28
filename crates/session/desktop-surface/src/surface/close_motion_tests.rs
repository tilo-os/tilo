//! How the strip moves when a window goes away: the windows left close the
//! gap in one short motion, what the person was looking at stays where it
//! is, and the strip pans only when its range shrinks below where it is.
//! Five windows of the default width on a 1280 px desktop.

use super::*;

struct Notices;

impl TileSource for Notices {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

/// One frame of the strip: its pan and each window's drawn left edge.
#[derive(Debug, Clone)]
struct Frame {
    scroll: f64,
    windows: Vec<(GroupId, f64)>,
}

impl Frame {
    fn x(&self, group: GroupId) -> Option<f64> {
        self.windows
            .iter()
            .find(|(id, _)| *id == group)
            .map(|&(_, x)| x)
    }
}

struct Desk {
    frame: ui::headless::FrameHarness,
    handle: DesktopHandle,
    /// Each window's group and tab, in strip order.
    windows: Vec<(GroupId, TabId)>,
    /// The held clock the frames run on.
    now: Instant,
}

impl Desk {
    /// Five windows, the strip panned to show window `focus`.
    fn focused_on(focus: usize) -> Self {
        let mut minter = IdMinter::new(7);
        let tabs = (0..5)
            .map(|i| Tab {
                id: minter.mint(),
                title: format!("App{i}"),
                tile: tilo_desktop_model::TileContent::LocalApp {
                    app_id: format!("so.tilo.app{i}"),
                    label: format!("App{i}"),
                    note: String::new(),
                },
                app_title: None,
            })
            .collect();
        let tree = seed_tree(&mut minter, "Desk", tabs);
        let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices));
        let mut frame = ui::headless::FrameHarness::for_root_with(
            ui::state::State::new(),
            surface,
            (1280, 800),
            ui::headless::FrameOptions::default(),
        );
        for _ in 0..4 {
            frame.relayout();
        }
        let windows = handle.read(|tree| {
            tree.workspaces[0].strips[0]
                .window_groups
                .iter()
                .map(|g| (g.id, g.tabs[0].id))
                .collect()
        });
        let mut strip = Self {
            frame,
            handle,
            windows,
            now: ui::time::now(),
        };
        strip.handle.focus_tab(strip.windows[focus].1);
        strip.frames(40);
        strip
    }

    /// The strip as drawn at the held clock's present frame.
    fn sample(&self) -> Frame {
        let row = self.frame.root::<DesktopSurface>().shown_row_ref().unwrap();
        tilo_clock::testing::with_monotonic_time(self.now, || Frame {
            scroll: row.drawn_scroll_x(),
            windows: row
                .groups
                .iter()
                .map(|g| (g.id, g.get_rect().x0 + row.shift_offset_for(g.id)))
                .collect(),
        })
    }

    /// `n` frames of a 60 Hz display on a held clock, the one the close and
    /// the strip animate by, sampled after each.
    fn frames(&mut self, n: usize) -> Vec<Frame> {
        (0..n)
            .map(|_| {
                self.now = self.now.checked_add(Duration::from_millis(16)).unwrap();
                tilo_clock::testing::with_monotonic_time(self.now, || self.frame.relayout());
                self.sample()
            })
            .collect()
    }

    /// Close window `index` as its close control does.
    fn close(&self, index: usize) {
        let tab = self.windows[index].1;
        let route = self
            .handle
            .snapshot("g", vec![])
            .entries
            .into_iter()
            .find(|e| e.route.tab == tab.to_string())
            .unwrap()
            .route;
        self.handle
            .dispatch(
                &navigation::Command::Close {
                    generation: "g".into(),
                    route,
                },
                "g",
            )
            .unwrap();
    }

    fn group(&self, index: usize) -> GroupId {
        self.windows[index].0
    }
}

/// The distance between two windows' left edges: a window and its seam.
fn slot(frame: &Frame) -> f64 {
    frame.windows[1].1 - frame.windows[0].1
}

/// Every window left, and the pan, moves one way only and by at most the
/// closed window's slot in all, and no window in view jumps; the windows in
/// `still` do not move at all. Returns the last frame.
fn assert_calm(case: &str, before: &Frame, trace: &[Frame], still: &[GroupId]) -> Frame {
    let last = trace.last().unwrap().clone();
    let slot = slot(before);
    let dump = || {
        trace
            .iter()
            .map(|f| {
                let xs: Vec<String> = f.windows.iter().map(|(_, x)| format!("{x:.0}")).collect();
                format!("scroll {:.0}: {}", f.scroll, xs.join(" "))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut tracks: Vec<(String, Vec<f64>)> = vec![(
        "the pan".into(),
        std::iter::once(before.scroll)
            .chain(trace.iter().map(|f| f.scroll))
            .collect(),
    )];
    for &(id, x) in &last.windows {
        let path: Vec<f64> = std::iter::once(before.x(id))
            .chain(trace.iter().map(|f| f.x(id)))
            .flatten()
            .collect();
        assert!(
            path.len() > 1 && before.x(id).is_some(),
            "{case}: window {id} was there before, at {x}"
        );
        tracks.push((format!("window {id}"), path));
    }
    for (what, path) in &tracks {
        let travel = path.last().unwrap() - path.first().unwrap();
        assert!(
            travel.abs() <= slot + 1.0,
            "{case}: {what} travels {travel:.0} px, more than the closed slot\n{}",
            dump()
        );
        for step in path.windows(2) {
            let d = step[1] - step[0];
            assert!(
                d * travel.signum() >= -0.5,
                "{case}: {what} turns back by {d:.1} px\n{}",
                dump()
            );
            // The pan, and windows out of view, may move at once where
            // nothing shown moves with them.
            let seen = |x: f64| x > -slot && x < 1280.0;
            assert!(
                what == "the pan" || !(seen(step[0]) || seen(step[1])) || d.abs() <= slot / 2.0,
                "{case}: {what} jumps {d:.0} px in a frame\n{}",
                dump()
            );
        }
    }
    for &id in still {
        let from = before.x(id).unwrap();
        for f in trace {
            let x = f.x(id).unwrap();
            assert!(
                (x - from).abs() < 1.0,
                "{case}: window {id}, in view, moved from {from:.0} to {x:.0}\n{}",
                dump()
            );
        }
    }
    last
}

#[test]
fn a_window_closed_left_of_the_view_takes_nothing_on_screen_with_it() {
    for (focus, close) in [(4, 1), (2, 0), (3, 0)] {
        let mut strip = Desk::focused_on(focus);
        let before = strip.sample();
        strip.close(close);
        let trace = strip.frames(40);
        let slot = slot(&before);
        let shown: Vec<GroupId> = (close + 1..5)
            .map(|i| strip.group(i))
            .filter(|&g| before.x(g).is_some_and(|x| x > -slot && x < 1280.0))
            .collect();
        let last = assert_calm(
            &format!("focused on {focus}, closing {close}"),
            &before,
            &trace,
            &shown,
        );
        assert!(
            (before.scroll - last.scroll - slot).abs() < 1.0,
            "focused on {focus}, closing {close}: the pan follows the canvas \
             before the view, from {} to {}",
            before.scroll,
            last.scroll
        );
        assert_eq!(last.windows.len(), 4, "the closed window is gone");
    }
}

#[test]
fn a_window_closed_right_of_the_view_moves_nothing_shown() {
    let mut strip = Desk::focused_on(2);
    let before = strip.sample();
    strip.close(4);
    let trace = strip.frames(40);
    let shown = [strip.group(1), strip.group(2), strip.group(3)];
    let last = assert_calm("closing 4", &before, &trace, &shown);
    assert!((last.scroll - before.scroll).abs() < 0.5);
}

#[test]
fn the_window_right_of_a_closed_one_slides_into_its_place() {
    let mut strip = Desk::focused_on(2);
    let before = strip.sample();
    strip.close(2);
    let trace = strip.frames(40);
    let last = assert_calm(
        "closing the focused 2",
        &before,
        &trace,
        &[strip.group(0), strip.group(1)],
    );
    assert!((last.scroll - before.scroll).abs() < 0.5, "the strip stays");
    let (closed, next) = (strip.group(2), strip.group(3));
    assert!(
        (last.x(next).unwrap() - before.x(closed).unwrap()).abs() < 1.0,
        "window 3 takes window 2's place"
    );
}

#[test]
fn closing_the_last_window_pans_only_as_far_as_the_strip_shrinks() {
    let mut strip = Desk::focused_on(4);
    let before = strip.sample();
    strip.close(4);
    let trace = strip.frames(40);
    let last = assert_calm("closing the last", &before, &trace, &[]);
    let row = strip
        .frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .unwrap();
    assert!(
        (last.scroll - row.max_scroll_x()).abs() < 0.5,
        "the strip rests at the end of its windows, {} not {}",
        row.max_scroll_x(),
        last.scroll
    );
    let left = last.x(strip.group(3)).unwrap();
    assert!(
        left > before.x(strip.group(3)).unwrap() && left >= 0.0,
        "its neighbour comes into view, at {left}"
    );
}

/// The room hands a tree back while the windows move, or a peer's tree
/// without a window arrives: the strip carries on as when the person closed
/// it here, rather than being rebuilt at home with every window cut to its
/// place.
#[test]
fn a_tree_replaced_from_the_room_moves_the_strip_as_a_close_here_does() {
    // The room echoes the person's own close mid-motion.
    for (focus, close, still) in [(4, 4, vec![]), (2, 2, vec![0, 1]), (4, 1, vec![2, 3, 4])] {
        let mut strip = Desk::focused_on(focus);
        let before = strip.sample();
        strip.close(close);
        let mut trace = strip.frames(3);
        strip
            .handle
            .replace_shared_tree(strip.handle.shared_tree(), &[]);
        trace.extend(strip.frames(40));
        let still: Vec<GroupId> = still.into_iter().map(|i| strip.group(i)).collect();
        let last = assert_calm(
            &format!("echo while closing {close}, focused on {focus}"),
            &before,
            &trace,
            &still,
        );
        assert_eq!(last.windows.len(), 4);
    }
    // A peer closes a window.
    for (focus, close, still) in [
        (2, 2, vec![0, 1]),
        (4, 1, vec![2, 3, 4]),
        (2, 4, vec![1, 2, 3]),
    ] {
        let mut strip = Desk::focused_on(focus);
        let before = strip.sample();
        let mut tree = strip.handle.shared_tree();
        tree.remove_tab(strip.windows[close].1);
        strip.handle.replace_shared_tree(tree, &[]);
        let trace = strip.frames(40);
        let still: Vec<GroupId> = still.into_iter().map(|i| strip.group(i)).collect();
        let last = assert_calm(
            &format!("a peer closing {close}, focused on {focus}"),
            &before,
            &trace,
            &still,
        );
        assert_eq!(last.windows.len(), 4);
    }
}
