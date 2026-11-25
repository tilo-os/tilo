//! What the interactions of a window of a workspace a person has used for years cost: the
//! table, read against what a person feels as instant.

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
// with the machine: a step over one is reported with its gap. What a step is held to is read
// against a reference the same run measures on the same thread beside every step ([`reference`]):
// a machine whose other work slows the step (a core's other thread, its caches, its clock) slows
// the reference as much, so a step held as so many references fails when it does more, not when
// the machine is busy. The check on the work a change does is also the instruction count the
// `budgets` test holds.

/// The workspace the interactions are measured in: what a person who has used Tasks for years
/// keeps.
const SEEDED: usize = 10_000;

/// A workspace a tenth the size of the one a person has used for years.
const SMALL: usize = 1_000;

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
/// for noise, on a machine whose [`reference`] takes [`REFERENCE`]. A step past it got slower by
/// a clear margin; the target is where the step is brought down to, and this follows it down.
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

/// The CPU time [`reference`] takes where [`Held`] was measured, in each build: what a step's
/// time is read against. A step that took `cpu_us` beside a reference that took `reference_us`
/// costs `cpu_us * REFERENCE / reference_us` on that machine.
const REFERENCE: Held = Held {
    optimized_us: 8_300,
    development_us: 36_000,
};

/// A fixed piece of the work a step does, timed in UI-thread CPU microseconds: formatting,
/// hashing and ordering strings and the allocations under them. It does the same work in every
/// run, so what moves its time is the machine's: another thread on the core, the caches and the
/// memory bus other work shares, the clock. A step is measured between two of them, and read
/// against what they took ([`Cost::held_us`]).
fn reference() -> u64 {
    const ITEMS: u64 = 20_000;
    let start = ui::rt::thread_cpu_time();
    let wall = ui::time::Instant::now();
    let mut by_name = std::collections::HashMap::new();
    let mut ordered = BTreeMap::new();
    for i in 0..ITEMS {
        let name = format!("task {i:06} {}", i.wrapping_mul(2_654_435_761) % 9_973);
        by_name.insert(name.clone(), i);
        ordered.insert(name, i);
    }
    let mut names: Vec<&String> = ordered.keys().collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    let found: u64 = names.iter().filter_map(|name| by_name.get(*name)).sum();
    std::hint::black_box(found);
    since(start, wall.elapsed())
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
    /// What [`reference`] took beside the step: the mean of one just before it and one just
    /// after.
    reference_us: u64,
}

impl Cost {
    fn eval_layout_us(&self) -> u64 {
        self.instance_us.saturating_add(self.layout_us)
    }

    /// The step's CPU time on the machine [`Held`] was measured on: its time scaled by what the
    /// reference took there over what it took beside the step.
    fn held_us(&self) -> u64 {
        let reference = REFERENCE.here();
        let scaled =
            u128::from(self.cpu_us) * u128::from(reference) / u128::from(self.reference_us.max(1));
        u64::try_from(scaled).unwrap_or(u64::MAX)
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
            "interaction        steps  frames  cpu_p50  cpu_max  shown  instance  window  event  sync  layout  paint  wall  reference"
        );
        for label in self.0.keys() {
            eprintln!(
                "{label:<18} {:>5} {:>7} {:>8} {:>8} {:>6} {:>9} {:>7} {:>6} {:>5} {:>7} {:>6} {:>5} {:>9}",
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
                ms(self.p50(label, |c| c.reference_us)),
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
            let on_reference = self.p50(label, Cost::held_us);
            let reference = self.p50(label, |cost| cost.reference_us);
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
            if on_reference > held {
                past.push(format!(
                    "{label}: {} ms of CPU beside a reference of {} ms, {} ms where the reference \
                     takes {} ms, held to {} ms (instant is {} ms)",
                    ms(cpu),
                    ms(reference),
                    ms(on_reference),
                    ms(REFERENCE.here()),
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
/// ([`tilo_app_window::AppWindow`] under `ui::headless::Driver`): the key down, the text and
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
            // A window on the frame clock draws when its tree owes work or asks to be drawn
            // again: a list placed by the last frame asks for the rows it now has room for.
            let schedule = window.frame_schedule();
            if !(schedule.queued_work || schedule.needs_redraw) {
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
        // What the tree itself owes a frame: the work of the step, or of the turn just taken. A
        // frame the window's own clock asks for, a glide or a fade going on, is owed as well, but
        // it is no work of the step's: how many of them run is how far the machine got through
        // the motion while the step was measured.
        let step_frame = changed || session.window.harness().frame_schedule().queued_work;
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
        // the workspace's documents, one a turn, waking the window as each moves what a view shows
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
/// a turn changed what it shows or its tree owes one ([`frame_if_asked`]), until nothing is left
/// to do. A frame drawn on every turn regardless is work no window at rest does.
fn opened_at_rest(session: &mut Session) {
    let mut quiet = 0_u32;
    let mut rounds = 0_u32;
    while quiet < 3 {
        let changed = session.host.turn();
        let framed = frame_if_asked(session, changed);
        if changed || framed {
            quiet = 0;
        } else {
            quiet = quiet.saturating_add(1);
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
    let before = reference();
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
        reference_us: before.midpoint(reference()),
    }
}

/// What a person feels as instant, for the interactions this measures: 8 ms for a keystroke,
/// 16 ms for a step a person waits on, 8 ms for a frame of scrolling under the pointer, and
/// 300 ms to open ten thousand tasks — the one step that reads the whole workspace, once. They
/// are the targets, not what this workspace costs: a step over one is reported with its gap (see
/// [`Costs::report`]), and one past what it is held to ([`Held`]) fails.
///
/// The same targets, counted rather than timed, are how a change is guarded: a modern core of
/// 3.5 GHz at an IPC of 2.5 retires 8.75 million instructions a millisecond, so 8 ms is 70
/// million instructions, 16 ms 140 million and 300 ms 2.6 billion, and a callgrind run counts
/// the same work the same on every machine. This test reports the times and fails a step past
/// what it is held to.
const TARGETS: &[(&str, Target)] = &[
    (
        "open",
        Target {
            step_us: 300_000,
            eval_layout_us: 300_000,
            held: Held {
                optimized_us: 289_000,
                development_us: 1_470_000,
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
                optimized_us: 100_000,
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
                optimized_us: 84_000,
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
                optimized_us: 60_000,
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

/// How many windows the open is measured over ([`what_the_interactions_a_person_feels_cost`]).
const OPENS: usize = 3;

/// The workspace a person who has used Tasks for years keeps: `n` tasks, seeded through a
/// window's own host as the store's origin writes what it brings in.
pub(super) fn seeded_tasks(n: usize) -> Rc<tilolang_process::testing::Bus> {
    let bus = Rc::new(tilolang_process::testing::Bus::default());
    seed_into(&bus, n);
    bus
}

/// Write `n` tasks into the workspace `bus` holds.
pub(super) fn seed_into(bus: &Rc<tilolang_process::testing::Bus>, n: usize) {
    bus.member(ui::tilo_actions::MemberId::person("ada"), "Ada");
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let spec = Spec::new(TASKS, route.cluster_id.to_string(), 1, "ada").in_language("en-US");
    // The seeding window runs its host, not its frames: it draws nothing a person sees.
    let mut seeder = super::Session::opened_on_bus(bus, &route, spec);
    seeder.settle_host();
    tilolang_tilo::seed::seed(&seeder.instance_host(), TASKS, n, "2026-06-30T09:00:00Z")
        .expect("the workspace seeds");
    // The seeding window draws none of it: the workspaces a test opens do.
    drop(seeder);
}

/// An import lands as the store's origin write: the tasks a person brings in are records of the
/// workspace's store, stamped `$origin`, of the kind their package declares.
#[test]
fn a_tasks_import_lands_in_the_workspace_store() {
    let bus = seeded_tasks(3);
    assert!(
        bus.workspace()
            .written()
            .iter()
            .any(|stamp| stamp.verb == collab_types::Stamp::ORIGIN_VERB),
        "the import is the store's origin write"
    );
    let listed = bus
        .workspace()
        .command(
            collab_types::store::wire::Command::Query(collab_types::store::Query::of(
                "tl.so.tilo.tasks.Task",
                100,
            )),
            "ada",
        )
        .expect("the workspace answers its tasks");
    let collab_types::store::wire::Reply::Page(page) = listed else {
        panic!("a query answers a page: {listed:?}");
    };
    assert_eq!(
        page.resources.len(),
        3,
        "every imported task is a record of the workspace's store"
    );
}

/// A person at a Tasks window over `bus`'s workspace, its first frame not run yet.
fn launched_in(bus: &Rc<tilolang_process::testing::Bus>, route: &RouteSpec) -> Session {
    super::Session::opened_on_bus(bus, route, spec_of(route, "ada"))
}

/// A person at a Tasks window over `bus`'s workspace, settled on the Inbox: the place the
/// seeded tasks wait in.
pub(super) fn opened_at_rest_in(
    bus: &Rc<tilolang_process::testing::Bus>,
    route: &RouteSpec,
) -> Session {
    let mut session = launched_in(bus, route);
    session.settle();
    show_inbox(&mut session);
    session
}

/// The person picks the sidebar's Inbox, where the seeded tasks wait.
fn show_inbox(session: &mut Session) {
    let (x, y) = place_at(session, "Inbox");
    session.window.click_at(x, y);
    session.settle();
    assert!(chosen(session, "Inbox"), "the sidebar shows Inbox chosen");
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
        .and_then(|node| node.shown_name().map(str::to_owned))
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
        node.shown_name().unwrap_or_default().to_owned(),
        rect.x0 + 4.0 + BOX / 2.0,
        f64::midpoint(rect.y0, rect.y1),
    )
}

/// Whether the list shows `title` checked off.
fn checked_off(session: &mut Session, title: &str) -> bool {
    accessible(session).into_iter().any(|node| {
        node.role == ui::a11y::Role::ListItem
            && node.answers_to(title)
            && node.state.checked == Some(true)
    })
}

/// Whether the sidebar shows `place` chosen.
fn chosen(session: &mut Session, place: &str) -> bool {
    accessible(session).into_iter().any(|node| {
        node.role == ui::a11y::Role::TreeItem && node.answers_to(place) && node.state.selected
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

    // The workspace is seeded before the window opens: what opening it costs is the window's
    // work over what it shows, not the workspace's making.
    let bus = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    // The open is one long step, over which what else the machine runs changes more than the two
    // references beside it see: it is measured over three windows, each closed before the next
    // opens, and read at the middle one.
    let mut session = None;
    for _ in 0..OPENS {
        if let Some(earlier) = session.take() {
            Session::close(earlier);
        }
        let before = reference();
        let start = ui::rt::thread_cpu_time();
        let wall = ui::time::Instant::now();
        let mut opened = on_the_frame_clock(launched_in(&bus, &route));
        opened_at_rest(&mut opened);
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
            reference_us: before.midpoint(reference()),
        });
        session = Some(opened);
    }
    let mut session = session.expect("a window opened");
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
    /// Records the instance's publication looked at, one per document per turn.
    published: u64,
    /// Times it read every document in full.
    sweeps: u64,
    /// The actions the instance published to the workspace's surface.
    surface: usize,
    /// The rows the list rendered.
    rows: usize,
    /// What the views sent the window.
    patches: u64,
    /// The references to collection items its code built: a loop over every task builds one
    /// per task, and a count or a list of labels the index answers builds none.
    built: u64,
}

impl Opened {
    /// The work opening the window of the session `session` did.
    fn of(session: &mut Session) -> Self {
        let rows = session.titles().len();
        // The window's search index work is the daemon's: opening a window publishes no entry.
        session.assert_no_index_delta();
        let hosted = session.instance();
        let instance = hosted.instance();
        Self {
            published: instance.visited_documents(),
            sweeps: instance.sweeps(),
            surface: hosted.published().len(),
            rows,
            patches: instance.patches_sent(),
            built: instance
                .host()
                .map_or(0, |host| host.runtime().items_built()),
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
            built,
        } = self;
        format!(
            "{published} documents published ({sweeps} sweeps), {surface} actions on the surface, \
             {rows} rows, {patches} patches, {built} items built"
        )
    }
}

/// Opening a window costs what the window shows, not what the workspace holds: the same window
/// over a thousand tasks and over ten thousand does the same work for the frame that opens it —
/// the rows it shows, the actions it offers and the documents it publishes — give or take a
/// constant. Opening is the one step the workspace's size must not reach: a person feels it as
/// the window appearing. The search index is the device's daemon's, read from the store, so
/// opening a window publishes nothing of it.
fn opening_a_window_does_the_work_the_window_shows(
    seeded: &Rc<tilolang_process::testing::Bus>,
    large: &Rc<tilolang_process::testing::Bus>,
) {
    let _motion = ui::motion::testing::settled();
    let mut opened = Vec::new();
    for (n, bus) in [(1_000, seeded), (SEEDED, large)] {
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        // The window as it opens: the first frame that shows it, which is what a person waits
        // for — not the turns after it.
        let mut session = launched_in(bus, &route);
        session.settle();
        session.host.turn();
        session.window.frame();
        // The seeded tasks wait in the Inbox: the same place switch shows both sizes' rows.
        show_inbox(&mut session);
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
    // shows no more rows, offers no more actions and publishes no more documents. The constant
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
    hold("documents published", small.published, large.published, 2);
    hold("sweeps", small.sweeps, large.sweeps, 0);
    hold(
        "actions on the surface",
        small.surface as u64,
        large.surface as u64,
        0,
    );
    hold("rows", small.rows as u64, large.rows as u64, 0);
    hold("patches", small.patches, large.patches, 2);
    // The window's code reads the tasks the list shows, never every task: what waits in a place
    // and the labels in use are the index's answers.
    hold("items built", small.built, large.built, 64);
    assert!(
        over.is_empty(),
        "opening a window does the work the window shows:\n{}",
        over.join("\n")
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
        task_in_workspace(session, "Buy", false),
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

/// Scrolling a list of ten thousand tasks: the app is asked for the rows a scroll is about to
/// reach before it reaches them, so a step costs the window's frames and one ask, or none. What
/// a scroll step asks of the app is what makes the person wait for it, so it is counted, not
/// timed: a step that asked once per row would pass any budget on a fast enough machine.
fn scrolling_asks_the_app_for_rows_ahead_not_once_a_row(
    seeded: &Rc<tilolang_process::testing::Bus>,
) {
    let _motion = ui::motion::testing::settled();
    let bus = Rc::clone(seeded);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = opened_at_rest_in(&bus, &route);
    session.settle();
    let first = top_title(&mut session);
    let at = shown_at(&mut session, &first);
    session.window.move_to(at.0, at.1);
    session.settle();

    let notches = 8;
    let before = top_title(&mut session);
    let mut asks = 0;
    for _ in 0..notches {
        session.window.scroll(0.0, -3.0);
        asks += turns_until_rest(&mut session);
        session.settle();
    }
    assert_ne!(top_title(&mut session), before, "the list scrolled");
    eprintln!("scroll: {notches} notches asked the app {asks} times");
    assert!(
        asks <= notches,
        "{notches} scroll steps asked the app {asks} times: a step costs one ask or none"
    );
}

/// The glide a wheel notch started at `at`, run to where it rests on the frame clock, the host
/// answering each frame before the next: where the list stands after a notch is what the notch
/// did, not how far the frames the step measured got before the window was looked at, which is
/// as far as the machine's speed let them.
fn glide_out(session: &mut Session, mut at: ui::time::Instant) {
    use ui::time::testing::with_monotonic_time;
    for _ in 0..400 {
        with_monotonic_time(at, || {
            session.settle_host();
            session.window.frame();
        });
        if !session
            .window
            .harness()
            .frame_schedule()
            .needs_animation_frame
        {
            return;
        }
        at += ui::time::Duration::from_millis(16);
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
        glide_out(session, ui::time::Instant::now());
        session.settle();
        let now = top_shown(session);
        assert_ne!(now, top, "the list scrolled");
        top = now;
    }
}

/// What a change to one task costs the search index: nothing. The device's daemon publishes a
/// tilolang app's entries, read from the workspace's store one document at a time, so a window's
/// change — one task of ten thousand — sends no index delta at all.
fn a_change_indexes_nothing(seeded: &Rc<tilolang_process::testing::Bus>) {
    use ui::winit::keyboard::NamedKey;

    let _motion = ui::motion::testing::settled();
    let bus = Rc::clone(seeded);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = opened_at_rest_in(&bus, &route);
    session.settle();

    session.window.chord(&["ctrl"], "n");
    session.settle();
    // The person's entry keeps what they typed in this workspace, as their own state: this step
    // starts from an empty one.
    session.window.press_key(NamedKey::Escape);
    session.settle();
    session.window.type_text("Buy milk");
    session.forget_sent();
    session.window.press_key(NamedKey::Enter);
    session.settle();
    assert!(
        task_in_workspace(&session, "Buy milk", false),
        "the task the entry added is in the workspace"
    );
    session.assert_no_index_delta();
}

/// The boundaries the window's views have rendered since it opened: every app instance the
/// window runs, and both levels it shows.
fn renders(session: &Session) -> u64 {
    session
        .host
        .instances()
        .flat_map(|instance| {
            let instance = instance.instance();
            [
                tilo_render_protocol::patch::Level::Root,
                tilo_render_protocol::patch::Level::Detail,
            ]
            .into_iter()
            .filter_map(|level| instance.view(level))
            .collect::<Vec<_>>()
        })
        .map(|view| view.borrow().stats().renders)
        .fold(0, u64::saturating_add)
}

/// Typing a task in the list's entry: a keystroke changes the entry's own view state and
/// nothing else, so what runs again is the boundary that read it — the entry and the chips it
/// shows — and the list around it, every row of it, stands as it is.
fn typing_in_the_entry_renders_the_entry(seeded: &Rc<tilolang_process::testing::Bus>) {
    let _motion = ui::motion::testing::settled();
    let bus = Rc::clone(seeded);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = opened_at_rest_in(&bus, &route);
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
fn a_row_opens_where_it_stands_after_tasks_are_checked(
    seeded: &Rc<tilolang_process::testing::Bus>,
) {
    let _motion = ui::motion::testing::settled();
    let bus = Rc::clone(seeded);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = opened_at_rest_in(&bus, &route);
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

/// A task in another place opened from search, as a hit's `open` shows it, with or without a task
/// typed into the entry first: the window goes to the task's place and opens its row into its
/// page, though the place's rows arrive after the window names the task open. A row clicked
/// there next opens as well.
fn a_task_opened_from_search_shows_its_page(seeded: &Rc<tilolang_process::testing::Bus>) {
    use ui::winit::keyboard::NamedKey;

    let _motion = ui::motion::testing::settled();
    for typed in [false, true] {
        let bus = Rc::clone(seeded);
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        let mut session = opened_at_rest_in(&bus, &route);
        let today = session.titles();
        // The window publishes nothing to search: the daemon reads the store.
        session.assert_no_index_delta();
        if typed {
            session.window.chord(&["ctrl"], "n");
            session.settle();
            session.window.type_text("Buy milk");
            session.window.press_key(NamedKey::Enter);
            session.settle();
            assert!(
                task_in_workspace(&session, "Buy milk", false),
                "the entry's task is in the workspace"
            );
        }
        // A task of the Inbox, neither due today nor done: what a search hit of it names.
        let (title, anchor) =
            inbox_task(&session, &today).expect("a task of the Inbox Today does not show");
        session.instance_mut().reveal(anchor);
        session.settle();
        assert!(
            opened_title_field(&mut session).is_some(),
            "{title}, opened from search (typed first: {typed}), shows its page: rows {:?}",
            session.titles()
        );
        // Another row of the place the window moved to opens into its page as a click asks.
        let other = session
            .titles()
            .into_iter()
            .find(|shown| *shown != title)
            .expect("another row of the Inbox");
        let (x, y) = shown_at(&mut session, &other);
        session.window.click_at(x, y);
        session.settle();
        assert!(
            opened_title_field(&mut session).is_some(),
            "{other}, clicked after {title} opened from search (typed first: {typed}), shows its \
             page: rows {:?}",
            session.titles()
        );
    }
}

/// A task the window's `today` rows do not show, as a hit of it names it: the task's title and
/// the resource a search hit is addressed to, read from the workspace as the window's host reads
/// it.
fn inbox_task(session: &Session, today: &[String]) -> Option<(String, String)> {
    use tilolang_tilo::runtime::Value;
    let host = session.instance_host();
    let root = host.root_of(TASKS).ok()?;
    let Ok(Value::List(items)) = host.runtime().read(&root, "items") else {
        return None;
    };
    items.into_iter().find_map(|item| {
        let Value::Ref(item) = item else {
            return None;
        };
        let Ok(Value::Text(title)) = host.runtime().read(&item.target, "title") else {
            return None;
        };
        (!today.contains(&title))
            .then(|| (title, format!("tl:{TASKS}:{}", item.target.id_string())))
    })
}

/// The task whose page the content column shows, as the resource its title field edits: the
/// column's page is reconciled in place, so its field keeps its key from one task to the next.
fn opened_task(session: &mut Session) -> Option<String> {
    let view = session.hosted().view()?;
    let field = find(view.composed()?, &|node| {
        node.kind == "ui.TextInput" && node.binding.as_ref().is_some_and(|b| b.field().is_some())
    })?;
    let (task, _) = field.binding.as_ref()?.field()?;
    Some(format!("{task:?}"))
}

/// The row held open into its item where it stands: the page a person reads there, and the
/// field they type the item's title into. `None` while no row stands open.
fn opened_title_field(session: &mut Session) -> Option<String> {
    let view = session.hosted().view()?;
    let edits_a_field = |node: &tilo_app_window::UiNode| {
        node.kind == "ui.TextInput" && node.binding.as_ref().is_some_and(|b| b.field().is_some())
    };
    // The opened task's page stands in the shell's content column, beside the list.
    // What the window composed carries the keys its widgets are laid out under.
    if let Some(field) = view.composed().and_then(|tree| find(tree, &edits_a_field)) {
        return Some(field.key.clone());
    }
    let mut found = None;
    for list in view.lists() {
        if list.starts_with(tilo_app_window::shell::DETAIL) {
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

/// Checking a task off moves it to the head of its column, among the tasks of its status: the
/// store answers which those are from its index, so the action reads the task, its neighbour and
/// what stands under and over it, and none of the workspace's other tasks. Checking one task in
/// ten thousand builds as many references to items as checking one in a hundred.
fn checking_a_task_reads_its_column_from_the_index(seeded: &Rc<tilolang_process::testing::Bus>) {
    /// What a workspace a hundred times the size may add to a check-off: a page of the column's
    /// listing.
    const SLACK: u64 = 64;

    let _motion = ui::motion::testing::settled();
    let mut built = Vec::new();
    for n in [100, SEEDED] {
        let bus = if n == SEEDED {
            Rc::clone(seeded)
        } else {
            seeded_tasks(n)
        };
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        let mut session = opened_at_rest_in(&bus, &route);
        let items_built = |session: &Session| {
            session
                .instance()
                .instance()
                .host()
                .map_or(0, |host| host.runtime().items_built())
        };
        // The first check-off finds its column empty; the second moves among the first.
        let mut ran = 0;
        for _ in 0..2 {
            let (title, x, y) = first_check(&mut session);
            let before = items_built(&session);
            session.window.click_at(x, y);
            session.settle();
            ran = ran.max(items_built(&session) - before);
            assert!(
                checked_off(&mut session, &title),
                "the row a person checked in a workspace of {n} tasks reads checked: {title}"
            );
        }
        eprintln!("checking a task in {n} tasks built {ran} item references");
        built.push((n, ran));
    }
    let [(small_n, small), (large_n, large)] = built[..] else {
        panic!("two workspaces are measured");
    };
    assert!(
        large <= small.saturating_add(SLACK),
        "checking a task built {large} item references in {large_n} tasks and {small} in \
         {small_n}: the action reads every task of the workspace, not the column it moves in"
    );
}

/// Typing a task's title on its page: the keystroke edits one field of one task, so the page
/// that shows it — the row grown into it where it stands — and the row behind it run again, and
/// the rest of the window stands.
fn typing_a_tasks_title_reruns_the_views_that_read_it(seeded: &Rc<tilolang_process::testing::Bus>) {
    let _motion = ui::motion::testing::settled();
    let bus = Rc::clone(seeded);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = opened_at_rest_in(&bus, &route);
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

/// How long a window opened cold over a workspace of ten thousand tasks may take to draw its
/// first frame of them, wall time: what a person waits for after launching it.
const COLD_FIRST_FRAME: Held = Held {
    optimized_us: 1_000_000,
    development_us: 6_000_000,
};

/// A window opened cold over ten thousand tasks the workspace holds draws its sidebar and its
/// rows within a second: nothing is read whole before the first frame, so the wait follows what
/// the window shows, never what the workspace holds.
#[test]
fn a_cold_window_over_ten_thousand_tasks_draws_its_first_frame_within_a_second() {
    let _motion = ui::motion::testing::settled();
    let bus = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let started = ui::time::Instant::now();
    let mut session = on_the_frame_clock(launched_in(&bus, &route));
    let mut rounds = 0_u32;
    while session.hosted().view().is_none() || session.titles().is_empty() {
        let changed = session.host.turn();
        frame_if_asked(&mut session, changed);
        rounds = rounds.saturating_add(1);
        assert!(rounds < 10_000, "the window never showed a task");
    }
    let waited = started.elapsed().as_micros() as u64;
    assert!(
        waited <= COLD_FIRST_FRAME.here(),
        "the first frame of ten thousand tasks took {} ms, past {} ms",
        waited / 1000,
        COLD_FIRST_FRAME.here() / 1000
    );
}

/// Checking tasks off one after another: every row the person checked stays where it stands,
/// reading checked, the list goes on rendering, and checking one task runs a handful of
/// boundaries rather than every row the window shows.
#[test]
fn checking_tasks_one_after_another_keeps_the_list_rendering() {
    let _motion = ui::motion::testing::settled();
    let bus = seeded_tasks(100);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = opened_at_rest_in(&bus, &route);
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
/// once, on the change the document brought back with what its derivation added, and sends the
/// tree once. The row then reads its title and its box checked off, in a workspace of a hundred
/// tasks and of ten thousand.
fn a_step_reaches_the_window_in_one_frame(seeded: &Rc<tilolang_process::testing::Bus>) {
    let _motion = ui::motion::testing::settled();
    for n in [100, SEEDED] {
        // The ten thousand are the test's, seeded once; the hundred are seeded here.
        let bus = if n == SEEDED {
            Rc::clone(seeded)
        } else {
            seeded_tasks(n)
        };
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        let mut session = opened_at_rest_in(&bus, &route);
        let counted = |session: &Session| -> (u64, u64) {
            let instance = session.instance().instance();
            let runs = instance
                .view(tilo_render_protocol::patch::Level::Root)
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
// week. These are the same steps counted: what the window's instance read of the document and of
// the resource store, how many items a filter was checked against one by one, which
// boundaries it rendered and what it sent the window; and what the window's layout built and
// its text shaped. A step costs what it changed and what the window shows, so every one of
// them reads the same in a workspace ten times the size.

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
fn a_step_lays_out_what_it_changes(seeded: &Rc<tilolang_process::testing::Bus>) {
    /// A step's name and the input a person gives the window.
    type Step = (&'static str, Box<dyn FnOnce(&mut Session)>);

    /// The nodes a frame may invalidate for a keystroke or a notch of the scroll: the field or
    /// the rows that moved and the nodes above them.
    const MEASURED: u64 = 12;

    /// The frames one input may run: the frame of the window's clock it asks for, and one more
    /// for what it changed.
    const STEPS_INPUT_FRAMES: u64 = 2;

    let _motion = ui::motion::testing::settled();
    let doc = Rc::clone(seeded);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = on_the_frame_clock(opened_at_rest_in(&doc, &route));
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
    let bus = seeded_tasks(20);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = opened_at_rest_in(&bus, &route);
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

/// Opening the first row's page brings the content column in beside the list: the window composes
/// its layout once for the column, in the one frame that places every widget, and no frame after
/// it composes again. A page opened while the column stands costs no composition at all
/// ([`choosing_another_task_keeps_the_windows_composition`]).
fn opening_a_rows_page_mounts_the_document_where_the_row_stands(
    seeded: &Rc<tilolang_process::testing::Bus>,
) {
    let _motion = ui::motion::testing::settled();
    let bus = Rc::clone(seeded);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = on_the_frame_clock(opened_at_rest_in(&bus, &route));
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
    // The column the page stands in came in once; nothing composed the window again after it.
    assert_eq!(
        after.rebuilt - before.rebuilt,
        1,
        "opening the page of {title} composed the window's layout tree {} time(s) in the {} \
         rounds that showed it: the content column comes in once",
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
    for round in 0..3 {
        press_new_note(&mut session);
        session.settle();
        let title = session.field("title");
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
    for round in 0..2 {
        press_new_note(&mut session);
        session.settle();
        let title = session.field("title");
        session.window.click_at(title.0, title.1);
        session.window.type_text(&format!("Note {round}"));
        session.settle();
        // The notes read differently: it is their bodies that another note replaces.
        let body = session.field("body");
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
fn choosing_another_task_keeps_the_windows_composition(
    seeded: &Rc<tilolang_process::testing::Bus>,
) {
    let _motion = ui::motion::testing::settled();
    let bus = Rc::clone(seeded);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = on_the_frame_clock(opened_at_rest_in(&bus, &route));
    session.settle();
    let titles = session.titles();
    assert!(titles.len() > 1, "the list shows rows: {titles:?}");
    let (x, y) = shown_at(&mut session, &titles[0]);
    session.window.click_at(x, y);
    session.settle();
    let first = opened_task(&mut session)
        .unwrap_or_else(|| panic!("the row a person opened shows the page of {}", titles[0]));
    let before = Lay::of(&mut session);
    let drawn = session.window.harness().drawn_commands();
    let (x, y) = shown_at(&mut session, &titles[1]);
    session.window.click_at(x, y);
    let rest = at_rest(&mut session, drawn);
    let after = Lay::of(&mut session);
    let second = opened_task(&mut session)
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

/// Opening a window at a thousand tasks and at ten thousand: the work the open does.
#[test]
fn opening_a_window_does_the_work_it_shows() {
    let thousand = seeded_tasks(1_000);
    let ten_thousand = seeded_tasks(SEEDED);
    opening_a_window_does_the_work_the_window_shows(&thousand, &ten_thousand);
}

/// A step in ten thousand tasks lays out what it changes, and a scroll asks the app for rows
/// ahead.
#[test]
fn a_step_in_ten_thousand_tasks_lays_out_what_it_changes_and_a_scroll_asks_ahead() {
    let ten_thousand = seeded_tasks(SEEDED);
    a_step_lays_out_what_it_changes(&ten_thousand);
    scrolling_asks_the_app_for_rows_ahead_not_once_a_row(&ten_thousand);
}

/// A task typed into the entry of ten thousand: its keystrokes render the entry, and the task
/// they add sends no search index delta: the daemon indexes it from the store.
#[test]
fn a_task_typed_into_the_entry_renders_the_entry_and_indexes_the_task() {
    let ten_thousand = seeded_tasks(SEEDED);
    typing_in_the_entry_renders_the_entry(&ten_thousand);
    a_change_indexes_nothing(&ten_thousand);
}

/// A task of ten thousand checked off, and one retitled on its page: each reaches the window
/// through the views that read it.
#[test]
fn a_check_off_and_a_title_typed_in_ten_thousand_tasks_run_what_reads_them() {
    let ten_thousand = seeded_tasks(SEEDED);
    a_step_reaches_the_window_in_one_frame(&ten_thousand);
    checking_a_task_reads_its_column_from_the_index(&ten_thousand);
    typing_a_tasks_title_reruns_the_views_that_read_it(&ten_thousand);
}

/// A row of a thousand tasks opened into its page: where the row stands, beside the page of
/// another, after tasks were checked off, and from search.
#[test]
fn a_row_of_a_thousand_tasks_opens_into_its_page_where_it_stands() {
    let thousand = seeded_tasks(SMALL);
    opening_a_rows_page_mounts_the_document_where_the_row_stands(&thousand);
    choosing_another_task_keeps_the_windows_composition(&thousand);
    a_row_opens_where_it_stands_after_tasks_are_checked(&thousand);
    a_task_opened_from_search_shows_its_page(&thousand);
}

impl crate::motion::Framed for Session {
    /// The host's turns: it takes in what the window sent and sends back what it answers, as the
    /// session's loop runs them beside the window's.
    fn answer(&mut self) {
        self.settle_host();
    }

    fn renders(&mut self) -> u64 {
        use tilo_render_protocol::patch::Level;
        let instance = self.instance().instance();
        [Level::Root, Level::Detail]
            .into_iter()
            .filter_map(|level| instance.view(level))
            .map(|view| view.borrow().stats().renders)
            .sum()
    }
}

/// A task ticked off in a session's window fills its box over frames that cost what a still frame
/// costs: the instance answers the press, and the mark's own frames repaint only.
#[test]
fn a_frame_of_the_mark_in_a_sessions_window_costs_what_a_still_frame_costs() {
    let mut session = Session::tasks();
    session.instance_mut().stage("a busy week");
    session.settle();
    crate::motion::a_mark_costs_a_still_frame(&mut session);
}
