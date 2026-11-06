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
// with the machine: a step over one is reported with its gap, and the gate on the work a change
// does is the instruction count the `budgets` test holds.

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
    /// over its target is reported, not failed: the gate on the work a change does is the
    /// instruction count [`tilolang_tilo::bench::Work`] and `tilolang-tilo`'s `budgets` test
    /// hold, which counts the same work the same on every machine, where this table measures
    /// the machine it runs on as much as the window.
    #[allow(
        clippy::print_stderr,
        reason = "why a measurement was not held is part of its report"
    )]
    fn report(&self, targets: &[(&'static str, Target)]) {
        let ms = |us: u64| format!("{:.2}", us as f64 / 1000.0);
        let gap = |measured: u64, target: u64| {
            if measured == 0 {
                "met".to_owned()
            } else {
                format!("{:.1}x target", measured as f64 / target as f64)
            }
        };
        eprintln!("interaction        cpu p50 vs instant   instance+layout p50 vs instant");
        let mut behind = Vec::new();
        for (label, target) in targets {
            if !self.0.contains_key(label) {
                behind.push(format!("{label}: no interaction measured"));
                continue;
            }
            let cpu = self.p50(label, |cost| cost.cpu_us);
            let eval_layout = self.p50(label, Cost::eval_layout_us);
            eprintln!(
                "{label:<18} {:>10} ms ({:>9}), {:>10} ms ({:>9})",
                ms(cpu),
                gap(cpu, target.step_us),
                ms(eval_layout),
                gap(eval_layout, target.eval_layout_us),
            );
            if cpu > target.step_us || eval_layout > target.eval_layout_us {
                behind.push(label.to_string());
            }
        }
        if !behind.is_empty() {
            eprintln!(
                "not instant yet: {} - reported, not failed: the gate counts instructions",
                behind.join(", ")
            );
        }
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

/// The host's turns and the window's frames until neither has anything left to do: the host's
/// CPU time and the window's, in microseconds, and the frames' timings.
fn at_rest(session: &mut Session) -> (u64, u64, Vec<ui::profiling::FrameTiming>) {
    let (mut instance_us, mut window_us) = (0, 0);
    let mut quiet = 0_u32;
    while quiet < 3 {
        let start = ui::rt::thread_cpu_time();
        let wall = ui::time::Instant::now();
        let changed = session.host.turn();
        instance_us += since(start, wall.elapsed());
        let start = ui::rt::thread_cpu_time();
        let wall = ui::time::Instant::now();
        session.window.frame();
        window_us += since(start, wall.elapsed());
        if changed {
            quiet = 0;
        } else {
            quiet = quiet.saturating_add(1);
        }
    }
    let frames = session.window.harness().take_profile_samples();
    (instance_us, window_us, frames)
}

/// One interaction, measured from `input` reaching the window to the windows' work at rest with
/// what it did on screen.
fn cost(session: &mut Session, label: &'static str, input: impl FnOnce(&mut Session)) -> Cost {
    session.window.harness().begin_profiling();
    let start = ui::rt::thread_cpu_time();
    let wall = ui::time::Instant::now();
    input(session);
    let (instance_us, window_us, frames) = at_rest(session);
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
/// [`Costs::report`]), and the work of bringing it down is the publication's.
///
/// The same targets, counted rather than timed, are how a change is gated: a modern core of
/// 3.5 GHz at an IPC of 2.5 retires 8.75 million instructions a millisecond, so 8 ms is 70
/// million instructions, 16 ms 140 million and 300 ms 2.6 billion, and a callgrind run counts
/// the same work the same on every machine. This test reports the times;
/// [`tilolang_tilo::bench::Work`] and `tilolang-tilo`'s `budgets` test hold the counts (a change
/// publishes the rows it touched, whatever the workspace holds). Opening a workspace reads the
/// workspace's index rather than the workspace: [`opening_a_window_reads_the_workspaces_index`].
const TARGETS: &[(&str, Target)] = &[
    (
        "open",
        Target {
            step_us: 300_000,
            eval_layout_us: 300_000,
        },
    ),
    (
        "entry keystroke",
        Target {
            step_us: 8_000,
            eval_layout_us: 8_000,
        },
    ),
    (
        "entry enter",
        Target {
            step_us: 16_000,
            eval_layout_us: 16_000,
        },
    ),
    (
        "check a task",
        Target {
            step_us: 16_000,
            eval_layout_us: 16_000,
        },
    ),
    (
        "switch place",
        Target {
            step_us: 16_000,
            eval_layout_us: 16_000,
        },
    ),
    (
        "open a task page",
        Target {
            step_us: 16_000,
            eval_layout_us: 16_000,
        },
    ),
    (
        "scroll",
        Target {
            step_us: 8_000,
            eval_layout_us: 8_000,
        },
    ),
];

/// Whether the workspace's record holds a task titled `title`, as the window's instance runs it:
/// the list shows only the rows on screen, so what the entry added is read off the record.
fn record_has_task(session: &Session, title: &str) -> bool {
    let doc = session
        .host
        .instances()
        .next()
        .expect("the window's instance")
        .record()
        .doc();
    let titled = |item: &tilolang_tilo::DynRecord| {
        item.schema().fields.iter().any(|field| {
            matches!(item.get(&field.id), Some(tilolang_tilo::DynValue::Text(text)) if text == title)
        })
    };
    tilolang_tilo::action::resource_ids(doc, TASKS)
        .into_iter()
        .filter_map(|id| tilolang_tilo::action::read_resource(doc, TASKS, &id).ok())
        .any(|record| {
            record
                .schema()
                .fields
                .iter()
                .any(|field| match record.get(&field.id) {
                    Some(tilolang_tilo::DynValue::Collection(items)) => {
                        items.iter().any(|(_, item)| titled(item))
                    }
                    _ => false,
                })
        })
}

/// The workspace a person who has used Tasks for years keeps: `n` tasks, as
/// `tilolang_tilo::bench` seeds one, in the record the installed Tasks package runs over.
fn seeded_tasks(n: usize) -> ui::tilo_resource::Doc {
    let tasks = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(package_of(TASKS));
    let packages = tilolang_tilo::bench::compile(&tasks).expect("the tasks package compiles");
    tilolang_tilo::bench::seeded_record(&packages, n).expect("the workspace seeds")
}

/// The title of the list's first row: where the list starts, and what a person reads there.
fn top_title(session: &mut Session) -> String {
    session
        .titles()
        .first()
        .cloned()
        .expect("the list shows a row")
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

    // The workspace is seeded before the window opens: what opening it costs is the window's
    // work, not the workspace's making. Whether the index answers that open is
    // [`opening_a_window_reads_the_workspaces_index`]'s.
    let doc = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let start = ui::rt::thread_cpu_time();
    let wall = ui::time::Instant::now();
    let mut session = Session::tasks_over_in(&route, doc, None);
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
    costs.report(TARGETS);
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
        &[SEEDED, SEEDED_LARGE]
    };
    let mut misses = Vec::new();
    for &n in sizes {
        // The record, and the index an earlier run of the app left on the disk.
        let doc = seeded_tasks(n);
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        let storage = kept_index(&route, &doc, &format!("open-{n}"));

        let start = ui::rt::thread_cpu_time();
        let wall = ui::time::Instant::now();
        let mut session = Session::tasks_over_launched(&route, doc, Some(storage));
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

/// A window's CPU time an open may take, in microseconds: making the store, reading the
/// workspace's indexes, loading the package and taking the record in. Only the last grows with
/// the workspace — the store keeps its own copy of the record, a fifth of a second of a hundred
/// thousand tasks — while the index is read rather than built from the record: built, ten
/// thousand tasks take seconds.
const OPEN_BUDGET_US: u64 = 500_000;

/// The same for a hundred thousand tasks: the record's copy is what the workspace's size adds
/// to an open.
const OPEN_BUDGET_LARGE_US: u64 = 900_000;

/// The directory the tests of this binary keep what a package keeps in, emptied first.
fn kept_root(name: &str) -> std::path::PathBuf {
    let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hosted-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    root
}

/// The storage an earlier run of the app left the workspace record `doc` of `route` indexes in:
/// its index is built from the record and on the disk, the file closed, before this returns.
fn kept_index(route: &RouteSpec, doc: &ui::tilo_resource::Doc, name: &str) -> std::path::PathBuf {
    let storage = kept_root(name);
    let kept = TilolangHost::new(Some(storage.clone()), Arc::new(|| {}))
        .kept(TASKS)
        .expect("the app keeps what it keeps under the storage");
    let indexes = kept.indexes.expect("the app's indexes are on the disk");
    let earlier = tilolang_tilo::host::Host::open(
        tilolang_tilo::host::Opening {
            record: Some(doc),
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
    earlier.wait_index();
    assert!(earlier.index_ready(), "the earlier run indexed the record");
    earlier.persist_index();
    drop(earlier);
    storage
}

/// The window's frames until its list shows a rendered row, as long as that took: from the
/// first frame that could show it. Panics when none does.
fn show_rows(session: &mut Session) -> u64 {
    let start = ui::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(120) {
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
            return start.elapsed().as_micros() as u64;
        }
        if !changed {
            std::thread::sleep(std::time::Duration::from_millis(2));
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
        record_has_task(session, "Buy"),
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

/// Opening a task's page: its row's title, whose page the content lane shows beside the list
/// while the overview keeps its empty state for the next time nothing is chosen.
fn open_pages(session: &mut Session, costs: &mut Costs) {
    for _ in 0..3 {
        let title = session
            .titles()
            .get(1)
            .cloned()
            .expect("the list shows a row");
        let (x, y) = shown_at(session, &title);
        costs.add(cost(session, "open a task page", |session| {
            session.window.click_at(x, y);
        }));
        session.settle();
        assert!(
            session.page().contains(&title),
            "the content lane shows the page of the task the row shows: {title}"
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
#[test]
fn scrolling_asks_the_app_for_rows_ahead_not_once_a_row() {
    let _motion = ui::motion::testing::settled();
    let doc = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, doc, None);
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

/// Scrolling the list: a wheel notch down and back up, the top row changing each time.
fn scroll_the_list(session: &mut Session, costs: &mut Costs) {
    let first = top_title(session);
    let at = shown_at(session, &first);
    session.window.move_to(at.0, at.1);
    session.settle();
    let mut top = top_title(session);
    for notch in 0..8 {
        let down = notch % 2 == 0;
        costs.add(cost(session, "scroll", |session| {
            session.window.scroll(0.0, if down { -3.0 } else { 3.0 });
        }));
        session.settle();
        let now = top_title(session);
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
    let doc = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, doc, None);
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
        record_has_task(&session, "Buy milk"),
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
    let doc = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, doc, None);
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

/// Typing a task's title on its page: the keystroke edits one field of one task, so the page
/// that shows it and the row behind it run again, and the rest of the window stands.
#[test]
fn typing_a_tasks_title_reruns_the_views_that_read_it() {
    let _motion = ui::motion::testing::settled();
    let doc = seeded_tasks(SEEDED);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, doc, None);
    let title = session.titles().get(1).cloned().expect("a row");
    let (x, y) = shown_at(&mut session, &title);
    session.window.click_at(x, y);
    session.settle();
    let key = {
        let view = session.hosted().view().expect("the window is connected");
        let tree = view.detail_tree().expect("the page is shown");
        let key = find(&tree, &|node| {
            node.kind == "ui.TextInput"
                && node.binding.as_ref().is_some_and(|b| b.field().is_some())
        })
        .unwrap_or_else(|| panic!("the page edits its title: {tree:?}"))
        .key
        .clone();
        format!("{}{key}", tilo_ui_host::shell::DETAIL)
    };
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
    let doc = seeded_tasks(100);
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, doc, None);
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
