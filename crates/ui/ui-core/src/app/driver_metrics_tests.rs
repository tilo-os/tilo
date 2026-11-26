//! What the display-less driver measures of an app's frames, read from the frames the app runs as a
//! window runs them: a scroll that keeps the layout tree and the text it measured, a rebuild that
//! lays out as the kept tree did, a window at rest that owes nothing, the profile of a scenario, input
//! left to the frame clock, the window's bar above the app's frame and an agent's mark on the control
//! it acts on.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use std::cell::Cell;
use std::rc::Rc;

use crate::headless::Driver;
use crate::state::State;
use crate::{
    widget::Button, widget::Column, widget::Scrollable, widget::Text, widget::resource_actions,
};
use tilo_ui_base::widget::Widget;

const VIEWPORT: (u32, u32) = (400, 300);

/// `rows` lines of text under one another.
fn rows(rows: usize) -> Column {
    (0..rows).fold(Column::new(), |column, row| {
        column.push(Box::new(Text::new(&format!("Row {row} of the list"))))
    })
}

/// `count` lines of text in a view that scrolls them.
fn long_list(count: usize) -> Box<dyn Widget> {
    Box::new(Scrollable::new_vertical().with_child(Box::new(rows(count))))
}

/// An app of its own showing a list and a button, as a component mounts it.
struct ListApp {
    content: Column,
}

impl tilo_ui_base::widget::Decorator for ListApp {
    type Inner = Column;

    fn inner(&self) -> &Self::Inner {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.content
    }
}

impl crate::widget::AppWidget for ListApp {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }

    fn app_title(&self) -> &'static str {
        "List"
    }

    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("L")
    }
}

fn list_app() -> ListApp {
    ListApp {
        content: rows(12).push(Box::new(Button::new("more", "More"))),
    }
}

fn list_factory(
    mut context: crate::application::ProcessContext,
    _: crate::application::LaunchTarget,
    _: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch_error::LaunchError> {
    context
        .initialize_new_resource(|_| Ok(()))
        .map_err(crate::launch_error::LaunchError::Record)?;
    Ok(crate::application::AppComponent::new(
        context,
        Box::new(list_app()),
        tilo_actions::ActionRegistry::new(),
    ))
}

/// [`ListApp`] launched as an app is, under its own wake scope: the host that keeps its record, and
/// the driver showing it.
fn mounted_list() -> (crate::application_test_support::ProbeHarness, Driver) {
    let host = crate::application_test_support::ProbeHarness::new();
    let definition = crate::application_test_support::definition(list_factory);
    let target = crate::application::LaunchTarget::New;
    let context = host
        .prepare_with_permissions(
            &definition,
            73,
            &target,
            crate::application_test_support::runner_permissions(),
        )
        .unwrap();
    let component = definition
        .instantiate(context, target, crate::application::RestoreInput::None)
        .unwrap();
    let driver = Driver::mount(component).unwrap();
    (host, driver)
}

#[test]
fn scrolling_a_long_list_keeps_its_layout_tree_and_measures_no_text_again() {
    let mut driver = Driver::new(State::new(), long_list(200), VIEWPORT);
    driver.hover_at(200.0, 150.0);
    // The rows the scroll below brings into view are drawn once before it is measured.
    for dy in [-3.0, 3.0] {
        for _ in 0..5 {
            driver.scroll(0.0, dy);
        }
    }
    let top = driver.render_rgba();
    let nodes = driver.layout_node_ids();
    let (frames, rebuilt) = (driver.relayout_frames(), driver.rebuilt_frames());
    let (hits, misses) = driver.text_cache_stats();
    assert!(
        hits > 0 && misses > 0,
        "laying the rows out measured their text: {hits} hits, {misses} misses"
    );
    let _ = driver.take_text_shape_stats();

    for _ in 0..5 {
        driver.scroll(0.0, -3.0);
    }

    assert_ne!(driver.render_rgba(), top, "the list moved under the wheel");
    assert!(
        driver.relayout_frames() >= frames + 5,
        "every turn of the wheel ran a frame"
    );
    assert_eq!(
        driver.rebuilt_frames(),
        rebuilt,
        "no turn of the wheel built the layout tree again"
    );
    assert_eq!(
        driver.layout_node_ids(),
        nodes,
        "the scrolled list is the same tree"
    );
    assert!(!driver.last_relayout().rebuilt);
    assert_eq!(
        driver.text_cache_stats().1,
        misses,
        "no row's text was measured again"
    );
    let (_, shaped, _) = driver.take_text_shape_stats();
    assert_eq!(shaped, 0, "no row's text was shaped again");
    driver
        .verify_retained_geometry()
        .expect("the scrolled tree lays out as a fresh one does");
}

#[test]
fn a_rebuilt_tree_is_counted_and_verifying_the_kept_one_counts_nothing() {
    let mut driver = Driver::new(State::new(), long_list(40), VIEWPORT);
    let (frames, rebuilt) = (driver.relayout_frames(), driver.rebuilt_frames());
    let last = driver.last_relayout();

    driver
        .verify_retained_geometry()
        .expect("the kept tree lays out as a fresh one does");
    assert_eq!(
        (driver.relayout_frames(), driver.rebuilt_frames()),
        (frames, rebuilt),
        "the frame that checked the tree is not one the caller counts"
    );
    assert_eq!(driver.last_relayout(), last);

    driver.recompose().unwrap();
    assert_eq!(driver.rebuilt_frames(), rebuilt + 1, "a recomposed frame");
    let layout = driver.last_relayout();
    assert!(
        layout.rebuilt && layout.measured && !layout.placed_partly,
        "a rebuilt tree is solved and placed whole: {layout:?}"
    );
    assert_eq!(
        driver.layout_node_ids().len(),
        nodes_of(&driver),
        "every node of the tree is listed"
    );
}

/// The nodes of the app's layout tree, counted from its root.
fn nodes_of(driver: &Driver) -> usize {
    let taffy = &driver.app().layout.taffy;
    let mut pending = vec![driver.app().layout.root_node.unwrap()];
    let mut count = 0;
    while let Some(node) = pending.pop() {
        count += 1;
        pending.extend(taffy.children(node).unwrap());
    }
    count
}

#[test]
fn a_window_at_rest_owes_no_frame_and_a_frame_run_anyway_syncs_nothing() {
    let (_host, mut driver) = mounted_list();
    driver.idle();
    assert_eq!(
        driver.queued_work_sources(),
        Vec::<&str>::new(),
        "a window at rest is owed nothing"
    );
    assert_eq!(
        driver.redraw_sources(),
        Vec::<String>::new(),
        "no widget asks to be drawn again"
    );
    assert!(
        !driver.poll_frame().unwrap(),
        "the frame clock runs nothing"
    );

    let (quiet, drawn) = (driver.quiet_frames(), driver.drawn_commands());
    driver.relayout();
    assert_eq!(
        driver.quiet_frames(),
        quiet + 1,
        "the frame took up and synced nothing"
    );
    let layout = driver.last_relayout();
    assert!(
        !layout.rebuilt && !layout.measured && layout.marked_dirty == 0,
        "the frame neither built, solved nor marked anything: {layout:?}"
    );
    assert!(
        driver.drawn_commands() > drawn,
        "the frame drew the window it shows"
    );
}

#[test]
fn a_tree_shown_on_its_own_is_quiet_at_rest_and_syncs_once_woken() {
    let mut driver = Driver::new(State::new(), long_list(20), VIEWPORT);
    driver.idle();
    assert!(!driver.frame_owed(), "a tree at rest is owed nothing");
    let quiet = driver.quiet_frames();
    driver.relayout();
    assert_eq!(
        driver.quiet_frames(),
        quiet + 1,
        "a frame nothing woke the tree for syncs nothing"
    );
    driver.wake_handle().wake();
    assert!(driver.frame_owed(), "the tree's wake owes a frame");
    assert!(driver.poll_frame().unwrap());
    assert_eq!(
        driver.quiet_frames(),
        quiet + 1,
        "the woken frame took up what woke it"
    );
}

#[test]
fn profiling_samples_each_frame_by_pass_after_the_warmup_is_cleared() {
    let presses = Rc::new(Cell::new(0));
    let counted = Rc::clone(&presses);
    let root = rows(30).push(Box::new(
        Button::new("add", "Add").on_click(move || counted.set(counted.get() + 1)),
    ));
    let mut driver = Driver::new(State::new(), Box::new(root), VIEWPORT);
    assert!(
        driver.take_profile_samples().is_empty(),
        "nothing is sampled before profiling begins"
    );

    driver.begin_profiling();
    for _ in 0..3 {
        driver.profile_frame();
    }
    driver.clear_profile_samples();
    assert!(
        driver.take_profile_samples().is_empty(),
        "the warmup is gone"
    );

    driver.profile_frame();
    driver.click_named("add").unwrap();
    assert_eq!(presses.get(), 1);
    let samples = driver.take_profile_samples();
    assert!(
        samples.len() >= 2,
        "a sample for the frame and for the press: {samples:?}"
    );
    for sample in &samples {
        assert!(
            sample.draw_commands > 0,
            "each frame drew the window: {sample:?}"
        );
        assert!(
            sample.total_us >= sample.draw_us,
            "a frame costs at least its drawing: {sample:?}"
        );
    }
    assert!(
        samples.windows(2).all(|pair| pair[0].frame < pair[1].frame),
        "oldest first"
    );
    assert!(
        driver.take_profile_samples().is_empty(),
        "a take drains the samples"
    );
}

#[test]
fn input_left_to_the_frame_clock_reaches_the_app_at_once_and_is_drawn_by_the_next_frame() {
    let presses = Rc::new(Cell::new(0));
    let counted = Rc::clone(&presses);
    let root = Column::new().push(Box::new(
        Button::new("add", "Add").on_click(move || counted.set(counted.get() + 1)),
    ));
    let mut driver = Driver::new(State::new(), Box::new(root), VIEWPORT);
    driver.idle();
    driver.set_coalesce_input_frames(true);
    let frames = driver.relayout_frames();

    driver.click_named("add").unwrap();
    assert_eq!(presses.get(), 1, "the press reached the button at once");
    assert_eq!(
        driver.relayout_frames(),
        frames,
        "no frame ran for the press"
    );
    assert!(
        !driver.queued_work_sources().is_empty(),
        "the press left a frame owed"
    );
    assert!(driver.poll_frame().unwrap(), "the frame clock runs it");
    assert!(driver.relayout_frames() > frames);

    driver.set_coalesce_input_frames(false);
    let frames = driver.relayout_frames();
    driver.click_named("add").unwrap();
    assert_eq!(presses.get(), 2);
    assert!(
        driver.relayout_frames() > frames,
        "without the frame clock a press runs its frame"
    );
}

#[test]
fn the_windows_bar_stands_above_the_apps_frame() {
    let viewport = (600, 400);
    let mut driver = Driver::for_app(
        State::new(),
        list_app(),
        viewport,
        tilo_ui_base::theme::Theme::dark(),
    );
    assert_eq!(
        driver.frame_extent(viewport),
        (0.0, 400.0),
        "a window without a bar is all frame"
    );
    let unbarred = driver.targets()["more"];

    driver.present_in_window_bar();
    assert!(driver.presents_window_bar());
    let (top, height) = driver.frame_extent(viewport);
    assert!(top > 0.0, "the bar takes the window's top");
    assert!((top + height - 400.0).abs() < f64::EPSILON);
    let more = driver.targets()["more"];
    assert!(
        (more.y0 - unbarred.y0 - top).abs() < 0.5,
        "the app's frame starts under the bar: its button moved from {unbarred:?} to {more:?} \
         under a bar {top} tall"
    );
}

#[test]
fn an_agents_mark_rests_on_the_control_it_acts_on() {
    let mut column = Column::new();
    for id in ["first", "second"] {
        let mut resource = resource_actions::resource(id, "Same label", "edit", "Edit");
        resource.id = tilo_actions::ResourceId::new(id);
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
        column = column.push(resource_actions::immediate(
            Box::new(Button::new(id, "Same label").on_action(binding)),
            resource,
            |_| Ok(tilo_actions::ActionOutput::default()),
        ));
    }
    let mut driver = Driver::new(State::new(), Box::new(column), (800, 600));
    assert!(
        driver.drawn_action_markers().is_empty(),
        "no one acts yet, so no mark is drawn"
    );

    let event = tilo_actions::SemanticInteraction {
        version: tilo_actions::INTERACTION_VERSION,
        invocation: tilo_actions::InvocationId::new(),
        sequence: 1,
        actor: tilo_actions::Acting::agent("scribe", "portable-review"),
        phase: tilo_actions::InteractionPhase::Applying,
        action: tilo_actions::InteractionAction {
            id: "edit".into(),
            label: "Edit".into(),
            app: None,
            step: None,
            creates: None,
            did: None,
        },
        resource: tilo_actions::ActionResource {
            id: "first".into(),
            kind: "example".into(),
            label: "Same label".into(),
        },
        anchor: tilo_actions::InteractionAnchor::Resource,
        group: None,
    };
    let at = crate::time::now()
        .checked_sub(crate::time::Duration::from_secs(1))
        .unwrap();
    driver.update_presence(|presence| presence.apply_interaction_at(event, at).unwrap());
    assert!(driver.poll_frame().unwrap(), "the agent's step is drawn");

    let first = driver.targets()["first"];
    let second = driver.targets()["second"];
    let [marker] = driver.drawn_action_markers() else {
        panic!("one mark of the agent: {:?}", driver.drawn_action_markers());
    };
    let [x0, y0, x1, y1] = marker.locus;
    let locus = vello::kurbo::Rect::new(x0, y0, x1, y1);
    assert!(
        first.contains(locus.center()) && !second.contains(locus.center()),
        "the mark rests on the control the agent acts on: {locus:?}, not {second:?}"
    );
}

/// What someone else changed in a field stays only as the hover note saying who and what it held:
/// the write itself is drawn where it landed by presence, so the field keeps no colour of theirs
/// of its own.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_field_changed_by_someone_else_keeps_no_colour_of_theirs() {
    use crate::widget::{TextInput, TextInputProps};
    use tilo_ui_base::permissions::MemberId;
    use tilo_ui_base::{component::Bound, component::Described as _, component::Person};
    let field = |changed_by: Option<Person>, title: bool| {
        let props = TextInputProps {
            value: Bound {
                state: tilo_ui_base::LocalState::new("Second agent task, renamed".to_owned()),
                ..Bound::default()
            },
            changed_by,
            previous: Some("Second agent task".to_owned()),
            title,
            ..TextInputProps::default()
        };
        let mut driver = Driver::new_with_controlled_inputs(
            State::new(),
            Box::new(TextInput::from_props("field", props)),
            (400, 240),
            tilo_ui_base::Theme::dark(),
            tilo_ui_base::font_catalog::fixture_renderer(),
            MemberId::person("portable-review"),
        );
        driver.relayout();
        driver.render_rgba()
    };
    let agent = Person::new(MemberId::agent("scribe", "portable-review"), "Ada");
    for title in [false, true] {
        assert!(
            field(Some(agent.clone()), title) == field(None, title),
            "a changed field draws as an unchanged one (title: {title})"
        );
    }
}

// ---- every mutable entry point brings its change to a frame ----

struct QuietEntry {
    file: &'static str,
    entry: &'static str,
    why: &'static str,
}

/// Entry points that can change what a frame reads but run, or schedule, no frame themselves, each
/// with why it cannot leave a frame stale.
///
/// A new entry point that forgot would let a caller change something behind a frame's back and the
/// next [`Driver::poll_frame`] or [`Driver::relayout`] would show a stale window, so this fails
/// until it runs or schedules the frame that shows the change, or is listed here with a reason.
const QUIET_ENTRY_POINTS: &[QuietEntry] = &[
    QuietEntry {
        file: "app/driver.rs",
        entry: "action_registry_mut",
        why: "hands the app's registry out; a caller adds a decoder and then runs the frame that uses it.",
    },
    QuietEntry {
        file: "app/driver/jobs.rs",
        entry: "action_ui_routes",
        why: "reads the retained action geometry; executes, lays out and focuses nothing.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "app_mut",
        why: "hands the app out; the caller runs the frame that shows what it changed.",
    },
    QuietEntry {
        file: "app/driver/session.rs",
        entry: "attach_owner_wake_queue",
        why: "attaches the queue a later service reads; nothing a frame reads changes.",
    },
    QuietEntry {
        file: "app/driver/metrics.rs",
        entry: "begin_profiling",
        why: "starts timing later frames; nothing a frame reads changes.",
    },
    QuietEntry {
        file: "app/driver/metrics.rs",
        entry: "clear_profile_samples",
        why: "drains the profiler's samples; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "close_components",
        why: "closes the hosted components; the host runs the frame that shows their going.",
    },
    QuietEntry {
        file: "app/driver/jobs.rs",
        entry: "component_contract_violations",
        why: "reads the published contracts; executes nothing and changes nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "dispatch_to_root",
        why: "hands input over before the frame the caller asks for, as a window hands it over.",
    },
    QuietEntry {
        file: "app/driver/widget.rs",
        entry: "dispatch_to_root_with",
        why: "sets the modifiers around dispatch_to_root, which runs no frame; the caller asks for the next.",
    },
    QuietEntry {
        file: "app/driver/uicore.rs",
        entry: "drawn_placed_pointers",
        why: "reads the last frame's pointers; measures through the shaping cache.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "drive",
        why: "runs each step through perform, which runs the frame that shows it.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "dump_json",
        why: "serializes the document; changes nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/uicore.rs",
        entry: "end_profiling",
        why: "stops timing the frames; nothing a frame reads changes.",
    },
    QuietEntry {
        file: "app/driver/session.rs",
        entry: "lift_parts",
        why: "chooses how later frames lift their layers; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/pictures.rs",
        entry: "paint_pictures",
        why: "paints the composition's pictures into the page's records; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/uicore.rs",
        entry: "pending_history_control_token",
        why: "reads a History token; changes nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/jobs.rs",
        entry: "retain_invocation_results",
        why: "starts keeping later outcomes; nothing a frame reads changes.",
    },
    QuietEntry {
        file: "app/driver/workspace_side.rs",
        entry: "serve_provider",
        why: "registers what answers another provider's action; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "set_action_transport",
        why: "replaces the transport later calls are sent on; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/metrics.rs",
        entry: "set_coalesce_input_frames",
        why: "chooses when input's frame runs; the input still runs the frame it asks for.",
    },
    QuietEntry {
        file: "app/driver/uicore.rs",
        entry: "set_history_clock",
        why: "sets the clock History reads; nothing a frame reads now.",
    },
    QuietEntry {
        file: "app/driver/jobs.rs",
        entry: "set_invocation_result_bytes",
        why: "sets the retention bound a later invocation record hits; reads nothing.",
    },
    QuietEntry {
        file: "app/driver/session.rs",
        entry: "set_keyboard_held",
        why: "releases the scene caches of the caret; a caller redraws the frames it changed.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "set_permission_ctx",
        why: "marks the tree stale inside the app, which the next frame takes up.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "state_mut",
        why: "hands the document out; the caller runs the frame that shows the change it made.",
    },
    QuietEntry {
        file: "app/driver/uicore.rs",
        entry: "set_retained_geometry_check",
        why: "sets how often frames verify retained geometry; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/jobs.rs",
        entry: "start_bound_job_for",
        why: "hands a call to the window's broker, which the app runs in the frames the caller asks for.",
    },
    QuietEntry {
        file: "app/driver/capture.rs",
        entry: "start_bound_job_within",
        why: "as start_bound_job_for: the call it delivers runs in the frames the caller asks for.",
    },
    QuietEntry {
        file: "app/driver/workspace_side.rs",
        entry: "stop_serving_provider",
        why: "unregisters a provider answer; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/apps_a.rs",
        entry: "take_browser_font_demands",
        why: "drains the text renderer's fetch list; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/jobs.rs",
        entry: "take_collaboration_requests",
        why: "drains what the widgets asked the transport; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "take_component_close_reports",
        why: "drains the close reports; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "take_process_intent",
        why: "takes the app's one process intent; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/metrics.rs",
        entry: "take_profile_samples",
        why: "drains the profiler's samples; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/session.rs",
        entry: "take_skip_report",
        why: "hands over a log line; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/metrics.rs",
        entry: "take_text_shape_stats",
        why: "drains the shaping counters; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/window_bar.rs",
        entry: "take_window_actions",
        why: "takes what the window's controls asked; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver/session.rs",
        entry: "track_scene_parts",
        why: "chooses whether later frames record damage parts; nothing a frame reads.",
    },
    QuietEntry {
        file: "app/driver.rs",
        entry: "undo_status",
        why: "reads the focused document's undo status; changes nothing.",
    },
];

/// Every public `&mut self` entry point of [`Driver`], and every `pub(crate)` one, either runs the
/// frame that shows what it changed, schedules one, or is listed in [`QUIET_ENTRY_POINTS`] with why
/// it cannot leave a frame stale.
///
/// The scan reads the sources from disk, so an entry point in any file of the driver is covered
/// without being named here.
#[test]
fn every_mutable_entry_point_brings_its_change_to_a_frame() {
    for entry in QUIET_ENTRY_POINTS {
        assert!(
            entry.why.trim().len() >= 12,
            "{} {} has no reason a reviewer can check",
            entry.file,
            entry.entry
        );
    }
    let sources = driver_sources();
    // What a frame reads of what a mutable entry point changed, and the calls that schedule the
    // frame taking it up.
    const SETTLES: &[&str] = &[
        "self.run_frame()",
        "self.relayout()",
        "self.relayout_at(",
        "self.recompose()",
        "self.resize(",
        "self.send(",
        "self.idle()",
        "self.poll_frame()",
        "self.settle()",
        "self.app.needs_sync = true",
        "self.app.layout.layout_dirty = true",
        "self.app.layout.request_rebuild()",
        "self.surface.redraw.set(true)",
        "self.app.invalidate_widget_resource_cache()",
        "self.app.set_theme(",
        "self.app.present_history_in_header()",
        "self.refresh_model()",
        "self.idle_pass(",
        // What the peer carries is the frame that runs it ([`Driver::peer_delivering`]): a call
        // delivered to it, and the cancellation it takes up, leave work the next frame owes.
        "self.peer_mut().deliver(",
        "self.cancel_call(",
        // Admitting the kept call again runs the frame that shows it.
        "self.resolve_asked(",
    ];
    let mut checked = 0;
    let mut impls = 0;
    let mut missing = Vec::new();
    let mut used = std::collections::BTreeSet::new();
    let mut all = Vec::new();
    for (file, source) in &sources {
        for line in source.lines() {
            let code = line.trim_start();
            if let Some(rest) = code.strip_prefix("impl Driver") {
                let next = rest.chars().next();
                if next.is_none_or(|next| !next.is_alphanumeric() && next != '_') {
                    assert_eq!(
                        code, "impl Driver {",
                        "{file}: the scan reads `impl Driver` only written on one line ending in `{{`"
                    );
                }
            }
        }
        let lines: Vec<&str> = source.lines().collect();
        all.extend(methods(&lines, file, &mut impls));
    }
    for method in all.iter().filter(|method| method.entry) {
        checked += 1;
        if settles(&method.name, &all, SETTLES) {
            continue;
        }
        match QUIET_ENTRY_POINTS
            .iter()
            .position(|entry| entry.file == method.file && entry.entry == method.name.as_str())
        {
            Some(at) => {
                used.insert(at);
            }
            None => missing.push(format!(
                "{} {} neither runs nor schedules the frame that shows what it changed",
                method.file, method.name
            )),
        }
    }
    assert!(
        sources.len() > 10,
        "the scan found the driver's sources: {}",
        sources.len()
    );
    assert!(impls > 10, "the scan found the driver: {impls} impl blocks");
    assert!(checked > 100, "the scan found the entry points: {checked}");
    assert!(
        missing.is_empty(),
        "entry points that can change what a frame reads without bringing it: {missing:?}"
    );
    let stale: Vec<String> = QUIET_ENTRY_POINTS
        .iter()
        .enumerate()
        .filter(|(at, _)| !used.contains(at))
        .map(|(_, entry)| format!("{} {}", entry.file, entry.entry))
        .collect();
    assert!(
        stale.is_empty(),
        "quiet entry points that no longer need the exemption: {stale:?}"
    );
}

/// A method of the driver: the file it is written in, its name, its body and whether a caller
/// outside the driver reaches it with `&mut self`.
struct Method {
    file: String,
    name: String,
    body: String,
    entry: bool,
}

/// Whether `name` reaches a call that runs or schedules a frame, itself or through another method
/// of the driver it calls.
fn settles(name: &str, methods: &[Method], settles: &[&str]) -> bool {
    fn visit(
        name: &str,
        methods: &[Method],
        settles: &[&str],
        seen: &mut std::collections::BTreeSet<String>,
    ) -> bool {
        if !seen.insert(name.to_owned()) {
            return false;
        }
        let Some(method) = methods.iter().find(|method| method.name == name) else {
            return false;
        };
        if settles.iter().any(|call| method.body.contains(call)) {
            return true;
        }
        let body = method.body.as_str();
        let mut at = 0;
        while let Some(found) = body[at..].find("self.") {
            let start = at + found + "self.".len();
            let called: String = body[start..]
                .chars()
                .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                .collect();
            if body[start + called.len()..].starts_with('(')
                && visit(&called, methods, settles, seen)
            {
                return true;
            }
            at = start;
        }
        false
    }
    visit(
        name,
        methods,
        settles,
        &mut std::collections::BTreeSet::new(),
    )
}

/// Every method in the source's `impl Driver` blocks.
fn methods(lines: &[&str], file: &str, impls: &mut usize) -> Vec<Method> {
    let mut methods = Vec::new();
    let mut index = 0;
    let mut depth = 0;
    while index < lines.len() {
        let code = lines[index].trim_start();
        if depth == 0 && code == "impl Driver {" {
            *impls += 1;
            depth = brace_depth(code);
            index += 1;
            continue;
        }
        if depth == 0 {
            index += 1;
            continue;
        }
        if depth == 1
            && let Some((name, public)) = method_name(code)
        {
            let mut end = index;
            while !code_ends(lines[end]) {
                end += 1;
            }
            let signature = lines[index..=end].join(" ");
            if lines[end].trim_end().ends_with('{') {
                let mut body = String::new();
                let mut at = end + 1;
                let mut inner = brace_depth(lines[end]).max(1);
                while at < lines.len() && inner > 0 {
                    inner += brace_depth(lines[at]);
                    if inner > 0 {
                        body.push_str(lines[at]);
                        body.push('\n');
                    }
                    at += 1;
                }
                methods.push(Method {
                    file: file.to_owned(),
                    name,
                    body,
                    entry: public && signature.contains("&mut self"),
                });
            }
            depth += brace_depth(&signature);
            index = end + 1;
            assert!(depth >= 0, "the scan lost the brace depth in {file}");
            continue;
        }
        depth += brace_depth(lines[index]);
        index += 1;
        assert!(depth >= 0, "the scan lost the brace depth in {file}");
    }
    methods
}

/// The name `code` declares and whether a caller outside the module reaches it; a method whose
/// signature continues on the next line is not read, as the scan requires one name per line.
fn method_name(code: &str) -> Option<(String, bool)> {
    let (rest, public) = if let Some(rest) = code.strip_prefix("pub fn ") {
        (rest, true)
    } else if let Some(rest) = code.strip_prefix("pub(crate) fn ") {
        (rest, true)
    } else {
        (code.strip_prefix("fn ")?, false)
    };
    let name: String = rest
        .chars()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
        .collect();
    Some((name, public))
}

fn driver_sources() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app");
    let mut sources = Vec::new();
    let driver = root.join("driver.rs");
    sources.push((
        "app/driver.rs".to_owned(),
        std::fs::read_to_string(&driver).expect("the driver's sources are readable"),
    ));
    let mut entries: Vec<_> = std::fs::read_dir(root.join("driver"))
        .expect("the driver's source directory is readable")
        .map(|entry| entry.expect("a source directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.extension().is_some_and(|extension| extension == "rs") {
            let text = std::fs::read_to_string(&path).expect("a source file is readable");
            let name = path
                .file_name()
                .expect("a source file has a name")
                .to_string_lossy()
                .into_owned();
            // A test file is not an entry point a caller reaches through the driver.
            if name.ends_with("_tests.rs") {
                continue;
            }
            sources.push((format!("app/driver/{name}"), text));
        }
    }
    sources
}

/// Whether `line` closes what stands open in a method's signature: the body's opening brace, or
/// the `;` of a bodyless item.
fn code_ends(line: &str) -> bool {
    let code = line.trim_end();
    code.ends_with('{') || code.ends_with(';')
}

fn brace_depth(line: &str) -> i64 {
    let chars: Vec<char> = line.chars().collect();
    let mut depth = 0;
    let mut index = 0;
    let mut literal: Option<char> = None;
    while index < chars.len() {
        let character = chars[index];
        index += 1;
        if let Some(quote) = literal {
            if character == '\\' {
                index += 1;
            } else if character == quote {
                literal = None;
            }
            continue;
        }
        match character {
            '"' => literal = Some('"'),
            '\'' => {
                let escaped = chars.get(index) == Some(&'\\');
                let closes = chars.get(index + 1) == Some(&'\'');
                if !escaped && !closes {
                    continue;
                }
                literal = Some('\'');
            }
            '/' if chars.get(index) == Some(&'/') => break,
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}
