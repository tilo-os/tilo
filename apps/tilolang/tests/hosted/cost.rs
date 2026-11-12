//! What the interactions of a window of a workspace a person has used for years cost: the
//! table, read against what a person feels as instant, and the opens that read the workspace's
//! index rather than the workspace.

use super::*;

use std::collections::BTreeMap;

// -- What the interactions cost ------------------------------------------------------------
//
// A person's window of a workspace they have used for years: ten thousand tasks, driven
// through the interactions they do all day, each measured from the input reaching the window to
// the window at rest with what it did on screen. The step is the work the window's process did
// for it (UI-thread CPU time, so a machine busy with other work reads what the window made it
// do rather than the moments it was descheduled), split into what the instance did (running the
// action and evaluating the views), the window's sync (taking the views' patches), its layout
// and its paint.
//
// The table is read against what a person feels as instant, and against what the instance's
// evaluation and the layout cost within it: a keystroke must not wait for the workspace's size,
// and neither must a place switch, a check-off, a page or a scroll. The targets do not move
// with the machine: a step over one is reported with its gap, and a step past what it is held to
// (what it costs now, with room for a loaded machine) fails. The gate on the work a change does
// is also the instruction count the `budgets` test holds.

/// The workspace the interactions are measured in: what a person who has used Tasks for years
/// keeps.
const SEEDED: usize = 10_000;

/// A workspace ten times the size again: opening it costs what opening the first does, since the
/// open reads the workspace's index rather than the workspace.
const SEEDED_LARGE: usize = 100_000;

/// What a person feels as instant for one interaction, in UI-thread CPU microseconds: the
/// whole step (the input, the instance's answer and every frame that shows it), and the
/// instance's evaluation and the window's layout within it.
#[derive(Clone, Copy, Debug)]
struct Target {
    step_us: u64,
    eval_layout_us: u64,
    /// The step's CPU time it is held to, past which the test fails: see [`Held`].
    held: Held,
}

/// What a step's CPU time is held to, in an optimized build and in a development one, whose
/// proofs and unoptimized code cost several times as much: what the step costs now, with room
/// for a loaded machine. A step past it got slower by a clear margin, which no machine's noise
/// explains; the target is where the step is brought down to, and this follows it down.
#[derive(Clone, Copy, Debug)]
struct Held {
    optimized_us: u64,
    development_us: u64,
}

impl Held {
    /// What this build holds the step to.
    fn here(self) -> u64 {
        if cfg!(debug_assertions) {
            self.development_us
        } else {
            self.optimized_us
        }
    }
}

/// One interaction, measured once.
#[derive(Clone, Copy, Debug)]
struct Cost {
    label: &'static str,
    /// UI-thread CPU time of the step.
    cpu_us: u64,
    /// Wall time of the same span.
    wall_us: u64,
    /// The instance's share: the host's turns, running the action and evaluating the views.
    instance_us: u64,
    /// The window's frames: event dispatch, sync, layout and paint together.
    window_us: u64,
    /// The frames' event dispatch, summed.
    event_us: u64,
    /// The frames' sync, taking the instance's patches, summed.
    sync_us: u64,
    /// The frames' layout, summed.
    layout_us: u64,
    /// The frames' paint, summed.
    paint_us: u64,
    /// From the input to the frame's commit that showed what it did.
    shown_us: u64,
    /// The frames the step took.
    frames: u64,
}

impl Cost {
    fn eval_layout_us(&self) -> u64 {
        self.instance_us.saturating_add(self.layout_us)
    }
}

/// What the interactions cost, per interaction.
#[derive(Default)]
struct Costs(BTreeMap<&'static str, Vec<Cost>>);

impl Costs {
    fn add(&mut self, cost: Cost) {
        self.0.entry(cost.label).or_default().push(cost);
    }

    fn p50(&self, label: &str, of: fn(&Cost) -> u64) -> u64 {
        quantile(self.values(label, of), 0.5)
    }

    fn values(&self, label: &str, of: fn(&Cost) -> u64) -> Vec<u64> {
        self.0
            .get(label)
            .map(|costs| costs.iter().map(&of).collect())
            .unwrap_or_default()
    }

    /// The table, in milliseconds, to stderr.
    #[allow(
        clippy::print_stderr,
        reason = "a measurement reports the table it measured"
    )]
    fn print(&self) {
        let ms = |us: u64| format!("{:.2}", us as f64 / 1000.0);
        eprintln!(
            "interaction        steps  frames  cpu_p50  cpu_max  shown  instance  window  event  sync  layout  paint  wall"
        );
        for label in self.0.keys() {
            eprintln!(
                "{label:<18} {:>5} {:>7} {:>8} {:>8} {:>6} {:>9} {:>7} {:>6} {:>5} {:>7} {:>6} {:>5}",
                self.0[label].len(),
                self.p50(label, |c| c.frames),
                ms(self.p50(label, |c| c.cpu_us)),
                ms(quantile(self.values(label, |c| c.cpu_us), 1.0)),
                ms(self.p50(label, |c| c.shown_us)),
                ms(self.p50(label, |c| c.instance_us)),
                ms(self.p50(label, |c| c.window_us)),
                ms(self.p50(label, |c| c.event_us)),
                ms(self.p50(label, |c| c.sync_us)),
                ms(self.p50(label, |c| c.layout_us)),
                ms(self.p50(label, |c| c.paint_us)),
                ms(self.p50(label, |c| c.wall_us)),
            );
        }
    }

    /// The reading against the targets every interaction is held to: what it costs, on this
    /// machine, beside what a person would feel as instant, and the gap between them. A reading
    /// over its target is reported; one past what the step is held to ([`Held`]) is returned,
    /// each with its numbers, for the test to fail on.
    #[allow(
        clippy::print_stderr,
        reason = "why a measurement was not held is part of its report"
    )]
    fn report(&self, targets: &[(&'static str, Target)]) -> Vec<String> {
        let ms = |us: u64| format!("{:.2}", us as f64 / 1000.0);
        let gap = |measured: u64, target: u64| {
            if measured == 0 {
                "met".to_owned()
            } else {
                format!("{:.1}x target", measured as f64 / target as f64)
            }
        };
        eprintln!(
            "interaction        cpu p50 vs instant   instance+layout p50 vs instant   held to"
        );
        let mut behind = Vec::new();
        let mut past = Vec::new();
        for (label, target) in targets {
            if !self.0.contains_key(label) {
                past.push(format!("{label}: no interaction measured"));
                continue;
            }
            let cpu = self.p50(label, |cost| cost.cpu_us);
            let eval_layout = self.p50(label, Cost::eval_layout_us);
            let held = target.held.here();
            eprintln!(
                "{label:<18} {:>10} ms ({:>9}), {:>10} ms ({:>9})   {:>8} ms",
                ms(cpu),
                gap(cpu, target.step_us),
                ms(eval_layout),
                gap(eval_layout, target.eval_layout_us),
                ms(held),
            );
            if cpu > target.step_us || eval_layout > target.eval_layout_us {
                behind.push(label.to_string());
            }
            if cpu > held {
                past.push(format!(
                    "{label}: {} ms of CPU, held to {} ms (instant is {} ms)",
                    ms(cpu),
                    ms(held),
                    ms(target.step_us)
                ));
            }
        }
        if !behind.is_empty() {
            eprintln!("not instant yet: {}", behind.join(", "));
        }
        past
    }
}

/// The `q` quantile of `samples` (0.5 the median, 1.0 the worst).
fn quantile(mut samples: Vec<u64>, q: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let last = samples.len().saturating_sub(1);
    let at = ((last as f64) * q).round() as usize;
    samples[at.min(last)]
}

/// The UI thread's CPU time since `start`, in microseconds: the wall time where the platform
/// keeps no CPU clock.
fn since(start: Option<ui::time::Duration>, wall: ui::time::Duration) -> u64 {
    start
        .zip(ui::rt::thread_cpu_time())
        .map_or(wall.as_micros(), |(start, now)| {
            now.saturating_sub(start).as_micros()
        }) as u64
}

/// A window at rest: the host's turns and the window's frames until neither has anything left to
/// do, the CPU time each took in microseconds, the frames' timings, and what the layout was
/// asked to do along the way.
#[derive(Default)]
struct Rest {
    instance_us: u64,
    window_us: u64,
    frames: Vec<ui::profiling::FrameTiming>,
    /// The draw commands the step's frames encoded.
    drawn: u64,
    /// The nodes the most demanding frame of them invalidated in the layout.
    marked: usize,
    /// The frames that placed every widget of the tree, not only those a view moved.
    full: u64,
    /// The frames that placed every widget with nothing composed, measured or remounted: a walk
    /// of the whole tree where only a view moved.
    full_unasked: u64,
    /// The rounds of turn and frame the wait took.
    rounds: u32,
}

/// The session's window on the frame clock the session's own host presents on
/// ([`tilo_ui_host::AppWindow`] under `ui::headless::FrameHarness`): the key down, the text and
/// the key up of one keystroke reach one frame, as they do in a window whose host draws on the
/// display's clock, instead of running a frame each.
fn on_the_frame_clock(mut session: Session) -> Session {
    session.window.harness().set_coalesce_input_frames(true);
    session
}

/// The frame the window runs now: the frame of what the host just did (its patches wake the
/// window and the frame reads them), or one the window owes the tree that no turn brought — the
/// frame an input waits for on the window's clock, whose events reached the widgets when they
/// came. A window at rest runs none: asking it by hand (`Window::frame`) is asking it to lay out
/// and draw again, which is work no person's interaction made. A frame only the window's own
/// clock asks for is not a step's cost either: these tests settle motion, and the frames a
/// scroll's glide ran would be as many as the machine got through. What the window owes nothing
/// for is asserted where the schedule is read (`a_window_at_rest_asks_its_host_for_no_frame`).
fn frame_if_asked(session: &mut Session, changed: bool) -> bool {
    match &mut session.window {
        Window::Harness(window) => {
            if changed {
                window.relayout();
                return true;
            }
            if !window.frame_schedule().queued_work {
                return false;
            }
            window.poll_frame().unwrap_or(true)
        }
        Window::Live(app) => {
            Window::live_frame(app);
            true
        }
    }
}

/// The host's turns and the window's frames until neither has anything left to do: the step's
/// work, not the wakes the app's other threads hand the window while it waits.
fn at_rest(session: &mut Session, drawn_before: u64) -> Rest {
    let harness = session.window.harness();
    let mut rest = Rest {
        // What the input drew before the waits below is the step's drawing: a window on the
        // frame clock runs no frame of its own for an event, one not on it runs one per event.
        drawn: harness.drawn_commands().saturating_sub(drawn_before),
        frames: harness.take_profile_samples(),
        ..Rest::default()
    };
    let mut quiet = 0_u32;
    while quiet < 3 {
        let start = ui::rt::thread_cpu_time();
        let wall = ui::time::Instant::now();
        let changed = session.host.turn();
        rest.instance_us += since(start, wall.elapsed());
        // What the tree itself owes a frame: the work of the step, or of the turn just taken.
        let sources = session.window.harness().queued_work_sources();
        let step_frame = changed || !sources.is_empty();
        let drawn_round = session.window.harness().drawn_commands();
        let start = ui::rt::thread_cpu_time();
        let wall = ui::time::Instant::now();
        let framed = frame_if_asked(session, changed);
        rest.window_us += since(start, wall.elapsed());
        if framed {
            // A frame the window ran for a wake that no source of the tree asked for is one
            // another of the app's threads handed it: the window takes up the answer it carries
            // and goes back to rest. It is not the step's frame, so it holds the wait off rest no
            // more than it is counted as the step's work.
            let samples = session.window.harness().take_profile_samples();
            if step_frame {
                rest.frames.extend(samples);
                rest.drawn += session
                    .window
                    .harness()
                    .drawn_commands()
                    .saturating_sub(drawn_round);
                let stats = session.window.harness().last_relayout();
                rest.marked = rest.marked.max(stats.marked_dirty);
                if !stats.placed_partly {
                    rest.full = rest.full.saturating_add(1);
                    if !(stats.rebuilt || stats.remounted || stats.measured)
                        && stats.marked_dirty == 0
                    {
                        rest.full_unasked = rest.full_unasked.saturating_add(1);
                    }
                }
            }
        }
        // The step is over when the host has nothing left to show the window and the tree owes
        // nothing of its own. What an instance reads off its frames — its publication sweeping
        // the workspace's records, one a turn, waking the window as each moves what a view shows
        // — is the workspace's work, sized by the workspace and not by what the person did, so
        // it does not hold the window here.
        if step_frame {
            quiet = 0;
        } else {
            quiet = quiet.saturating_add(1);
        }
        rest.rounds = rest.rounds.saturating_add(1);
        // A window whose work never ends is a window that never rests: the bound is far past any
        // step's frames, and what kept it awake is what the failure says.
        if rest.rounds > 400 {
            let window = session.window.harness();
            panic!(
                "the window rested never: {:?} {:?} {:?} {:?}",
                window.queued_work_sources(),
                window.redraw_sources(),
                window.frame_schedule(),
                window.last_relayout()
            );
        }
    }
    rest
}

/// The window's open taken to rest as a window takes it: the host's turns, and a frame only when
/// a turn changed what it shows or its tree owes one ([`frame_if_asked`]), until the workspace's
/// index is whole and nothing is left to do. A frame drawn on every turn regardless is work no
/// window at rest does.
fn opened_at_rest(session: &mut Session) {
    let deadline = std::time::Instant::now() + WAKE_WAIT;
    let mut quiet = 0_u32;
    let mut rounds = 0_u32;
    while quiet < 3 {
        let changed = session.host.turn();
        let framed = frame_if_asked(session, changed);
        let waiting = (!session.index_whole() || session.search_index_pending())
            && std::time::Instant::now() < deadline;
        if changed || framed || waiting {
            quiet = 0;
        } else {
            quiet = quiet.saturating_add(1);
        }
        if !changed && waiting {
            session.woken.wait(WAKE_WAIT);
        }
        rounds = rounds.saturating_add(1);
        assert!(
            rounds < 10_000,
            "the window never came to rest after opening"
        );
    }
}

/// One interaction, measured from `input` reaching the window to the windows' work at rest with
/// what it did on screen.
fn cost(session: &mut Session, label: &'static str, input: impl FnOnce(&mut Session)) -> Cost {
    session.window.harness().begin_profiling();
    let drawn = session.window.harness().drawn_commands();
    let start = ui::rt::thread_cpu_time();
    let wall = ui::time::Instant::now();
    input(session);
    // The frames the input itself ran are the window's too.
    let input_us = since(start, wall.elapsed());
    let rest = at_rest(session, drawn);
    let (frames, instance_us, window_us) = (
        rest.frames,
        rest.instance_us,
        rest.window_us.saturating_add(input_us),
    );
    let wall_us = wall.elapsed().as_micros() as u64;
    let cpu_us = start
        .zip(ui::rt::thread_cpu_time())
        .map_or(wall_us, |(start, now)| {
            now.saturating_sub(start).as_micros() as u64
        });
    let sum = |of: fn(&ui::profiling::FrameTiming) -> u64| frames.iter().map(of).sum::<u64>();
    Cost {
        label,
        cpu_us,
        wall_us,
        instance_us,
        window_us,
        event_us: sum(|frame| frame.handle_event_us),
        sync_us: sum(|frame| frame.sync_us),
        layout_us: sum(|frame| frame.build_layout_us.saturating_add(frame.apply_layout_us)),
        paint_us: sum(|frame| frame.draw_us),
        shown_us: frames.last().map_or(0, |frame| frame.input_to_commit_us),
        frames: frames.len() as u64,
    }
}

/// What a person feels as instant, for the interactions this measures: 8 ms for a keystroke,
/// 16 ms for a step a person waits on, 8 ms for a frame of scrolling under the pointer, and
/// 300 ms to open ten thousand tasks — the one step that reads the whole workspace, once. They
/// are the targets, not what this workspace costs: a step over one is reported with its gap (see
/// [`Costs::report`]), and one past what it is held to ([`Held`]) fails.
///
/// The same targets, counted rather than timed, are how a change is gated: a modern core of
/// 3.5 GHz at an IPC of 2.5 retires 8.75 million instructions a millisecond, so 8 ms is 70
/// million instructions, 16 ms 140 million and 300 ms 2.6 billion, and a callgrind run counts
/// the same work the same on every machine. This test reports the times and fails a step past
/// what it is held to; [`tilolang_tilo::bench::Work`] and `tilolang-tilo`'s `budgets` test hold
/// the counts (a change publishes the rows it touched, whatever the workspace holds). Opening a
/// workspace reads the workspace's index rather than the workspace:
/// [`opening_a_window_reads_the_workspaces_index`].
const TARGETS: &[(&str, Target)] = &[
    (
        "open",
        Target {
            step_us: 300_000,
            eval_layout_us: 300_000,
            held: Held {
                optimized_us: 620_000,
                development_us: 2_400_000,
            },
        },
    ),
    (
        "entry keystroke",
        Target {
            step_us: 8_000,
            eval_layout_us: 8_000,
            held: Held {
                optimized_us: 18_000,
                development_us: 70_000,
            },
        },
    ),
    (
        "entry enter",
        Target {
            step_us: 16_000,
            eval_layout_us: 16_000,
            held: Held {
                optimized_us: 110_000,
                development_us: 500_000,
            },
        },
    ),
    (
        "check a task",
        Target {
            step_us: 16_000,
            eval_layout_us: 16_000,
            held: Held {
                optimized_us: 96_000,
                development_us: 500_000,
            },
        },
    ),
    (
        "switch place",
        Target {
            step_us: 16_000,
            eval_layout_us: 16_000,
            held: Held {
                optimized_us: 90_000,
                development_us: 340_000,
            },
        },
    ),
    (
        "open a task page",
        Target {
            step_us: 16_000,
            eval_layout_us: 16_000,
            held: Held {
                optimized_us: 90_000,
                development_us: 470_000,
            },
        },
    ),
    (
        "scroll",
        Target {
            step_us: 8_000,
            eval_layout_us: 8_000,
            held: Held {
                optimized_us: 25_000,
                development_us: 110_000,
            },
        },
    ),
];

/// The workspace a person who has used Tasks for years keeps: `n` tasks over the chunks the
/// store keeps them in (a run of 2048 tasks a chunk), as the actions that add them one at a
/// time write them, in the record the installed Tasks package runs over.
pub(super) fn seeded_tasks(n: usize) -> tilolang_tilo::chunk::Chunks {
    let tasks = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(package_of(TASKS));
    let packages = tilolang_tilo::bench::compile(&tasks).expect("the tasks package compiles");
    tilolang_tilo::bench::seeded_chunks(&packages, n).expect("the workspace seeds")
}

/// The title of the list's first row: where the list starts, and what a person reads there.
fn top_title(session: &mut Session) -> String {
    session
        .titles()
        .first()
        .cloned()
        .expect("the list shows a row")
}

/// The row at the top of what the window shows: the first whose top is on screen. The rows
/// built around it reach past the window's edges, so the first of those can stay as the list
/// scrolls under a row grown into its page.
fn top_shown(session: &mut Session) -> String {
    accessible(session)
        .into_iter()
        .filter(|node| node.role == ui::a11y::Role::ListItem)
        .find(|node| node.bounds.is_some_and(|bounds| bounds.y0 >= 0.0))
        .and_then(|node| node.name)
        .expect("the window shows a row")
}

/// The first row the list shows that the person has not checked yet, its title, and where
/// their press checks it off: the leading checkbox the row draws, in the row's start, as tall
/// as its box.
///
/// A row a person checks stays where it stands, reading checked and muted, until the window
/// leaves the place: their own change never moves a row out from under their pointer
/// (`tilolang_runtime::live::virtual_rows`), so the rows to check next are the ones after it.
fn first_check(session: &mut Session) -> (String, f64, f64) {
    const BOX: f64 = 22.0;
    let node = accessible(session)
        .into_iter()
        .find(|node| {
            node.role == ui::a11y::Role::ListItem
                && node.state.checked != Some(true)
                && node.bounds.is_some()
        })
        .expect("the list shows a row not checked yet");
    let rect = node.bounds.expect("the row is laid out");
    (
        node.name.unwrap_or_default(),
        rect.x0 + 4.0 + BOX / 2.0,
        f64::midpoint(rect.y0, rect.y1),
    )
}

/// Whether the list shows `title` checked off.
fn checked_off(session: &mut Session, title: &str) -> bool {
    accessible(session).into_iter().any(|node| {
        node.role == ui::a11y::Role::ListItem
            && node.name.as_deref() == Some(title)
            && node.state.checked == Some(true)
    })
}

/// Whether the sidebar shows `place` chosen.
fn chosen(session: &mut Session, place: &str) -> bool {
    accessible(session).into_iter().any(|node| {
        node.role == ui::a11y::Role::TreeItem
            && node.name.as_deref() == Some(place)
            && node.state.selected
    })
}

/// What the interactions of a person's window of ten thousand tasks cost: opening it, switching
/// the sidebar's place, typing a task in the entry and pressing Enter, checking a task off,
/// opening a task's page and scrolling the list.
#[test]
fn what_the_interactions_a_person_feels_cost() {
    // The `settled` guard keeps the window's motion out of the measurement: what is measured is
    // the work that shows the result, not the frames that finish its animation.
    let _motion = ui::motion::testing::settled();
    let mut costs = Costs::default();

    // The workspace is seeded before the window opens, and so is the index an earlier run of the
    // app kept: what opening it costs is the window's work over that index, not the workspace's
    // making. Whether the index answers that open is
    // [`opening_a_window_reads_the_workspaces_index`]'s.
    let chunks = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let storage = kept_index(&route, &chunks, "interactions");
    let start = ui::rt::thread_cpu_time();
    let wall = ui::time::Instant::now();
    let mut session =
        on_the_frame_clock(Session::tasks_over_launched(&route, chunks, Some(storage)));
    opened_at_rest(&mut session);
    let wall_us = wall.elapsed().as_micros() as u64;
    let cpu_us = since(start, wall.elapsed());
    costs.add(Cost {
        label: "open",
        cpu_us,
        wall_us,
        instance_us: cpu_us,
        window_us: 0,
        event_us: 0,
        sync_us: 0,
        layout_us: 0,
        paint_us: 0,
        shown_us: 0,
        frames: 0,
    });
    assert!(
        !session.titles().is_empty(),
        "the window opens on the workspace's tasks"
    );

    switch_places(&mut session, &mut costs);
    type_a_task(&mut session, &mut costs);
    check_tasks(&mut session, &mut costs);
    open_pages(&mut session, &mut costs);
    scroll_the_list(&mut session, &mut costs);

    costs.print();
    let past = costs.report(TARGETS);
    assert!(
        past.is_empty(),
        "interactions got slower than they are held to:\n{}",
        past.join("\n")
    );
}

/// What one open of a window did, in counts that do not depend on the machine: the work opening
/// a window costs is the work the window shows, whatever the workspace holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Opened {
    /// Records the instance's publication looked at, one per record per turn.
    published: u64,
    /// Times it read every record in full.
    sweeps: u64,
    /// The actions the instance published to the workspace's surface.
    surface: usize,
    /// The rows the list rendered.
    rows: usize,
    /// What the views sent the window.
    patches: u64,
    /// The entries the instance published to the session's search index.
    indexed: usize,
    /// How many times the window's instance built its index from the record.
    builds: u64,
}

impl Opened {
    /// The work opening the window of the session `session` did.
    fn of(session: &mut Session) -> Self {
        let rows = session.titles().len();
        let (indexed, _removed, _reset) = session.index_published();
        let hosted = session
            .host
            .instances()
            .next()
            .expect("the window's instance");
        let instance = hosted.instance();
        let builds = instance
            .host()
            .map_or(0, |host| host.store().stats().builds);
        Self {
            published: instance.visited_records(),
            sweeps: instance.sweeps(),
            surface: hosted.published().len(),
            rows,
            patches: instance.patches_sent(),
            indexed,
            builds,
        }
    }

    /// The counts, as a line: what a person reading a failure needs to see which of them grew.
    fn line(self) -> String {
        let Self {
            published,
            sweeps,
            surface,
            rows,
            patches,
            indexed,
            builds,
        } = self;
        format!(
            "{published} records published ({sweeps} sweeps), {surface} actions on the surface, \
             {rows} rows, {patches} patches, {indexed} indexed, {builds} builds"
        )
    }
}

/// Opening a window costs what the window shows, not what the workspace holds: the same window
/// over a thousand tasks and over ten thousand does the same work for the frame that opens it —
/// the rows it shows, the actions it offers and the records it publishes — give or take a
/// constant. Opening is the one step the workspace's size must not reach: a person feels it as
/// the window appearing. The search index is read off the frame, so its size is the reader's
/// work, not the open's; it is printed either way.
#[test]
fn opening_a_window_does_the_work_the_window_shows() {
    let _motion = ui::motion::testing::settled();
    let mut opened = Vec::new();
    for &n in &[1_000, SEEDED] {
        let chunks = seeded_tasks(n);
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        // The window as it opens: its index read and the first frame that shows it, which is
        // what a person waits for — not the turns after it.
        let mut session = Session::tasks_over_launched(&route, chunks, None);
        session.wait_index();
        session.host.turn();
        session.window.frame();
        assert!(
            !session.titles().is_empty(),
            "the window opens on the workspace's tasks"
        );
        let work = Opened::of(&mut session);
        eprintln!("open at {n} tasks: {}", work.line());
        opened.push((n, work));
        session.settle();
    }
    let mut sizes = opened.iter();
    let (Some((small_n, small)), Some((large_n, large))) = (sizes.next(), sizes.next()) else {
        panic!("two opens are measured");
    };
    let (small_n, small, large_n, large) = (*small_n, *small, *large_n, *large);
    // What the window shows does not grow with the workspace: a table longer than the window
    // shows no more rows, offers no more actions and publishes no more records. The constant
    // covers what a longer workspace may legitimately add to the work (a page read, a row of
    // a list's tail).
    let mut over = Vec::new();
    let mut hold = |what: &str, small: u64, large: u64, slack: u64| {
        if large > small.saturating_add(slack) {
            over.push(format!(
                "{what}: {large} at {large_n} tasks, {small} at {small_n}"
            ));
        }
    };
    hold("records published", small.published, large.published, 2);
    hold("sweeps", small.sweeps, large.sweeps, 0);
    hold(
        "actions on the surface",
        small.surface as u64,
        large.surface as u64,
        0,
    );
    hold("rows", small.rows as u64, large.rows as u64, 0);
    hold("patches", small.patches, large.patches, 2);
    assert!(
        over.is_empty(),
        "opening a window does the work the window shows:\n{}",
        over.join("\n")
    );
}

/// Opening a window of a workspace a person has used for years: the index an earlier run of the
/// app kept answers what the record holds, so the open reads it and applies only what the record
/// gained since — what a hundred thousand tasks cost to open, ten thousand do. A hundred thousand
/// are seeded in an optimised build only (the shipped code), the one the budgets hold in: a
/// development build measures the ten thousand and reports it without holding it.
#[test]
fn opening_a_window_reads_the_workspaces_index() {
    let _motion = ui::motion::testing::settled();
    let sizes: &[usize] = if cfg!(debug_assertions) {
        &[SEEDED]
    } else {
        &[1_000, SEEDED, SEEDED_LARGE]
    };
    let mut misses = Vec::new();
    for &n in sizes {
        // The record, and the index an earlier run of the app left on the disk.
        let chunks = seeded_tasks(n);
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        let storage = kept_index(&route, &chunks, &format!("open-{n}"));

        let start = ui::rt::thread_cpu_time();
        let wall = ui::time::Instant::now();
        let mut session = Session::tasks_over_launched(&route, chunks, Some(storage));
        let open_us = since(start, wall.elapsed());
        let wall_us = wall.elapsed().as_micros() as u64;
        session.wait_index();
        assert_eq!(
            session.builds(),
            0,
            "the open reads the workspace's index: it does not build one"
        );
        let ready_us = since(start, wall.elapsed());
        eprintln!(
            "open at {n} tasks: CPU {:.2} ms, wall {:.2} ms; reading the index {:.2} ms",
            open_us as f64 / 1000.0,
            wall_us as f64 / 1000.0,
            ready_us as f64 / 1000.0
        );
        if n == SEEDED {
            // The window shows the workspace's tasks from what its index holds. What it then
            // publishes to show them all is the publication's cost, not the open's.
            show_rows(&mut session);
            assert!(
                !session.titles().is_empty(),
                "the window opens on the workspace's tasks"
            );
        }
        let budget = if n == SEEDED {
            OPEN_BUDGET_US
        } else {
            OPEN_BUDGET_LARGE_US
        };
        if open_us > budget {
            misses.push(format!(
                "{n} tasks: {open_us}us of the window's CPU to open, over {budget}us"
            ));
        }
        session.settle();
    }
    if cfg!(debug_assertions) {
        eprintln!("a development build: the open is reported, not held");
        return;
    }
    assert!(
        misses.is_empty(),
        "opening a workspace over its index:\n{}",
        misses.join("\n")
    );
}

/// Opening the same workspace where no index was kept, as a device sees it the first time: every
/// chunk the record names is read once, the index built over them, and kept for the next open.
/// The work is linear in the workspace — a chunk a record names is read when the open reaches it,
/// and what it holds is derived once — so ten thousand tasks cost what their five chunks hold, not
/// a pass over every item for every chunk.
#[test]
fn opening_a_workspace_with_no_index_reads_its_chunks_once() {
    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(SEEDED);
    let all: Vec<tilolang_tilo::chunk::ChunkNo> = chunks.numbers().collect();
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let storage = kept_root("cold");

    let start = ui::rt::thread_cpu_time();
    let wall = ui::time::Instant::now();
    let mut session = Session::tasks_over_launched(&route, chunks, Some(storage));
    session.wait_index();
    let ready_us = since(start, wall.elapsed());
    // The window reads its rows from the first step of the index while the rest of the record is
    // derived off the frame: what the open builds over, and what it reads, is counted once it is
    // whole, not when its first step lands.
    session.settle();
    let wall_us = wall.elapsed().as_micros() as u64;
    assert!(session.builds() > 0, "the first open builds the index");
    assert_eq!(
        session.catch_ups(),
        0,
        "the first open builds over every chunk of the record: none is caught up after it"
    );
    show_rows(&mut session);
    assert!(
        !session.titles().is_empty(),
        "the window opens on the tasks"
    );
    let held = holding(&session);
    assert_eq!(
        held, all,
        "a cold open reads every chunk the record names, once"
    );
    eprintln!(
        "cold open at {SEEDED} tasks over {} chunks: first rows {:.2} ms of CPU, whole {:.2} ms",
        held.len(),
        ready_us as f64 / 1000.0,
        wall_us as f64 / 1000.0
    );
}

/// A window that opens over years of a person's workspace shows its first screen of rows from the
/// first step of the index it builds, not once the whole workspace is read: the chunk the first
/// screen's rows are in is derived first, the rest of the record off the frame while the person
/// reads, and what the store has not read yet shows as not yet known rather than as a count. The
/// first rows cost what the chunk they are in holds, so ten thousand tasks pay about what a
/// thousand do for them, and the lists the window ends on are the lists of a window that opens
/// over the index the cold open kept.
#[test]
fn a_cold_open_shows_its_first_rows_before_it_reads_the_workspace() {
    let _motion = ui::motion::testing::settled();
    let mut measured = Vec::new();
    for &n in &[SEEDED / 10, SEEDED] {
        let chunks = seeded_tasks(n);
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        let storage = kept_root(&format!("first-rows-{n}"));

        let mut session =
            Session::tasks_over_launched(&route, chunks.clone(), Some(storage.clone()));
        // From the window opening, as a person waits on it: the package's build is what every
        // window of the app pays once, and not what the workspace's size adds.
        let start = ui::rt::thread_cpu_time();
        let wall = ui::time::Instant::now();
        show_rows(&mut session);
        let rows_us = since(start, wall.elapsed());
        let shown_us = u64::try_from(wall.elapsed().as_micros()).unwrap_or(u64::MAX);
        shots(&format!("cold-{n}-first-rows"), &mut session);
        assert!(
            !session.titles().is_empty(),
            "the window opens on the tasks's rows"
        );
        let first_rows_shown = session.titles();
        session.settle();
        let whole_us = since(start, wall.elapsed());
        assert!(
            session.index_whole(),
            "the window reads the whole workspace by the time it is at rest"
        );
        let settled = session.titles();
        let shared = settled.len().min(first_rows_shown.len());
        assert_eq!(
            settled[..shared],
            first_rows_shown[..shared],
            "the rows the window opened on stay where they were while the rest is read: \
             {first_rows_shown:?} then {settled:?}"
        );
        shots(&format!("cold-{n}-read"), &mut session);
        let cold = shown(&mut session);
        eprintln!(
            "cold open at {n} tasks: first rows {:.2} ms of the window's CPU, shown after \
             {:.2} ms; whole {:.2} ms",
            rows_us as f64 / 1000.0,
            shown_us as f64 / 1000.0,
            whole_us as f64 / 1000.0
        );
        measured.push((n, rows_us, whole_us));

        // What the window ends on is what a window that opens over a kept index shows.
        session.close();
        let kept = kept_index(&route, &chunks, &format!("first-rows-kept-{n}"));
        let mut warm = Session::tasks_over_in(&route, chunks, Some(kept));
        assert_eq!(
            warm.builds(),
            0,
            "the window over the kept index reads it, rather than building one"
        );
        assert_eq!(
            shown(&mut warm),
            cold,
            "the window at rest shows what a window over the kept index shows"
        );
    }
    if cfg!(debug_assertions) {
        eprintln!("a development build: the first rows are reported, not bounded");
        return;
    }
    let (small_n, small_rows, _) = measured[0];
    let (large_n, large_rows, large_whole) = measured[1];
    assert!(
        large_rows < large_whole,
        "the first rows come before the window has read the whole workspace: {large_rows}us of \
         the {large_whole}us the open took"
    );
    assert!(
        large_rows <= FIRST_ROWS_BUDGET_US,
        "the first rows at {large_n} tasks took {large_rows}us of the window's CPU, over \
         {FIRST_ROWS_BUDGET_US}us"
    );
    // The first rows read the chunk they are in: they cost what that chunk holds, not what the
    // workspace holds behind it. A thousand tasks have filled half of their first chunk, so some
    // of the growth from there is the chunk filling rather than the workspace behind it, and the
    // cost an item of the chunk is what does not grow.
    let was = per_chunk_item(small_rows, small_n);
    let cost = per_chunk_item(large_rows, large_n);
    assert!(
        cost < was * 2.0,
        "the first rows wait for the chunk they are in, not the workspace: {large_n} tasks took \
         {cost:.3} us of the window's CPU an item of their first chunk, {small_n} took {was:.3}"
    );
}

/// What a cold open's first rows cost an item of the chunk they are in. A workspace that fills
/// less than a chunk shows rows from all of it; a larger one shows them from the first chunk.
fn per_chunk_item(rows_us: u64, tasks: usize) -> f64 {
    let per_chunk = usize::try_from(tilolang_tilo::chunk::ITEMS).expect("items a chunk holds");
    let held = tasks.clamp(1, per_chunk);
    rows_us as f64 / held as f64
}

/// A cold open reads each chunk the record names once, and derives what it holds once: the work
/// grows with the tasks, not with the square of the chunks they span. Measured in an optimised
/// build, where the numbers mean something: `--ignored`.
///
/// The first rows wait for the chunk they are in — and the manifest's run tails, which a hundred
/// thousand items write as many changes of, are derived after them — at a hundred thousand tasks
/// as at a thousand, an item of their chunk costing what it costs there. What grows with the
/// workspace is the rest of the record, derived off the frame once the window shows the first
/// rows.
#[test]
#[ignore = "a measurement for an optimised build"]
fn a_cold_open_grows_with_the_tasks_not_their_chunks() {
    let _motion = ui::motion::testing::settled();
    let mut measured = Vec::new();
    let mut first_rows = Vec::new();
    for &n in &[SEEDED / 10, SEEDED, SEEDED_LARGE] {
        let chunks = seeded_tasks(n);
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        let storage = kept_root(&format!("cold-{n}"));
        let mut session = Session::tasks_over_launched(&route, chunks, Some(storage));
        // From the window opening: writing the workspace the session starts from is the test's
        // work, not the open's.
        let start = ui::rt::thread_cpu_time();
        let wall = ui::time::Instant::now();
        show_rows(&mut session);
        let rows_us = since(start, wall.elapsed());
        let shown_us = u64::try_from(wall.elapsed().as_micros()).unwrap_or(u64::MAX);
        session.settle();
        let us = since(start, wall.elapsed());
        eprintln!(
            "cold open at {n} tasks: first rows {:.2} ms of the window's CPU, shown after \
             {:.2} ms; whole {:.2} ms",
            rows_us as f64 / 1000.0,
            shown_us as f64 / 1000.0,
            us as f64 / 1000.0
        );
        measured.push((n, us));
        first_rows.push((n, rows_us, shown_us));
    }
    let (small_n, small) = measured[0];
    let (large_n, large) = measured[1];
    let ratio = large as f64 / small.max(1) as f64;
    let items = large_n as f64 / small_n.max(1) as f64;
    assert!(
        ratio <= items * 2.0,
        "a cold open grows with the tasks: {large_n} took {ratio:.1} times what {small_n} did"
    );
    // The first rows read the chunk they are in, whatever the workspace holds behind it: what
    // they cost an item of that chunk is what a cold open pays, and once more of the tasks, and
    // ten times more of them again, do not make it grow with the workspace.
    for (small, large) in [(0, 1), (1, 2)] {
        let (small_n, small_us, small_shown) = first_rows[small];
        let (large_n, large_us, large_shown) = first_rows[large];
        let us = per_chunk_item(large_us, large_n) / per_chunk_item(small_us, small_n);
        assert!(
            us < 2.0,
            "the first rows do not wait for the workspace: {large_n} tasks took {us:.1} times the \
             window's CPU that {small_n} did, an item of their first chunk"
        );
        assert!(
            large_shown <= small_shown.saturating_mul(3).saturating_add(250_000),
            "the first rows at {large_n} tasks were shown after {large_shown}us, {:.1} times the \
             {small_shown}us it took at {small_n}",
            large_shown as f64 / small_shown.max(1) as f64
        );
    }
    let (large_n, large_us, large_shown) = first_rows[2];
    assert!(
        large_us <= FIRST_ROWS_BUDGET_US,
        "the first rows at {large_n} tasks took {large_us}us of the window's CPU, over \
         {FIRST_ROWS_BUDGET_US}us"
    );
    if !cfg!(debug_assertions) {
        assert!(
            large_shown < 2_000_000,
            "the first rows at {large_n} tasks were shown after {large_shown}us, over the two \
             seconds spent before a person reads anything"
        );
    }
}

/// A window over a workspace a person has used for years reads the chunks its rows are in, not
/// every chunk the workspace holds: the index an earlier run kept answers the lists and the
/// sidebar's counts, and a row reads the chunk that holds it as it shows. Scrolling to the end
/// reads the last chunk; the chunks between are read only when a row in them shows. What the
/// window shows is what a window holding every chunk shows.
#[test]
fn a_window_reads_only_the_chunks_it_shows() {
    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(SEEDED);
    let all: Vec<tilolang_tilo::chunk::ChunkNo> = chunks.numbers().collect();
    assert!(
        all.len() > 4,
        "ten thousand tasks span their chunks: {all:?}"
    );
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let storage = kept_index(&route, &chunks, "shown");
    let last = *all.last().expect("the record and its chunks");

    let mut session = Session::tasks_over_in(&route, chunks.clone(), Some(storage));
    assert_eq!(session.builds(), 0, "the open reads the workspace's index");
    let opened = holding(&session);
    assert_eq!(
        opened,
        [tilolang_tilo::chunk::FIRST],
        "the rows and the counts the window opens on are the index's: it read {opened:?} of \
         {all:?}"
    );
    assert!(!session.titles().is_empty(), "the window shows its rows");

    // The rows, and the places and counts beside them, are what a replica holding every chunk
    // shows: the index answered them without a chunk being read.
    let mut whole = Session::tasks_over_in(&route, chunks.clone(), None);
    whole.held_every_chunk(&chunks);
    whole.settle();
    assert_eq!(
        shown(&mut session),
        shown(&mut whole),
        "the window shows what a full load shows"
    );

    // Scrolling to the end shows the last rows from the index as well: the list is the index's.
    let top = session
        .titles()
        .into_iter()
        .find(|title| title.starts_with("Item "))
        .expect("the window shows the workspace's tasks");
    let (x, y) = shown_at(&mut session, &top);
    session.window.move_to(x, y);
    session.window.scroll(0.0, -20_000.0);
    session.settle();
    let end = number_of(
        &session
            .titles()
            .into_iter()
            .filter(|title| title.starts_with("Item "))
            .max_by_key(|title| number_of(title).unwrap_or(0))
            .expect("rows at the end of the list"),
    );
    let first_of_last = usize::try_from(tilolang_tilo::chunk::ITEMS)
        .expect("items a chunk holds")
        .saturating_mul(all.len() - 2);
    assert!(
        end.is_some_and(|n| n >= first_of_last),
        "the list scrolled to its last chunk ({first_of_last} and up): {:?}",
        session.titles()
    );
    let scrolled = holding(&session);
    assert_eq!(
        scrolled,
        [tilolang_tilo::chunk::FIRST],
        "scrolling read no chunk: the rows are the index's: {scrolled:?} of {all:?}"
    );

    // Opening the last task's page reads at most the chunk that holds it: what its page shows
    // the index may answer on its own.
    let bottom = session
        .titles()
        .into_iter()
        .filter(|title| title.starts_with("Item "))
        .max_by_key(|title| number_of(title).unwrap_or(0))
        .expect("rows at the end of the list");
    let (x, y) = shown_at(&mut session, &bottom);
    session.window.click_at(x, y);
    session.settle();
    let opened = holding(&session);
    assert!(
        opened
            .iter()
            .all(|chunk| *chunk == tilolang_tilo::chunk::FIRST || *chunk == last),
        "the page read no chunk but the last: {opened:?} of {all:?}"
    );
}

/// The task number the title `title` ("Item 42") names.
pub(super) fn number_of(title: &str) -> Option<usize> {
    title.strip_prefix("Item ")?.parse().ok()
}

/// The chunks the window's store holds, in order.
fn holding(session: &Session) -> Vec<tilolang_tilo::chunk::ChunkNo> {
    let host = session
        .host
        .instances()
        .next()
        .expect("the window's instance")
        .instance()
        .host()
        .expect("the instance's store");
    host.store()
        .held_chunks()
        .into_iter()
        .map(|(number, _, _)| number)
        .collect()
}

/// The window's screen under `TILO_COLD_SHOTS`, named `name`, for review: nothing when the
/// variable is not set.
fn shots(name: &str, session: &mut Session) {
    let Some(dir) = std::env::var_os("TILO_COLD_SHOTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
        session.window.harness().set_theme(theme);
        session.window.frame();
        session
            .window
            .harness()
            .render_png(&dir.join(format!("{name}-{shade}.png")));
    }
}

/// What the window shows, as a person reads it off the accessibility tree: the role and the name
/// of every node that has one.
fn shown(session: &mut Session) -> Vec<(ui::a11y::Role, Option<String>)> {
    accessible(session)
        .into_iter()
        .filter_map(|node| node.name.map(|name| (node.role, Some(name))))
        .collect()
}

/// A window's CPU time an open may take, in microseconds: making the store, reading the
/// workspace's indexes, loading the package and taking the record in. Only the last grows with
/// the workspace — the store keeps its own copy of the record, a fifth of a second of a hundred
/// thousand tasks — while the index is read rather than built from the record: built, ten
/// thousand tasks take seconds.
const OPEN_BUDGET_US: u64 = 500_000;

/// The same for a hundred thousand tasks: the record's copy is what the workspace's size adds
/// to an open.
const OPEN_BUDGET_LARGE_US: u64 = 900_000;

/// A loose ceiling on the window's CPU time a cold open may take to show its first screen of
/// rows, whatever the workspace holds: the index is built in steps, the chunk the first screen's
/// rows are in first, and the window reads from it while the rest of the workspace is derived off
/// the frame. What the first rows cost beyond this is caught by the growth they are held to
/// against a tenth of the workspace, and by their coming before the whole workspace is read.
const FIRST_ROWS_BUDGET_US: u64 = 1_500_000;

/// The directory the tests of this binary keep what a package keeps in, emptied first.
fn kept_root(name: &str) -> std::path::PathBuf {
    let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hosted-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    root
}

/// The storage an earlier run of the app left the workspace `chunks` of `route` indexed in: its
/// index is built from every chunk and on the disk, the file closed, before this returns.
fn kept_index(
    route: &RouteSpec,
    chunks: &tilolang_tilo::chunk::Chunks,
    name: &str,
) -> std::path::PathBuf {
    let storage = kept_root(name);
    let kept = TilolangHost::new(Some(storage.clone()), Arc::new(|| {}))
        .kept(TASKS)
        .expect("the app keeps what it keeps under the storage");
    let indexes = kept.indexes.expect("the app's indexes are on the disk");
    let earlier = tilolang_tilo::host::Host::open(
        tilolang_tilo::host::Opening {
            record: chunks.first(),
            chunks: Some(chunks),
            index_dir: Some(tilolang_tilo::index::record_dir(
                &indexes,
                &route.cluster_id.to_string(),
                None,
            )),
            device: None,
        },
        tilolang_tilo::runtime::Principal::new("ada"),
        || None,
    )
    .expect("the record opens");
    earlier
        .load_bundle(&package(package_of(TASKS)))
        .expect("the app's package loads");
    // The chunks the earlier run held: the record's own, and every run above it.
    earlier.sync(chunks);
    earlier.wait_index();
    assert!(earlier.index_ready(), "the earlier run indexed the record");
    earlier.persist_index();
    drop(earlier);
    storage
}

/// The window's frames until a list beside the page shows a row: the UI thread's CPU time it
/// took, from the first turn that could show it. A cold window shows rows from the first step of
/// the index it builds, while the rest of the workspace is derived off the frame, so this is
/// short even where the whole workspace takes seconds.
fn show_rows(session: &mut Session) -> u64 {
    let start = ui::rt::thread_cpu_time();
    let wall = ui::time::Instant::now();
    while wall.elapsed() < std::time::Duration::from_secs(120) {
        let changed = session.host.turn();
        session.window.frame();
        let shows = session.hosted().view().is_some_and(|view| {
            view.lists().into_iter().any(|list| {
                !list.starts_with(tilo_ui_host::shell::DETAIL)
                    && view
                        .rows_of(&list)
                        .is_some_and(|rows| !rows.borrow().rendered().is_empty())
            })
        });
        if shows {
            return since(start, wall.elapsed());
        }
        if !changed {
            // Nothing on the frame to do: what the rows wait for is read beside the frames, and
            // the host is woken when it is ready to be taken in. The wait is short, so that how
            // long the rows took is the work and not the moment the window looked again.
            session.woken.wait(std::time::Duration::from_millis(10));
        }
    }
    let ready = session.index_ready();
    let view = session.hosted().view();
    panic!(
        "the window never showed the workspace's rows: index ready {ready}, view {}, lists {:?}",
        view.is_some(),
        view.map(tilo_ui_host::AppWindow::lists),
    );
}

/// Choosing a sidebar place: the list shows what that place holds.
fn switch_places(session: &mut Session, costs: &mut Costs) {
    for place in ["Today", "Inbox", "Done", "Inbox", "Today", "Inbox"] {
        let (x, y) = place_at(session, place);
        costs.add(cost(session, "switch place", |session| {
            session.window.click_at(x, y);
        }));
        assert!(chosen(session, place), "the sidebar shows {place} chosen");
    }
}

/// Typing a task into the entry, a character at a time, and Enter to add it.
fn type_a_task(session: &mut Session, costs: &mut Costs) {
    use ui::winit::keyboard::NamedKey;

    session.window.chord(&["ctrl"], "n");
    session.settle();
    let mut typed = String::new();
    for ch in "Buy".chars() {
        typed.push(ch);
        let typed_so_far = typed.clone();
        costs.add(cost(session, "entry keystroke", |session| {
            session.window.type_text(&ch.to_string());
        }));
        assert_eq!(
            entry(session),
            typed_so_far,
            "the entry shows what was typed"
        );
    }
    costs.add(cost(session, "entry enter", |session| {
        session.window.press_key(NamedKey::Enter);
    }));
    session.settle();
    assert!(
        task_in_record(session, "Buy", false),
        "the entry's task is in the workspace"
    );
}

/// Checking tasks off: each row stays where it stands, reading checked, the next row below it
/// the one a person reaches for next.
fn check_tasks(session: &mut Session, costs: &mut Costs) {
    for _ in 0..4 {
        let (title, x, y) = first_check(session);
        costs.add(cost(session, "check a task", |session| {
            session.window.click_at(x, y);
        }));
        session.settle();
        assert!(
            checked_off(session, &title),
            "the row a person checked reads checked: {title}"
        );
    }
}

/// Opening a task's page: its row opens into it where it stands, the list growing the row into
/// the item's page — the title it edits, and under it what the item holds.
fn open_pages(session: &mut Session, costs: &mut Costs) {
    for at in 1..4 {
        let title = session
            .titles()
            .get(at)
            .cloned()
            .expect("the list shows a row");
        let (x, y) = shown_at(session, &title);
        costs.add(cost(session, "open a task page", |session| {
            session.window.click_at(x, y);
        }));
        session.settle();
        let page = opened_title_field(session).unwrap_or_else(|| {
            panic!("the row a person opened shows the page of the task it stands for: {title}")
        });
        let rect = widget_by_id(session.window.root(), &page)
            .unwrap_or_else(|| panic!("the page's title {page} is laid out"))
            .get_rect();
        assert!(
            rect.height() > 0.0,
            "the page of {title} stands under its row"
        );
    }
}

/// The host's turns that changed something, until the window and the app have nothing left to
/// do: what a step spent in round trips between the two.
fn turns_until_rest(session: &mut Session) -> u32 {
    let mut changed = 0;
    let mut quiet = 0;
    while quiet < 3 {
        if session.host.turn() {
            changed += 1;
            quiet = 0;
        } else {
            quiet = 1_u32.saturating_add(quiet);
        }
        session.window.frame();
    }
    changed
}

/// What opening a window did, counted rather than timed: the changes the open wrote into the
/// workspace's record, the times the store took a chunk of it against the chunks the record
/// holds, and — taken at the frame that finished the open — the records the instance's
/// publication visited and the rows the window shows.
#[derive(Clone, Copy, Debug)]
struct Opening {
    /// The changes the open wrote into the workspace's record.
    written: usize,
    /// Times the store took a chunk of the record: its first copy of one, or a sync into it.
    read: u64,
    /// The chunks the record the window opened holds.
    chunks: u64,
    /// The records the instance's publication visited to show the open.
    published: u64,
    /// The rows the window shows.
    rows: usize,
}

impl Opening {
    /// The counts, as a line: what a person reading a failure needs to see which of them grew.
    fn line(self) -> String {
        let Self {
            written,
            read,
            chunks,
            published,
            rows,
        } = self;
        format!(
            "wrote {written} changes, read the record's {chunks} chunks {read} times, published \
             {published} records, {rows} rows"
        )
    }
}

/// Opening a window of the workspace the record `chunks` holds, and what it did. The open is
/// finished at a condition of the window's, not after a count of turns: the index the store
/// builds off the frame is read and taken in, the first frame that shows what the workspace
/// holds has drawn, and the open set nothing going that is still owed — the window and its
/// publication at rest, which the frame schedule states. The turns a wait for that build takes
/// are the machine's, so they say nothing about the workspace.
fn opening(n: usize) -> Opening {
    let chunks = seeded_tasks(n);
    let held = chunks.numbers().count() as u64;
    // What the workspace holds before the window opens it: opening it must leave the record
    // with no more changes than the workspace already had, whether the window finds the root
    // it opens or makes it.
    let seeded = chunks
        .first()
        .expect("the workspace's record")
        .committed_document()
        .get_changes(&[])
        .len();
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_launched(&route, chunks, None);
    session.wait_index();
    session.host.turn();
    session.window.frame();
    let shown = Opened::of(&mut session);
    session.settle();
    let (schedule, queued, redraw) = {
        let window = session.window.harness();
        (
            window.frame_schedule(),
            window.queued_work_sources(),
            window.redraw_sources(),
        )
    };
    assert!(
        !schedule.needs_redraw && !schedule.queued_work && !schedule.needs_animation_frame,
        "the open comes to rest with the window and its publication: {schedule:?} {queued:?} \
         {redraw:?}"
    );
    let written = changes(&session) - seeded;
    let stats = {
        let hosted = session
            .host
            .instances()
            .next()
            .expect("the window's instance");
        let host = hosted.instance().host().expect("the instance's store");
        host.store().stats()
    };
    let read = (stats.attaches + stats.incremental_syncs) + stats.full_syncs;
    Opening {
        written,
        read,
        chunks: held,
        published: shown.published,
        rows: shown.rows,
    }
}

/// The changes the window's record holds: what the window wrote into the workspace.
fn changes(session: &Session) -> usize {
    let hosted = session
        .host
        .instances()
        .next()
        .expect("the window's instance");
    hosted
        .record()
        .doc()
        .committed_document()
        .get_changes(&[])
        .len()
}

/// Opening a window of a workspace it has used before writes nothing: the root the window
/// opens is the one the app made when it was first added, and a window that finds it reads it.
/// Each chunk of the record is taken once, and what the open does to show the window — the
/// records its publication visits and the rows it shows — is the same whatever the workspace
/// holds. The open is counted by that work rather than by the turns it took: a settle waits on
/// the index the instance builds off its frames, and how many turns such a wait takes is the
/// machine's.
#[test]
fn opening_a_window_writes_nothing() {
    let _motion = ui::motion::testing::settled();
    let small = opening(1_000);
    let large = opening(10_000);
    eprintln!(
        "open at 1000 tasks: {}; at 10000: {}",
        small.line(),
        large.line()
    );
    assert_eq!(
        small.written, 0,
        "opening a workspace it has used writes nothing"
    );
    assert_eq!(
        large.written, 0,
        "opening a larger workspace writes nothing"
    );
    assert_eq!(
        (small.read, large.read),
        (small.chunks, large.chunks),
        "the store takes each chunk of the record once, whatever it holds"
    );
    assert!(
        small.rows > 0 && large.rows == small.rows,
        "the window shows the same rows whatever the workspace holds: {} at 1000 tasks, {} at \
         10000",
        small.rows,
        large.rows
    );
    assert!(
        large.published <= small.published.saturating_add(2),
        "the open publishes the records the window shows, not the workspace's: {} records at \
         10000 tasks, {} at 1000",
        large.published,
        small.published
    );
}

/// Scrolling a list of ten thousand tasks: the app is asked for the rows a scroll is about to
/// reach before it reaches them, so a step costs the window's frames and one ask, or none. What
/// a scroll step asks of the app is what makes the person wait for it, so it is counted, not
/// timed: a step that asked once per row would pass any budget on a fast enough machine.
#[test]
fn scrolling_asks_the_app_for_rows_ahead_not_once_a_row() {
    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, chunks, None);
    session.settle();
    let first = top_title(&mut session);
    let at = shown_at(&mut session, &first);
    session.window.move_to(at.0, at.1);
    session.settle();

    let notches = 8;
    let before = top_title(&mut session);
    let mut turns = 0;
    for _ in 0..notches {
        session.window.scroll(0.0, -3.0);
        turns += turns_until_rest(&mut session);
        session.settle();
    }
    assert_ne!(top_title(&mut session), before, "the list scrolled");
    eprintln!("scroll: {notches} notches asked the app {turns} times");
    assert!(
        turns <= notches,
        "{notches} scroll steps asked the app {turns} times: a step costs one ask or none"
    );
}

/// The glide a wheel notch started, run to where it rests on the frame clock: where the list
/// stands after a notch is what the notch did, not how far the frames the step measured got
/// before the window was looked at, which is as far as the machine's speed let them.
fn glide_out(session: &mut Session) {
    use ui::time::testing::with_monotonic_time;
    let mut at = ui::time::Instant::now();
    for _ in 0..400 {
        if !session
            .window
            .harness()
            .frame_schedule()
            .needs_animation_frame
        {
            return;
        }
        at += ui::time::Duration::from_millis(16);
        with_monotonic_time(at, || {
            session.host.turn();
            session.window.frame();
        });
    }
    panic!("the list's glide never came to rest");
}

/// Scrolling the list: a wheel notch down and back up, the top row changing each time.
fn scroll_the_list(session: &mut Session, costs: &mut Costs) {
    let first = top_title(session);
    let at = shown_at(session, &first);
    session.window.move_to(at.0, at.1);
    session.settle();
    let mut top = top_shown(session);
    for notch in 0..8 {
        let down = notch % 2 == 0;
        costs.add(cost(session, "scroll", |session| {
            session.window.scroll(0.0, if down { -3.0 } else { 3.0 });
        }));
        glide_out(session);
        session.settle();
        let now = top_shown(session);
        assert_ne!(now, top, "the list scrolled");
        top = now;
    }
}

/// What a change to one task re-indexes: the entries of the resources it touched, whatever the
/// workspace holds. The session's search index keeps an entry per resource; adding one task of
/// ten thousand adds that task's entry, and the whole set is published again only when the
/// publication cannot say what moved.
#[test]
fn a_change_reindexes_the_resources_it_touched() {
    use ui::winit::keyboard::NamedKey;

    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, chunks, None);
    session.settle();
    // Opening published every entry; what the change publishes is counted from here.
    session.forget_sent();

    session.window.chord(&["ctrl"], "n");
    session.settle();
    session.window.type_text("Buy milk");
    session.forget_sent();
    session.window.press_key(NamedKey::Enter);
    session.settle();
    assert!(
        task_in_record(&session, "Buy milk", false),
        "the task the entry added is in the workspace"
    );

    let (upserted, dropped, whole) = session.index_published();
    assert!(
        !whole,
        "a change publishes the entries it moved, not every entry again"
    );
    assert_eq!(
        (upserted, dropped),
        (1, 0),
        "adding one task of {SEEDED} indexes one entry, not the workspace's"
    );
}

/// The boundaries the window's views have rendered since it opened: every app instance the
/// window runs, and both lanes it shows.
fn renders(session: &Session) -> u64 {
    session
        .host
        .instances()
        .flat_map(|instance| {
            let instance = instance.instance();
            [
                tilo_render_protocol::patch::Lane::Root,
                tilo_render_protocol::patch::Lane::Detail,
            ]
            .into_iter()
            .filter_map(|lane| instance.view(lane))
            .collect::<Vec<_>>()
        })
        .map(|view| view.borrow().stats().renders)
        .fold(0, u64::saturating_add)
}

/// Typing a task in the list's entry: a keystroke changes the entry's own view state and
/// nothing else, so what runs again is the boundary that read it — the entry and the chips it
/// shows — and the list around it, every row of it, stands as it is.
#[test]
fn typing_in_the_entry_renders_the_entry() {
    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, chunks, None);
    session.window.chord(&["ctrl"], "n");
    session.settle();
    for ch in "Buy #milk".chars() {
        let before = renders(&session);
        session.window.type_text(&ch.to_string());
        session.settle();
        let ran = renders(&session) - before;
        assert!(
            ran <= 2,
            "typing {ch:?} runs {ran} boundaries: the entry and its chips, not the list it \
             stands under"
        );
    }
    assert_eq!(
        entry(&mut session),
        "Buy #milk",
        "the entry shows what was typed"
    );
}

/// A row opens where it stands after the person's own changes have taken rows out of the place
/// they stand in: the rows they kept are rows that open into their item like any other, and
/// opening one is reading on in the same place, so every one of them stays.
#[test]
fn a_row_opens_where_it_stands_after_tasks_are_checked() {
    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(SMALL);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, chunks, None);
    for _ in 0..4 {
        let (_, x, y) = first_check(&mut session);
        session.window.click_at(x, y);
        session.settle();
    }
    let title = session.titles().get(1).cloned().expect("a row");
    let (x, y) = shown_at(&mut session, &title);
    let shown = session.titles();
    let before = renders(&session);
    session.window.click_at(x, y);
    session.settle();
    let ran = renders(&session) - before;
    assert!(
        opened_title_field(&mut session).is_some(),
        "the row of {title} opens where it stands"
    );
    // The row and the page it grows into run, and the list that holds it hears which row is
    // open; the rows beside it, and the place's query, run not at all.
    assert!(
        ran <= 12,
        "opening the row of {title} ran {ran} boundaries: the rows beside it ran again"
    );
    assert_eq!(
        session.titles(),
        shown,
        "the rows checked off stand where they stood, the one opened among them"
    );
}

/// The row standing open into its item where it stands: the page a person reads there, and the
/// field they type the item's title into. `None` while no row stands open.
fn opened_title_field(session: &mut Session) -> Option<String> {
    let view = session.hosted().view()?;
    let mut found = None;
    for list in view.lists() {
        if list.starts_with(tilo_ui_host::shell::DETAIL) {
            continue;
        }
        let Some(rows) = view.rows_of(&list) else {
            continue;
        };
        let rows = rows.borrow();
        for key in rows.rendered() {
            let Some(row) = rows.row(&key) else {
                continue;
            };
            if let Some(field) = find(row, &|node| {
                node.kind == "ui.TextInput"
                    && node.binding.as_ref().is_some_and(|b| b.field().is_some())
            }) {
                found = Some(field.key.clone());
                break;
            }
        }
    }
    found
}

/// Typing a task's title on its page: the keystroke edits one field of one task, so the page
/// that shows it — the row grown into it where it stands — and the row behind it run again, and
/// the rest of the window stands.
#[test]
fn typing_a_tasks_title_reruns_the_views_that_read_it() {
    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, chunks, None);
    let title = session.titles().get(1).cloned().expect("a row");
    let (x, y) = shown_at(&mut session, &title);
    session.window.click_at(x, y);
    session.settle();
    let key = opened_title_field(&mut session)
        .unwrap_or_else(|| panic!("the row a person opened shows the page that edits its title"));
    let rect = widget_by_id(session.window.root(), &key)
        .unwrap_or_else(|| panic!("the title {key} is laid out"))
        .get_rect();
    session.window.click_at(rect.center().x, rect.center().y);
    session.settle();
    for ch in "Buy".chars() {
        let before = renders(&session);
        session.window.type_text(&ch.to_string());
        session.settle();
        let ran = renders(&session) - before;
        assert!(
            ran <= 20,
            "typing {ch:?} on the page runs {ran} boundaries: the page and the row that shows \
             the title, not every row the window holds"
        );
    }
}

/// Checking tasks off one after another: every row the person checked stays where it stands,
/// reading checked, the list goes on rendering, and checking one task runs a handful of
/// boundaries rather than every row the window shows.
#[test]
fn checking_tasks_one_after_another_keeps_the_list_rendering() {
    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(100);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, chunks, None);
    let mut checked = Vec::new();
    for _ in 0..4 {
        let (title, x, y) = first_check(&mut session);
        let before = renders(&session);
        session.window.click_at(x, y);
        session.settle();
        let ran = renders(&session) - before;
        assert!(
            ran <= 10,
            "checking {title} runs {ran} boundaries: the rows that read it and the window, \
             not every row it shows"
        );
        checked.push(title);
    }
    for title in checked {
        assert!(
            checked_off(&mut session, &title),
            "the row a person checked reads checked: {title}"
        );
    }
}

/// A step of the person's reaches the window once: checking a task off runs the window's root
/// once, on the change the record brought back with what its derivation added, and sends the
/// tree once. The row then reads its title and its box checked off, in a workspace of a hundred
/// tasks and of ten thousand.
#[test]
fn a_step_reaches_the_window_in_one_frame() {
    let _motion = ui::motion::testing::settled();
    for n in [100, SEEDED] {
        let chunks = seeded_tasks(n);
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        let mut session = Session::tasks_over_in(&route, chunks, None);
        let counted = |session: &Session| -> (u64, u64) {
            let instance = session
                .host
                .instances()
                .next()
                .expect("the window's instance")
                .instance();
            let runs = instance
                .view(tilo_render_protocol::patch::Lane::Root)
                .expect("the root view")
                .borrow()
                .stats()
                .root_runs;
            (runs, instance.patches_sent())
        };
        let (title, x, y) = first_check(&mut session);
        let (runs, sent) = counted(&session);
        session.window.click_at(x, y);
        session.settle();
        let (ran, published) = counted(&session);
        assert_eq!(
            ran - runs,
            1,
            "checking {title} in a workspace of {n} tasks runs the window's root {} times, \
             not once",
            ran - runs
        );
        assert_eq!(
            published - sent,
            1,
            "checking {title} in a workspace of {n} tasks sends the tree {} times, not once",
            published - sent
        );
        assert!(
            checked_off(&mut session, &title),
            "the row a person checked in a workspace of {n} tasks reads its title and checked: \
             {title}"
        );
    }
}
// -- What a step costs, counted ------------------------------------------------------------
//
// The interactions above are costed in time, which a machine reads differently from week to
// week. These are the same steps counted: what the window's instance read of the record and of
// the workspace's indexes, how many items a filter was checked against one by one, which
// boundaries it rendered and what it sent the window; and what the window's layout built and
// its text shaped. A step costs what it changed and what the window shows, so every one of
// them reads the same in a workspace ten times the size.

/// A workspace a tenth the size of the one a person has used for years.
const SMALL: usize = 1_000;

/// What one step did, counted rather than timed.
#[derive(Clone, Copy, Debug, Default)]
struct Done {
    renders: u64,
    items: u64,
    located: u64,
    fields: u64,
    listings: u64,
    entries: u64,
    dirs: u64,
    pages: u64,
    index_fields: u64,
    evaluated: u64,
    versions: u64,
    version_changes: u64,
    sent: u64,
    nodes: u64,
    shaped: u64,
}

impl Done {
    /// What the window and its instance have done so far.
    fn now(session: &mut Session) -> Self {
        let (renders, items, reads, sent) = {
            let hosted = session
                .host
                .instances()
                .next()
                .expect("the window's instance");
            let instance = hosted.instance();
            let host = instance.host().expect("the instance's host");
            let reads = host.store().read_stats();
            let renders = [
                tilo_render_protocol::patch::Lane::Root,
                tilo_render_protocol::patch::Lane::Detail,
            ]
            .into_iter()
            .filter_map(|lane| instance.view(lane))
            .map(|view| view.borrow().stats().renders)
            .fold(0, u64::saturating_add);
            (
                renders,
                host.runtime().items_built(),
                reads,
                instance.patches_sent(),
            )
        };
        let (_, shaped, _) = session.window.harness().take_text_shape_stats();
        Self {
            renders,
            items,
            located: reads.doc.located,
            fields: reads.doc.fields,
            listings: reads.doc.listings,
            entries: reads.doc.entries,
            dirs: reads.index.dirs,
            pages: reads.index.pages,
            index_fields: reads.index.fields,
            evaluated: reads.evaluated,
            versions: reads.versions,
            version_changes: reads.version_changes,
            sent,
            nodes: session.window.harness().layout_nodes_built(),
            shaped,
        }
    }

    /// This, less what `earlier` had done.
    fn since(self, earlier: Self) -> Self {
        Self {
            renders: self.renders.saturating_sub(earlier.renders),
            items: self.items.saturating_sub(earlier.items),
            located: self.located.saturating_sub(earlier.located),
            fields: self.fields.saturating_sub(earlier.fields),
            listings: self.listings.saturating_sub(earlier.listings),
            entries: self.entries.saturating_sub(earlier.entries),
            dirs: self.dirs.saturating_sub(earlier.dirs),
            pages: self.pages.saturating_sub(earlier.pages),
            index_fields: self.index_fields.saturating_sub(earlier.index_fields),
            evaluated: self.evaluated.saturating_sub(earlier.evaluated),
            versions: self.versions.saturating_sub(earlier.versions),
            version_changes: self.version_changes.saturating_sub(earlier.version_changes),
            sent: self.sent.saturating_sub(earlier.sent),
            nodes: self.nodes.saturating_sub(earlier.nodes),
            shaped: self.shaped.saturating_sub(earlier.shaped),
        }
    }

    /// Each count with its name, for a table.
    fn counts(self) -> [(&'static str, u64); 14] {
        [
            ("renders", self.renders),
            ("items", self.items),
            ("located", self.located),
            ("fields", self.fields),
            ("listings", self.listings),
            ("entries", self.entries),
            ("dirs", self.dirs),
            ("pages", self.pages),
            ("index_fields", self.index_fields),
            ("evaluated", self.evaluated),
            ("versions", self.versions),
            ("sent", self.sent),
            ("nodes", self.nodes),
            ("shaped", self.shaped),
        ]
    }
}

/// The rows the window's lists hold rendered: what it shows, whatever the workspace holds.
fn rows_shown(session: &mut Session) -> u64 {
    let Some(view) = session.hosted().view() else {
        return 0;
    };
    view.lists()
        .into_iter()
        .filter_map(|list| view.rows_of(&list))
        .map(|rows| rows.borrow().rendered().len() as u64)
        .sum()
}

/// `input`, the frames that show what it did, and what the window and its instance did for them:
/// what a step of a person costs.
fn step(session: &mut Session, input: impl FnOnce(&mut Session)) -> Done {
    let before = Done::now(session);
    input(session);
    session.settle();
    Done::now(session).since(before)
}

/// What a day of the window's steps costs at `n` tasks, in the order a person does them, and
/// the rows the window shows at rest: each step, and what it did.
fn steps_costs(n: usize) -> (Vec<(&'static str, Done)>, u64) {
    let chunks = seeded_tasks(n);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, chunks, None);
    let mut out = Vec::new();
    // What a writer's first change in a record renumbers is none of these steps'.
    for _ in 0..2 {
        let (_, x, y) = first_check(&mut session);
        session.window.click_at(x, y);
        session.settle();
    }
    assert!(!session.titles().is_empty(), "the window shows the place");
    let (_, x, y) = first_check(&mut session);
    out.push((
        "check a task",
        step(&mut session, |session| session.window.click_at(x, y)),
    ));
    // Done orders its place by when each task was finished, a stamp of the record's history
    // rather than a field a column orders: reading the place once to show it is the open of
    // the place, not a step, so it is taken here and what is counted is the way back.
    let (x, y) = place_at(&mut session, "Done");
    session.window.click_at(x, y);
    session.settle();
    let (x, y) = place_at(&mut session, "Inbox");
    out.push((
        "switch place",
        step(&mut session, |session| session.window.click_at(x, y)),
    ));
    session.window.chord(&["ctrl"], "n");
    session.settle();
    out.push((
        "entry keystroke",
        step(&mut session, |session| session.window.type_text("B")),
    ));
    out.push((
        "entry enter",
        step(&mut session, |session| {
            session
                .window
                .press_key(ui::winit::keyboard::NamedKey::Enter);
        }),
    ));
    // The rows the window shows, read along the top of the list: a scroll is a fraction of one,
    // so it lays out a row more or less by where it lands rather than by what the workspace holds.
    let rows = rows_shown(&mut session);
    let first = top_title(&mut session);
    let at = shown_at(&mut session, &first);
    session.window.move_to(at.0, at.1);
    session.settle();
    out.push((
        "scroll",
        step(&mut session, |session| session.window.scroll(0.0, -3.0)),
    ));
    // The page of the row at the top of the list, opened where it stands: the page reads the
    // task it shows and the index around it, never the workspace the task is one of.
    let first = top_title(&mut session);
    let (x, y) = shown_at(&mut session, &first);
    out.push((
        "open a task page",
        step(&mut session, |session| session.window.click_at(x, y)),
    ));
    (out, rows)
}

/// A step of the window costs what it changed and what the window shows, never the size of the
/// workspace: opening the window, checking a task off, switching the sidebar's place, a keystroke
/// in the entry, the Enter that adds the task, a notch of the scroll and the page of a row do the
/// same work in ten thousand tasks as in a thousand. What would grow with the workspace is a loop
/// over the collection, a filter no index answers, a publication of rows the window cannot show, a
/// layout of rows off screen, or a history read of the workspace's every change rather than of the
/// chunk the row it shows is stored in. An index reads a tree whose depth is the logarithm of what
/// it holds, so ten times the workspace must not make a step do twice the work: what a step costs
/// is what it changed.
/// Adding a task runs the row it adds and the views that count it, not every row the window
/// shows: the task is written into its collection's newest chunk, which the window had not read
/// but the workspace's index already reflects, so taking that chunk in moves nothing any row
/// read.
#[test]
fn adding_a_task_runs_the_row_it_adds_not_every_row() {
    use ui::winit::keyboard::NamedKey;

    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let storage = kept_index(&route, &chunks, "adding");
    let mut session = Session::tasks_over_in(&route, chunks, Some(storage));
    let (x, y) = place_at(&mut session, "Inbox");
    session.window.click_at(x, y);
    session.settle();
    session.window.chord(&["ctrl"], "n");
    session.settle();
    session.window.type_text("Buy");
    session.settle();
    let rows = rows_shown(&mut session);
    let done = step(&mut session, |session| {
        session.window.press_key(NamedKey::Enter);
    });
    assert!(
        task_in_record(&session, "Buy", false),
        "the entry's task is in the workspace"
    );
    assert!(
        rows > 20 && done.renders <= 12,
        "adding a task with {rows} rows on screen ran {} boundaries: the rows it did not \
         touch ran again",
        done.renders
    );
}

#[test]
fn a_step_costs_what_it_changes_not_the_workspace() {
    let _motion = ui::motion::testing::settled();
    let (small, small_rows) = steps_costs(SMALL);
    let (large, large_rows) = steps_costs(SEEDED);
    assert_eq!(
        large_rows, small_rows,
        "the window shows {large_rows} rows of {SEEDED} tasks and {small_rows} of {SMALL}"
    );
    // A resource's log is the log of the chunk holding it: what a step's history reads — the
    // marks of the rows it shows and the history behind them — is a chunk's changes, never the
    // workspace's every change.
    let mut past = Vec::new();
    for (label, done) in &large {
        if done.version_changes > tilolang_tilo::chunk::ITEMS {
            past.push(format!(
                "{label} read {} changes of the record for its versions at {SEEDED} tasks; a \
                 chunk holds at most {}",
                done.version_changes,
                tilolang_tilo::chunk::ITEMS
            ));
        }
    }
    assert!(
        past.is_empty(),
        "a step's history reads the chunk it shows:\n{}",
        past.join("\n")
    );
    let mut grew = Vec::new();
    let mut table = vec![format!(
        "{:<18} {:>12} {:>8} {:>8}",
        "step", "count", SMALL, SEEDED
    )];
    for ((label, at_large), (_, at_small)) in large.iter().zip(&small) {
        for ((name, at_large), (_, at_small)) in
            at_large.counts().into_iter().zip(at_small.counts())
        {
            table.push(format!(
                "{label:<18} {name:>12} {at_small:>8} {at_large:>8}"
            ));
            let allowed = at_small.saturating_mul(2).saturating_add(8);
            if at_large > allowed {
                grew.push(format!(
                    "{label} {name}: {at_large} at {SEEDED} tasks, {at_small} at {SMALL}"
                ));
            }
        }
    }
    eprintln!("{}", table.join("\n"));
    assert!(
        grew.is_empty(),
        "a step costs more in a larger workspace:\n{}",
        grew.join("\n")
    );
}

#[test]
#[ignore = "temporary: the open a single-document workspace costs"]
fn tmp_open_of_one_document() {
    let _motion = ui::motion::testing::settled();
    let tasks = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(package_of(TASKS));
    let packages = tilolang_tilo::bench::compile(&tasks).expect("compiles");
    let n = SEEDED;
    let doc = tilolang_tilo::bench::seeded_record(&packages, n).expect("seeds");
    let chunks = tilolang_tilo::chunk::Chunks::of(doc);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let storage = kept_index(&route, &chunks, &format!("one-{n}"));
    let start = ui::rt::thread_cpu_time();
    let wall = ui::time::Instant::now();
    let mut session = Session::tasks_over_launched(&route, chunks, Some(storage));
    let open_us = since(start, wall.elapsed());
    let wall_us = wall.elapsed().as_micros() as u64;
    session.wait_index();
    let ready_us = since(start, wall.elapsed());
    eprintln!(
        "one document at {n} tasks: CPU {:.2} ms, wall {:.2} ms; reading the index {:.2} ms",
        open_us as f64 / 1000.0,
        wall_us as f64 / 1000.0,
        ready_us as f64 / 1000.0
    );
    session.settle();
}

/// What a frame asked the layout for: the nodes it built, whether it recomposed the tree, and
/// how many nodes it invalidated to be measured again.
#[derive(Debug)]
struct Lay {
    built: u64,
    rebuilt: u64,
    nodes: u64,
}

impl Lay {
    fn of(session: &mut Session) -> Self {
        let window = session.window.harness();
        Self {
            built: window.layout_nodes_built(),
            rebuilt: window.rebuilt_frames(),
            nodes: u64::try_from(window.layout_node_ids().len()).unwrap_or(u64::MAX),
        }
    }
}

/// A step lays out what it changes, not the window: a keystroke in the entry and a notch of the
/// scroll recompose at most the one frame their change composes, and the frames after it place
/// only the widgets a view moved, ask the layout to measure the field (or the row) that changed
/// and the nodes above it, and build no node of the window's composition again. The frames a
/// step runs are also the ones its work asks for: a window at rest runs none, and neither does a
/// step that leaves it at rest.
#[test]
fn a_step_lays_out_what_it_changes() {
    /// A step's name and the input a person gives the window.
    type Step = (&'static str, Box<dyn FnOnce(&mut Session)>);

    /// The nodes a frame may invalidate for a keystroke or a notch of the scroll: the field or
    /// the rows that moved and the nodes above them.
    const MEASURED: u64 = 12;

    /// The frames one input may run: the frame of the window's clock it asks for, and one more
    /// for what it changed.
    const STEPS_INPUT_FRAMES: u64 = 2;

    let _motion = ui::motion::testing::settled();
    let doc = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = on_the_frame_clock(Session::tasks_over_in(&route, doc, None));
    session.settle();
    session.window.chord(&["ctrl"], "n");
    session.settle();
    let window = Lay::of(&mut session);
    assert!(
        window.nodes > 40,
        "the window's composition holds {} layout nodes, too few for a step to be measured \
         against",
        window.nodes
    );
    let first = top_title(&mut session);
    let at = shown_at(&mut session, &first);
    session.window.move_to(at.0, at.1);
    session.settle();
    // Each step's frames as the harness timed them, and none of the settles before it.
    session.window.harness().begin_profiling();
    session.window.harness().take_profile_samples();
    let steps: [Step; 2] = [
        (
            "entry keystroke",
            Box::new(|session: &mut Session| session.window.type_text("a")),
        ),
        (
            "scroll",
            Box::new(|session: &mut Session| session.window.scroll(0.0, -3.0)),
        ),
    ];
    for (label, input) in steps {
        let before = Lay::of(&mut session);
        let drawn_before = session.window.harness().drawn_commands();
        input(&mut session);
        // The frames the input itself ran, then the frames its work asks the window for.
        let rest = at_rest(&mut session, drawn_before);
        let marked = rest
            .marked
            .max(session.window.harness().last_relayout().marked_dirty);
        let after = Lay::of(&mut session);
        let ran = u64::try_from(rest.frames.len()).unwrap_or(u64::MAX);
        let drawn = rest.drawn;
        eprintln!(
            "{label}: {ran} frames, {drawn} draw commands, {marked} of the window's {} nodes \
             measured, {} of the {ran} frames placed every widget",
            after.nodes, rest.full
        );
        // A keystroke or a notch of the scroll is one input on the window's clock, and one more
        // frame if what it changed has to be composed: the key down and the key up of a character
        // reach the same frame, whatever the tree's size.
        assert!(
            ran > 0 && ran <= STEPS_INPUT_FRAMES,
            "{label} ran {ran} frames of the window's clock, {drawn} draw commands for its {} \
             nodes: one input frame and at most one for what it changed",
            after.nodes
        );
        // Whatever the step changed, one frame composes it; what comes after moves it.
        let rebuilds = after.rebuilt.saturating_sub(before.rebuilt);
        assert!(
            rebuilds <= 1,
            "{label} recomposed the layout tree in {rebuilds} of its {ran} frames"
        );
        // One composition measures and builds the window's nodes again; what the frames beyond it
        // touch are the nodes a change mounts and the nodes above them.
        let composition = rebuilds.saturating_mul(window.nodes);
        assert!(
            u64::try_from(marked).unwrap_or(u64::MAX) <= MEASURED.saturating_add(composition),
            "{label} asked the layout to measure {marked} of the window's {} nodes",
            after.nodes
        );
        // A frame places every widget of the tree when it composes it or measures nodes it did
        // not hold, which is what a change reaches; a frame with nothing composed or measured
        // places only the widgets a view moved.
        assert_eq!(
            rest.full_unasked, 0,
            "{label} placed every widget of the window's {} with nothing composed or measured, \
             in {} of the {ran} frames it ran",
            after.nodes, rest.full_unasked
        );
        let built = after.built.saturating_sub(before.built);
        // What a step builds is what it composes or mounts once: a frame that recomposed the
        // tree, or the rows the app published for the step, and the nodes the step measured.
        // Frames that built the window's nodes again, which is what a walk of the whole tree
        // costs a person, build past it once per frame.
        assert!(
            built <= after.nodes.saturating_add(MEASURED),
            "{label} built {built} layout nodes of the window's {}, one composition of them at \
             most",
            after.nodes
        );
        session.settle();
        // What the settle after the step ran is not the step's.
        session.window.harness().take_profile_samples();
    }
}

/// A window a person has not touched asks for no frame: nothing is queued, nothing animates,
/// nothing is due, and what the last frame drew is what it shows.
#[test]
fn a_window_at_rest_asks_its_host_for_no_frame() {
    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(20);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, chunks, None);
    session.settle();
    let window = session.window.harness();
    let schedule = window.frame_schedule();
    assert!(
        !schedule.needs_redraw && !schedule.queued_work && !schedule.needs_animation_frame,
        "the window at rest asks for none: {schedule:?} {:?} {:?}",
        window.queued_work_sources(),
        window.redraw_sources()
    );
    assert_eq!(schedule.next_wake_deadline, None);
}

/// Opening a row's page mounts the task's document where the row stands: the frame that shows it
/// adds the document's widgets to the retained layout tree and measures those, and no frame
/// composes the window's whole layout tree again for it. Rows open all day, and the window's
/// composition is the work none of them may cost — the row's document is a slot mount.
#[test]
fn opening_a_rows_page_mounts_the_document_where_the_row_stands() {
    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(SMALL);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = on_the_frame_clock(Session::tasks_over_in(&route, chunks, None));
    session.settle();
    let title = top_title(&mut session);
    let (x, y) = shown_at(&mut session, &title);
    let before = Lay::of(&mut session);
    let drawn = session.window.harness().drawn_commands();
    session.window.click_at(x, y);
    let rest = at_rest(&mut session, drawn);
    let after = Lay::of(&mut session);
    let page = opened_title_field(&mut session)
        .unwrap_or_else(|| panic!("the row a person opened shows the page of {title}"));
    assert!(
        !page.is_empty(),
        "the page of {title} stands where the row did"
    );
    // The frame that showed it mounted the document and measured it; the window's own
    // composition stood, as it does for a row coming into view.
    assert_eq!(
        after.rebuilt,
        before.rebuilt,
        "opening the page of {title} composed the window's layout tree {} time(s) in the {} \
         rounds that showed it: the document mounts where its row stands",
        after.rebuilt - before.rebuilt,
        rest.rounds
    );
    // One frame placed every widget — the one that mounted the document. A window laid out
    // twice for one row is the cost a person waits on.
    assert_eq!(
        rest.full, 1,
        "the step placed every widget of the window {} times in {} rounds: one mounts the \
         document and the rest move what they show",
        rest.full, rest.rounds
    );
    assert_eq!(
        rest.full_unasked, 0,
        "a frame placed the whole window with nothing composed or measured"
    );
}

/// Choosing a note in the tree shows it beside the tree, and pressing the note the window is
/// already reading shows what is there: the content column's document stands, reconciled in
/// place, and no frame composes the window's layout tree again for a press on it.
#[test]
fn choosing_a_note_in_the_tree_opens_it_without_composing_the_window() {
    let mut session = Session::rich();
    let plus = session.new_note_button();
    for round in 0..3 {
        session.window.click_at(plus.0, plus.1);
        session.settle();
        let title = session.field("Untitled");
        session.window.click_at(title.0, title.1);
        session.window.type_text(&format!("Note {round}"));
        session.settle();
    }
    let titles = session.titles();
    assert_eq!(titles.len(), 3, "the tree shows the notes made: {titles:?}");
    session.settle();
    for title in titles {
        let (x, y) = shown_at(&mut session, &title);
        // The first press chooses it; the one after it presses the note the window reads.
        session.window.click_at(x, y);
        session.settle();
        let before = Lay::of(&mut session);
        session.window.click_at(x, y);
        session.settle();
        let after = Lay::of(&mut session);
        assert_eq!(
            after.rebuilt, before.rebuilt,
            "pressing {title} again composed the window's layout tree: the note stands beside \
             the tree already"
        );
    }
}

/// The nodes another document may invalidate: the editor's own stack, its own node and the
/// nodes that hold it up to the window's column, never the window's composition.
const EDITOR_SUBTREE: usize = 12;

/// Reading one note after another: choosing another note in the tree shows that note where the
/// last one stood — the column's editor lays its own blocks out again — and nothing above the
/// editor is replaced, so no frame composes the window's layout tree for the note a person
/// turned to. Notes are read one after another all day, and the window around them stands.
#[test]
fn choosing_another_note_in_the_tree_keeps_the_windows_composition() {
    let _motion = ui::motion::testing::settled();
    let mut session = Session::rich();
    let plus = session.new_note_button();
    for round in 0..2 {
        session.window.click_at(plus.0, plus.1);
        session.settle();
        let title = session.field("Untitled");
        session.window.click_at(title.0, title.1);
        session.window.type_text(&format!("Note {round}"));
        session.settle();
        // The notes read differently: it is their bodies that another note replaces.
        let body = session.field("Write something");
        session.window.click_at(body.0, body.1);
        session
            .window
            .type_text(&format!("What note {round} is about."));
        session.settle();
    }
    let titles = session.titles();
    assert_eq!(titles.len(), 2, "the tree shows the notes made: {titles:?}");
    // The first press chooses the note the window then reads; what is measured is the next
    // choice, the one a person makes over and over.
    let (x, y) = shown_at(&mut session, &titles[0]);
    session.window.click_at(x, y);
    session.settle();
    let before = Lay::of(&mut session);
    let drawn = session.window.harness().drawn_commands();
    let (x, y) = shown_at(&mut session, &titles[1]);
    session.window.click_at(x, y);
    let rest = at_rest(&mut session, drawn);
    let after = Lay::of(&mut session);
    assert_eq!(
        after.rebuilt,
        before.rebuilt,
        "choosing {} composed the window's layout tree {} time(s) in the {} rounds that showed \
         it: the note is read in the editor that stands",
        titles[1],
        after.rebuilt - before.rebuilt,
        rest.rounds
    );
    assert!(
        rest.marked <= EDITOR_SUBTREE,
        "choosing {} asked the layout to measure {} of the window's {} nodes: the editor's own \
         stack and what holds it",
        titles[1],
        rest.marked,
        after.nodes
    );
    assert_eq!(
        rest.full_unasked, 0,
        "a frame placed the whole window with nothing composed or measured"
    );
}

/// The same where the document is a task's: opening another task in the list reads its notes
/// in the editor that stands, so the page a person turns to costs its own blocks and never the
/// window's composition.
#[test]
fn choosing_another_task_keeps_the_windows_composition() {
    let _motion = ui::motion::testing::settled();
    let chunks = seeded_tasks(SMALL);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = on_the_frame_clock(Session::tasks_over_in(&route, chunks, None));
    session.settle();
    let titles = session.titles();
    assert!(titles.len() > 1, "the list shows rows: {titles:?}");
    let (x, y) = shown_at(&mut session, &titles[0]);
    session.window.click_at(x, y);
    session.settle();
    let first = opened_title_field(&mut session)
        .unwrap_or_else(|| panic!("the row a person opened shows the page of {}", titles[0]));
    assert!(!first.is_empty(), "the page of {} stands", titles[0]);
    let before = Lay::of(&mut session);
    let drawn = session.window.harness().drawn_commands();
    let (x, y) = shown_at(&mut session, &titles[1]);
    session.window.click_at(x, y);
    let rest = at_rest(&mut session, drawn);
    let after = Lay::of(&mut session);
    let second = opened_title_field(&mut session)
        .unwrap_or_else(|| panic!("the row a person chose shows the page of {}", titles[1]));
    assert_ne!(
        second, first,
        "the window shows the page of the task the person chose"
    );
    assert_eq!(
        after.rebuilt,
        before.rebuilt,
        "choosing {} composed the window's layout tree {} time(s) in the {} rounds that showed \
         it: its notes are read in the editor that stands",
        titles[1],
        after.rebuilt - before.rebuilt,
        rest.rounds
    );
    assert!(
        rest.marked <= after.nodes as usize,
        "choosing {} asked the layout to measure {} of the window's {} nodes",
        titles[1],
        rest.marked,
        after.nodes
    );
    assert_eq!(
        rest.full_unasked, 0,
        "a frame placed the whole window with nothing composed or measured"
    );
}
