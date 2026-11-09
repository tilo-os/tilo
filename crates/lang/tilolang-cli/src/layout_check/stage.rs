//! An app's window as the UI host shows it, with nothing on screen: its instance over a workspace of
//! its own, staged with a scenario, sending its views over the patch protocol to the UI host's
//! half of the window (`tilo_ui_host::AppWindow`), which the toolkit's frame pipeline lays out
//! (`ui::headless::FrameHarness`). Nothing is rasterised and no GPU is opened.

use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value as Json;
use tilo_render_protocol::patch::local::pair;
use tilo_render_protocol::patch::{ResourceRef, ToApp, Value};
use tilo_ui_host::AppWindow;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::publish::{COMMIT, HOST_RESOURCE};
use tilolang_tilo::tilolang::Compiled;
use ui::headless::FrameHarness;
use ui::interaction::InteractionEnvironment;

/// The compiled packages an instance loads: the app and what it uses, the app last; and the
/// packages it uses as metadata: the native apps' it calls (`use so.tilo.session as session`),
/// whose actions run at their own doors, and the platform's (the device's, Settings') whose
/// published resources it reads.
pub struct Packages(
    pub Rc<Vec<Compiled>>,
    pub Vec<tilolang_tilo::tilolang::PackageMeta>,
);

impl Platform for Packages {
    fn load(&self, host: &Host) -> u64 {
        // The platform's resources stand as a device that has none of them, read as each
        // declares when nothing is published: a view over the sound or the battery lays out
        // as it does on a computer without them.
        for meta in &self.1 {
            if let Err(error) = host.runtime().load_foreign(meta.clone()) {
                eprintln!("tilolang layout-check: a platform package did not load: {error}");
            }
        }
        for (published, ty) in tilolang_tilo::platform_apps::platform_roots() {
            if !self
                .1
                .iter()
                .any(|meta| ty.starts_with(&format!("{}#", meta.app)))
            {
                continue;
            }
            match tilolang_tilo::runtime::ResourceRef::parse_id(published, ty) {
                Some(target) => {
                    if let Err(error) = host.runtime().set_reading(&target, "{}") {
                        eprintln!("tilolang layout-check: `{published}` reads nothing: {error}");
                    }
                }
                None => eprintln!("tilolang layout-check: `{published}` names no resource"),
            }
        }
        for compiled in self.0.iter() {
            if let Err(error) = host.load_compiled(compiled) {
                eprintln!("tilolang layout-check: a package did not load: {error}");
            }
        }
        0
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        let args: Vec<(&str, ui::i18n::ArgValue)> = args
            .iter()
            .map(|(name, value)| {
                // A number stays a number, so a plural rule selects by it.
                let value: ui::i18n::ArgValue = match value.parse::<f64>() {
                    Ok(number) => number.into(),
                    Err(_) => value.as_str().into(),
                };
                (name.as_str(), value)
            })
            .collect();
        tilo_ui_host::i18n::t_args(id, &args)
    }

    /// The package's words, and the one language its window is laid out in: the person's when
    /// the package ships it, the package's source language otherwise. A package the window uses
    /// (another app's view it calls) only rides along in that language.
    fn add_words(&self, window: bool, catalogues: &[(String, String)]) {
        if window {
            ui::i18n::speak_window_language(catalogues);
        } else {
            tilo_ui_host::i18n::add_shared_catalogues(catalogues);
        }
    }

    /// A fixed instant, so every run lays out the same dates: the clock the capture pins
    /// ([`ui::clock::freeze_for_capture`]), which the days the app reads and the days its
    /// window writes both come from.
    fn now(&self) -> String {
        ui::clock::local_now()
            .to_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// The person's zone, as a window knows it: a seed's wall-clock time and every day the
    /// window shows are read in it, so the layout is the one a person in their zone sees.
    fn zone(&self) -> i64 {
        i64::from(ui::clock::local_now().offset().local_minus_utc())
    }

    fn zone_name(&self) -> Option<String> {
        ui::time::local_zone_name()
    }

    /// A day or a time of day in a sentence, as the toolkit's lists show them, so a row's line
    /// and a notice carry no stored text.
    fn shown(&self, kind: &str, value: &str) -> String {
        tilo_ui_host::date::shown_text(kind, value)
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        Arc::new(|| {})
    }
}

/// One window of the app, laid out by one frame pipeline that every configuration reuses.
pub struct Staged {
    instance: Instance,
    harness: FrameHarness,
    /// What the window has cost so far, as [`Self::take_work`] reports it since the last call.
    work: Work,
}

/// What laying a window out cost, in the toolkit's own units rather than in time: frames laid
/// out, layout nodes built, and text shaped that the renderer's cache did not hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Work {
    /// Frames the toolkit laid out (a layout is one, plus one per change the app answered it
    /// with).
    pub frames: u64,
    /// Layout nodes built: every node of each tree a frame rebuilt.
    pub nodes: u64,
    /// Text shaped that the renderer's cache did not hold.
    pub shaped: u64,
}

impl Work {
    /// Add `other` to this.
    pub fn add(&mut self, other: Self) {
        self.frames = self.frames.saturating_add(other.frames);
        self.nodes = self.nodes.saturating_add(other.nodes);
        self.shaped = self.shaped.saturating_add(other.shaped);
    }
}

impl Staged {
    /// The app `app` of `packages` as the window's bar names it: its icon and its title, as the
    /// launcher and the desktop's bar wear them.
    fn name_window(harness: &mut FrameHarness, app: &str, packages: &Rc<Packages>) {
        let meta = packages.1.iter().find(|meta| meta.app == app);
        let title = meta.map_or_else(String::new, |meta| meta.title.clone());
        let icon = meta
            .and_then(|meta| meta.icon.as_deref())
            .and_then(|declared| ui::icons::Icon::from_name(&declared.replace('_', "-")))
            .unwrap_or(ui::icons::Icon::AppWindow);
        harness.set_window_app(ui::widget::AppKind::App(icon), title);
    }
}

/// How often the window and its instance may answer each other before a frame is taken as it
/// is: the rows a list shows, then the rows those rows show.
const EXCHANGES: usize = 8;

/// What a window is staged over and for, beyond its app, its packages and its size: the
/// workspace it opens over, the layer it shows, the scenario staged into it, and whether what
/// its units run is counted.
#[derive(Default)]
pub struct Staging {
    /// The scenario to stage; none stages none.
    pub scenario: Option<String>,
    /// The layer a package with more than one root shows (`Desk`, `Arrangement`).
    pub root: Option<String>,
    /// The workspace record it opens over; an empty one when none.
    pub record: Option<ui::automerge::AutoCommit>,
    /// Whether what the app's units run is counted
    /// ([`tilolang_tilo::runtime::cost`]).
    pub costs: bool,
    /// The view states the run stages, as the app's own code names them
    /// (`place=Task.today`), as `layout-check --state` takes them: which place a window
    /// stands on before anyone picks.
    pub states: Vec<String>,
}

impl Staged {
    /// The app `app` of `packages` with `scenario` staged, its window laid out at `viewport`,
    /// showing the layer `root` names rather than the package's first root: a package with more
    /// than one root (the shell's `Desk` beside its `Arrangement`) shows each, and each is laid
    /// out as a person sees it. `states` are the view states the run stages (`place=Task.today`),
    /// as `layout-check --state` takes them: which place a window stands on before anyone picks.
    pub fn over(
        app: &str,
        packages: &Rc<Packages>,
        scenario: &str,
        viewport: (u32, u32),
        theme: ui::Theme,
        root: Option<&str>,
        states: &[String],
    ) -> Result<Self, String> {
        Self::over_workspace(
            app,
            packages,
            viewport,
            theme,
            Staging {
                scenario: Some(scenario.to_owned()),
                root: root.map(str::to_owned),
                states: states.to_vec(),
                ..Staging::default()
            },
        )
    }

    /// As [`Self::over`], staged as `staging` says: its scenario, its layer, over the workspace
    /// it brings (a seeded one, as a person with ten thousand items has it), counting what the
    /// app's units run ([`tilolang_tilo::runtime::cost`]) when it says so.
    pub fn over_workspace(
        app: &str,
        packages: &Rc<Packages>,
        viewport: (u32, u32),
        mut theme: ui::Theme,
        staging: Staging,
    ) -> Result<Self, String> {
        // Layout at rest: what changes land at once rather than moving there.
        theme.reduce_motion = true;
        let (app_end, window_end) = pair();
        let me = "ada".to_owned();
        let names = Rc::new(tilolang_tilo::names::Told::new(
            &me,
            Rc::new(tilolang_tilo::names::Unnamed),
        ));
        names.tell("Ada".to_owned(), Vec::new());
        let mut instance = Instance::new(
            Config {
                app: app.to_owned(),
                me: me.clone(),
                mode: Mode::Live,
                device: None,
                index_dir: None,
                record_loader: None,
                root: tilolang_tilo::host::RootView {
                    root: staging.root.clone(),
                    view: None,
                },
                sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            },
            Rc::clone(packages) as Rc<dyn Platform>,
            // The person the layout opens as is called as a person is, `Ada`, and everyone else
            // as a window calls them when nobody told it their names: a scenario's sample person
            // (`person("Leo")`) by that name, never by their id.
            names,
            Box::new(app_end),
        );
        let mut record = staging.record.unwrap_or_default();

        instance.sync(Some(&record));
        // The window is laid out for a still picture: no frame comes to take in the indexes
        // its load builds off the frame, so they are waited for, as a host without frames
        // waits for them. Until then the window would show the platform's loading state.
        if let Some(host) = instance.host() {
            host.wait_index();
        }
        // Counted from the first frame: a workspace's opening is what it costs to open.
        if staging.costs
            && let Some(host) = instance.host()
        {
            host.runtime().record_costs(true);
        }
        // The package's own root resources — every layer it has (the shell's `Desk` beside its
        // arrangement) and a person's record kept beside them (`root shell: Shell`) — are made
        // and its migrations run, as the live session does when it loads the package: a
        // scenario that seeds another layer than the one laid out, or seeds a root through a
        // migration, shows them. Making a root queues a change, which `settle` writes down as
        // the door writes a session's.
        if let Some(host) = instance.host() {
            let meta = host.package(app).map(|package| package.meta);
            let roots = meta.iter().flat_map(|meta| {
                meta.roots
                    .iter()
                    .cloned()
                    .chain(meta.named_roots.iter().map(|root| root.ty.clone()))
            });
            for ty in roots {
                host.root_record(app, &ty)
                    .map_err(|error| format!("a root could not be made: {error}"))?;
            }
        }
        instance.set_origin(true);
        settle(&mut instance, &mut record, &me, None)?;
        if let Some(host) = instance.host() {
            for state in &staging.states {
                let (name, text) = state.split_once('=').ok_or_else(|| {
                    format!("--state {state} names no value: write `--state place=Task.today`")
                })?;
                host.runtime()
                    .stage_view_state(app, name.trim(), text.trim())
                    .map_err(|error| format!("--state {state}: {error}"))?;
            }
        }
        instance.stage(staging.scenario.as_deref());
        // A scenario's sample data is what it writes without a time of its own; what it dates
        // is the story it tells.
        let now = packages.now();
        settle(&mut instance, &mut record, &me, Some(&now))?;
        if let Some(why) = instance.take_stage_failure() {
            return Err(format!(
                "the scenario \"{}\" failed: {why}",
                staging.scenario.as_deref().unwrap_or_default()
            ));
        }
        instance.update();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        window.receive();
        if let Some(why) = window.failed() {
            return Err(format!("the window refused what the app sent: {why}"));
        }
        let harness =
            FrameHarness::new_with_theme(ui::State::new(), Box::new(window), viewport, theme);
        let mut staged = Self {
            instance,
            harness,
            work: Work::default(),
        };
        // The window a person sees: the desktop's bar over the app's frame, the app's slots in
        // it, and the app told that the bar hosts them, as the desktop tells it.
        Self::name_window(&mut staged.harness, app, packages);
        staged.harness.present_in_window_bar();
        staged.exchange();
        Ok(staged)
    }

    fn window(&mut self) -> Option<&mut AppWindow> {
        self.harness
            .root_widget_mut()
            .as_any_mut()?
            .downcast_mut::<AppWindow>()
    }

    /// Let the window and its instance answer each other until neither has more to say, laying
    /// the window out after each answer.
    fn exchange(&mut self) {
        for _ in 0..EXCHANGES {
            if let Some(window) = self.window() {
                window.flush();
            }
            let moved = self.instance.update();
            let received = self.window().is_some_and(AppWindow::receive);
            if !moved && !received {
                break;
            }
            self.harness.relayout();
        }
    }

    /// Lay the window out at `viewport` under `theme`, for a person using `input`. The window
    /// tells each view the space layout gave it, as the live window does.
    pub fn lay_out(
        &mut self,
        viewport: (u32, u32),
        mut theme: ui::Theme,
        input: InteractionEnvironment,
        display_scale: f64,
    ) -> Result<(), String> {
        theme.reduce_motion = true;
        self.harness.set_input_scale(display_scale)?;
        self.harness.set_interaction_environment(input);
        self.harness
            .relayout_window_themed(viewport, theme)
            .map_err(|error| format!("layout failed: {error:?}"))?;
        self.exchange();
        // A motion that lands even when reduced (a cross-fade) runs out on the toolkit's clock.
        self.settle()?;
        Ok(())
    }

    /// Press `key` (`Tab`, `Down`, `Space`: a key as [`FrameHarness::key`] names it) on the
    /// window, as a person at the keyboard does.
    pub fn key(&mut self, key: &str) {
        self.harness.key(key);
    }

    /// What the app's units ran since the last [`Self::reset_costs`], by unit
    /// ([`tilolang_tilo::runtime::cost`]), when the costs are being counted.
    pub fn costs(&self) -> Option<tilolang_tilo::runtime::Costs> {
        self.instance.host()?.runtime().costs()
    }

    /// Count from here, forgetting what was run before: one interaction's cost.
    pub fn reset_costs(&self) {
        if let Some(host) = self.instance.host() {
            host.runtime().reset_costs();
        }
    }

    pub fn harness(&self) -> &FrameHarness {
        &self.harness
    }

    /// What laying the window out has cost since the last call: frames laid out, layout nodes
    /// built, and text shaped that the renderer's cache did not hold.
    pub fn take_work(&mut self) -> Work {
        let frames = self.harness.relayout_frames();
        let nodes = self.harness.layout_nodes_built();
        let (_, misses, _) = self.harness.take_text_shape_stats();
        let delta = Work {
            frames: frames.saturating_sub(self.work.frames),
            nodes: nodes.saturating_sub(self.work.nodes),
            shaped: misses,
        };
        self.work.frames = frames;
        self.work.nodes = nodes;
        self.work.shaped = self.work.shaped.saturating_add(misses);
        delta
    }

    pub fn harness_mut(&mut self) -> &mut FrameHarness {
        &mut self.harness
    }

    /// The workspace's records and the items of their collections, as references.
    fn records(&self) -> Vec<ResourceRef> {
        let mut out = Vec::new();
        let Some(host) = self.instance.host() else {
            return out;
        };
        for (_, _, doc) in host.records() {
            let Some(ty) = host.store().resource_type(&doc) else {
                continue;
            };
            out.push(ResourceRef {
                id: doc.clone(),
                ty: ty.clone(),
            });
            // The record's type by the package versions this host loaded, not by whatever a
            // store elsewhere in this process registered last.
            let (Some(schema), Ok(record)) = host.with_schemas(|| {
                (
                    tilolang_tilo::schema::lookup(&ty),
                    host.store().record(&doc),
                )
            }) else {
                continue;
            };
            for field in &schema.fields {
                // Each item's type is its own: a collection of a union of record types (a
                // category, a channel or a direct message) declares none for the field, so the
                // item's record is the only thing that says which type it is.
                let Some(tilolang_tilo::DynValue::Collection(items)) = record.get(&field.id) else {
                    continue;
                };
                out.extend(items.iter().map(|(id, item)| ResourceRef {
                    id: format!("{doc}/{}", id.as_str()),
                    ty: item.schema().id.clone(),
                }));
            }
        }
        out
    }

    /// Every resource the workspace holds, the window's views and the rows of their lists name.
    pub fn resources(&mut self) -> Vec<ResourceRef> {
        fn in_value(value: &Value, out: &mut Vec<ResourceRef>) {
            match value {
                Value::Ref(target) => out.push(target.clone()),
                Value::List(items) => items.iter().for_each(|item| in_value(item, out)),
                Value::Opt(Some(item)) => in_value(item, out),
                Value::Map(entries) => entries.iter().for_each(|(k, v)| {
                    in_value(k, out);
                    in_value(v, out);
                }),
                _ => {}
            }
        }
        fn in_node(node: &tilo_ui_host::UiNode, out: &mut Vec<ResourceRef>) {
            node.props.values().for_each(|value| in_value(value, out));
            node.children.iter().for_each(|child| in_node(child, out));
        }
        let mut out = self.records();
        let Some(window) = self.window() else {
            return out;
        };
        for tree in [window.root_tree(), window.detail_tree()]
            .into_iter()
            .flatten()
        {
            in_node(&tree, &mut out);
        }
        for list in window.lists() {
            if let Some(rows) = window.rows_of(&list) {
                let rows = rows.borrow();
                for key in rows.rendered() {
                    if let Some(row) = rows.row(&key) {
                        in_node(row, &mut out);
                    }
                }
            }
        }
        out
    }

    /// Tap the window at `at`, as a finger does, and let it answer: a row of a list is painted
    /// by the list, one widget without a place of its own to tap by name, so a person's hand
    /// lands on where the row is drawn.
    pub fn tap_at(&mut self, at: (f64, f64)) -> Result<(), String> {
        self.harness.tap_at(at.0, at.1);
        self.exchange();
        self.settle()
    }

    /// The resource the window shows where a person holding it is looking: nothing while the
    /// sidebar stands in front, and the page the window pushed in front of its list (or opened
    /// over it) once that does.
    pub fn front_target(&mut self) -> Option<ResourceRef> {
        let window = self.window()?;
        let sidebar = matches!(
            window.shell_ref().map(ui::widget::NavShell::front_pane),
            Some(ui::widget::NavPane::Sidebar | ui::widget::NavPane::Overview)
        );
        if sidebar {
            return None;
        }
        window
            .detail_target()
            .or_else(|| window.root_target())
            .cloned()
    }

    /// The tree the window composed from what the app sent, as sent: every view that ran is in
    /// it, keyed by its view, even one that drew nothing (a bare `if let` whose value is
    /// absent leaves an empty fragment), where the laid-out widgets keep no trace of it.
    pub fn composed(&mut self) -> Option<tilo_ui_host::UiNode> {
        self.window()?.composed().cloned()
    }

    /// The trees of the rows the window's lists show, by row key: a list draws each row as one
    /// widget keyed by the row, from the row view's nodes.
    pub fn rows(&mut self) -> std::collections::HashMap<String, tilo_ui_host::UiNode> {
        let mut out = std::collections::HashMap::new();
        let Some(window) = self.window() else {
            return out;
        };
        for list in window.lists() {
            if let Some(rows) = window.rows_of(&list) {
                let rows = rows.borrow();
                for key in rows.rendered() {
                    if let Some(row) = rows.row(&key) {
                        out.insert(key, row.clone());
                    }
                }
            }
        }
        out
    }

    /// Open `resource` in the window, as a person opening it from a list does.
    pub fn open(&mut self, resource: ResourceRef) {
        if let Some(window) = self.window() {
            window.tell(ToApp::Open { resource });
        }
        self.exchange();
        self.harness.relayout();
    }

    /// Lay the window out at `viewport` under `theme` for a person using `input`, with motion
    /// as it is live: what a person's press then sets moving moves ([`Self::press`]).
    pub fn lay_out_moving(
        &mut self,
        viewport: (u32, u32),
        mut theme: ui::Theme,
        input: InteractionEnvironment,
    ) -> Result<(), String> {
        theme.reduce_motion = false;
        self.harness.set_interaction_environment(input);
        self.harness
            .relayout_window_themed(viewport, theme)
            .map_err(|error| format!("layout failed: {error:?}"))?;
        self.exchange();
        self.settle()
    }

    /// Press the control `id` of the window as a person does, with a finger on a touchscreen
    /// (`touch`) and the mouse otherwise (a click is a mouse's: the window then sizes itself
    /// for one), let the window and its instance answer it, and let the motion it starts run
    /// out: the frames a live window draws, and no layout afresh after them.
    pub fn press(&mut self, id: &str, touch: bool) -> Result<(), String> {
        let pressed = if touch {
            self.harness.tap_named(id)
        } else {
            self.harness.click_named(id)
        };
        pressed.map_err(|error| format!("pressing {id:?}: {error}"))?;
        self.exchange();
        self.settle()
    }

    /// Press one of the window's bar slots by its id, as the desktop hands the press over: the
    /// shell's own Back, an action the app's page declared. The bar is the window's, so the press
    /// reaches the app through it, never through a widget of the frame.
    pub fn press_bar_slot(&mut self, id: &str) -> Result<(), String> {
        self.harness
            .press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: id.into() });
        self.exchange();
        self.settle()
    }

    /// Press `x`, `y` as a person presses them, and let the window and its instance answer: what
    /// reaches a row a menu paints, which has no node of its own to name.
    pub fn press_at(&mut self, x: f64, y: f64) -> Result<(), String> {
        // A row is pressed the way the window expects its person to: a finger taps it, where a
        // mouse press on a menu a finger opened only dismisses the menu.
        if self.harness.interaction_environment().modality == ui::interaction::InputModality::Touch
        {
            self.harness.tap_at(x, y);
        } else {
            self.harness.click_at(x, y);
        }
        self.exchange();
        self.settle()
    }

    /// Lay the window out as it stands now, so that what a press opened over it — a menu, a
    /// popover, a sheet — is there to be read and pressed in turn.
    pub fn freshen(&mut self) -> Result<(), String> {
        self.exchange();
        self.harness.relayout();
        self.settle()
    }

    /// Press the action the control `id` publishes as a semantic child, as assistive technology
    /// activates it: a control a widget paints itself (an empty state's action) has no node of
    /// its own to tap, and the widget's own node is not where the action is.
    pub fn press_action(&mut self, id: &str) -> Result<(), String> {
        self.harness
            .perform_named(id, &ui::a11y::Action::Activate)
            .map_err(|error| format!("pressing {id:?}: {error}"))?;
        self.exchange();
        self.settle()
    }

    /// Run the toolkit's clock until nothing in the window moves.
    ///
    /// The wall clock is the capture's (pinned by [`ui::clock::freeze_for_capture`]), and a
    /// widget that paints by it — an indeterminate progress bar, a spinner — holds still and
    /// asks for no frame, so a looping indicator settles here. Anything else still asking for a
    /// frame after two seconds is an animation that never ends, which no screen should have.
    fn settle(&mut self) -> Result<(), String> {
        // A widget that took a layout switch after its parts were placed asks for the frame
        // that lays them out again, as a live window draws it.
        if self.harness.root_widget_mut().is_layout_dirty() {
            self.harness.relayout();
        }
        if self.harness.frame_schedule().needs_animation_frame
            && !self
                .harness
                .settle_for_capture(ui::time::Duration::from_secs(2))
        {
            return Err(format!(
                "the window still moves after two seconds: {:?}",
                self.harness.frame_requesters()
            ));
        }
        Ok(())
    }

    /// Show `resource` with the view `view` (an id) in the window's detail lane: a view the
    /// system shows on a surface of its own (a row view as a search result, a proposal view in
    /// its card), laid out as a window is.
    pub fn open_with(&mut self, resource: &ResourceRef, view: &str) {
        self.instance.open_with(resource, view);
        self.exchange();
        self.harness.relayout();
    }

    /// Why the window shows the platform's error state, if it does.
    pub fn failed(&mut self) -> Option<String> {
        self.window()
            .and_then(|window| window.failed().map(str::to_owned))
    }

    /// Each component of the window's view that did not build or take its props, and why.
    pub fn broken_components(&mut self) -> Vec<String> {
        self.window()
            .map(|window| window.broken_components())
            .unwrap_or_default()
    }
}

/// The moment a second before `at` (an RFC 3339 time), written as the record writes one.
fn second_before(at: &str) -> String {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(at) else {
        return at.to_owned();
    };
    let Some(earlier) = parsed.checked_sub_signed(chrono::Duration::seconds(1)) else {
        return at.to_owned();
    };
    earlier
        .to_utc()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The capture has no door over the workspace, so it stands where the door stands: it takes
/// each step the instance hands over, writes it down and answers it, as a session's door does.
/// The store reads the record's changes back, so what it tells of a change — the versions of a
/// resource, and the marks and history drawn from them — is a session's, never a workspace
/// staged but never written down.
///
/// `sample_at` is the moment a scenario's undated steps carry (the capture's clock), where one
/// ran: the steps are all read before any is written, so each undated one lands a second before
/// the first dated one. A change made on top of another is never dated before it, and the marks
/// and the history drawn from the record stay in the order the scenario wrote it.
fn settle(
    instance: &mut Instance,
    record: &mut ui::automerge::AutoCommit,
    person: &str,
    sample_at: Option<&str>,
) -> Result<(), String> {
    instance.sync(Some(record));
    instance.commit_planned();
    let Some(host) = instance.host() else {
        return Ok(());
    };
    let mut steps = Vec::new();
    while let Some((invocation, _)) = instance.take_invocation() {
        let address = invocation.address.clone();
        if address.resource == HOST_RESOURCE && address.action == COMMIT {
            let ticket = invocation.arguments.get("ticket").and_then(Json::as_u64);
            if let Some(action) = ticket.and_then(|ticket| host.store().committed(ticket)) {
                steps.push((address, action));
                continue;
            }
        }
        instance.answered(&address, None);
    }
    let earliest = sample_at.and_then(|sample_at| {
        steps
            .iter()
            .filter_map(|(_, action)| action.authorship.at.as_deref())
            .filter(|at| *at < sample_at)
            .min()
            .map(second_before)
    });
    for (address, mut action) in steps {
        if let Some(sample_at) = sample_at
            && action.authorship.at.as_deref() == Some(sample_at)
            && let Some(earliest) = earliest.as_deref()
        {
            action.authorship.at = Some(earliest.to_owned());
        }
        commit(&host, person, record, &action)?;
        instance.answered(&address, None);
    }
    instance.sync(Some(record));
    Ok(())
}

/// Apply `action` to `record` as the door applies one for a session: encoded, as the person's
/// step, with the context a step on the record carries, so what the store tells of the change —
/// who made it, when, and the version it is of each resource — reads as a session's.
fn commit(
    host: &Host,
    person: &str,
    record: &mut ui::automerge::AutoCommit,
    action: &tilolang_tilo::action::DynAction,
) -> Result<(), String> {
    use ui::tilo_actions::{ActionContext, ActionInvoker, apply_local_with_context};
    // A resource its origin derives — the shell's desk and arrangement, a session's desk —
    // lives in the store's unsaved layer and is never the workspace record's to have, so its
    // steps stay there: what the store holds of it, the window is laid out with. A step that
    // touches one and the record both writes its record part, its author's and its moment's,
    // and leaves the derived part as the store wrote it.
    let Some(action) = only_the_record(action, host) else {
        return Ok(());
    };
    // The step is its author's, the person the scenario ran it as, and the change commits
    // under their actor: a gesture's continuation is written as the actor's, so a step of
    // another person's committed under this one would be read as a continuation of their
    // gesture, and its versions, which marks and history draw, would not be written down.
    let author = if action.authorship.invoker.is_empty() {
        person.to_owned()
    } else {
        action.authorship.invoker.clone()
    };
    let invoker = ActionInvoker::actor(author.clone());
    let context = ActionContext::for_resource(
        action.resources().first().copied().unwrap_or("workspace"),
        &action.kind,
        &action.kind,
    )
    .with_invoker(invoker);
    let encoded = action.encode().map_err(|error| error.to_string())?;
    let actor = author.as_bytes().to_vec();
    host.with_schemas(|| apply_local_with_context(record, &action, encoded, Some(&actor), context))
        .map_err(|error| error.to_string())?;
    record.commit();
    Ok(())
}

/// The step `action` as the workspace record takes it: its edits of the resources the record
/// holds, `None` when it has none. What its origin derives is the store's unsaved layer, and
/// the record never holds it.
fn only_the_record(
    action: &tilolang_tilo::action::DynAction,
    host: &Host,
) -> Option<tilolang_tilo::action::DynAction> {
    let edits: Vec<_> = action
        .edits
        .iter()
        .filter(|edit| !host.store().derives(resource_of(edit)))
        .cloned()
        .collect();
    if edits.is_empty() {
        return None;
    }
    let mut kept = action.clone();
    kept.edits = edits;
    Some(kept)
}

/// The resource `edit` writes.
fn resource_of(edit: &tilolang_tilo::action::DynEdit) -> &str {
    use tilolang_tilo::action::DynEdit;
    match edit {
        DynEdit::Create { id, .. }
        | DynEdit::Edit { id, .. }
        | DynEdit::Insert { id, .. }
        | DynEdit::Place { id, .. }
        | DynEdit::Remove { id }
        | DynEdit::OutboxPush { id, .. }
        | DynEdit::OutboxSettle { id, .. }
        | DynEdit::OutboxMark { id, .. }
        | DynEdit::Migrated { id, .. }
        | DynEdit::TriggerRan { id, .. }
        | DynEdit::RemoveItem { id, .. } => id,
        // A run of a collection counts against the record it is a field of, which names no
        // record of its own.
        DynEdit::RunChunk { holder, .. } => holder,
        DynEdit::In { edit, .. } | DynEdit::Chunk { edit, .. } => resource_of(edit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tilo_render_protocol::patch::{ActionCatalogue, ToUi};

    /// The packages of the app `name` of the apps the platform ships (`apps/tl/<name>`) and what
    /// it uses.
    fn app_packages(name: &str) -> Rc<Packages> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps/tl")
            .join(name);
        let search = vec![dir.parent().expect("apps/tl").to_path_buf()];
        let (built, platform) =
            crate::layout_check::build(&dir, &search).expect("the app compiles");
        Rc::new(Packages(
            Rc::new(built.into_iter().map(|b| b.compiled).collect()),
            platform,
        ))
    }

    /// The actions the app publishes at its window, offered the way its host offers them: the
    /// window's palette, action directory and agents read them off it from then on.
    fn publish_actions(staged: &mut Staged) -> Vec<tilo_actions::ActionDescriptor> {
        let host = staged.instance.host().expect("a host");
        let planner: Arc<dyn tilolang_tilo::publish::Planner> = Arc::new(host.handle());
        let mut publications = tilolang_tilo::publish::Publications::new(&host);
        // Records are published whole off the frame: a window with no frames waits for them.
        publications.settle(&host, &planner);
        let resources: Vec<tilo_actions::ResourceSpec> = publications
            .trees()
            .values()
            .map(|tree| (**tree).clone())
            .collect();
        let descriptors = tilo_actions::door::survey(&resources);
        let encoded = serde_json::to_vec(&descriptors).expect("the action directory encodes");
        staged
            .instance
            .say(ToUi::Actions(ActionCatalogue::All(encoded)));
        staged.instance.flush();
        staged.exchange();
        staged.harness.relayout();
        descriptors
    }

    /// Where the captures of a frame go: the target directory's own, as the toolkit's marker
    /// captures do.
    fn marker_shots() -> std::path::PathBuf {
        match std::env::var_os("CARGO_TARGET_DIR") {
            Some(dir) => std::path::PathBuf::from(dir).join("tilolang-marker"),
            None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/tilolang-marker"),
        }
    }

    /// The check box the frame binds to a published action: the one control of `descriptor`'s
    /// exactly, holding a check box where a person would press.
    fn bound_check(
        staged: &Staged,
        descriptors: &[tilo_actions::ActionDescriptor],
    ) -> Option<(
        tilo_actions::ActionDescriptor,
        String,
        ui::kurbo::Rect,
        ui::kurbo::Rect,
    )> {
        let controls = staged.harness().controls();
        for descriptor in descriptors {
            let routes = staged.harness().action_ui_routes(&descriptor.address);
            let [route] = routes.as_slice() else {
                continue;
            };
            let Some(control) = route.bounds else {
                continue;
            };
            let Some(check) = controls
                .iter()
                .filter(|shown| shown.role == ui::accesskit::Role::CheckBox)
                .filter_map(|shown| shown.bounds)
                .find(|bounds| control.contains(bounds.center()))
            else {
                continue;
            };
            return Some((descriptor.clone(), route.control_id.clone(), control, check));
        }
        None
    }

    /// The agent's interaction with `descriptor`, as an agent states it: one action, applying.
    fn interaction(
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> tilo_actions::SemanticInteraction {
        tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: tilo_actions::InvocationId::new(),
            sequence: 1,
            actor: tilo_actions::InteractionActor {
                id: "scribe".into(),
                kind: tilo_actions::ActorKind::Agent,
                for_person: None,
            },
            phase: tilo_actions::InteractionPhase::Applying,
            action: tilo_actions::InteractionAction {
                id: descriptor.address.action.clone(),
                label: descriptor.action_label.clone(),
                app: None,
            },
            resource: tilo_actions::ActionResource {
                id: descriptor.address.resource.clone(),
                kind: descriptor.resource_kind.clone(),
                label: descriptor.resource_label.clone(),
            },
            anchor: tilo_actions::InteractionAnchor::Resource,
            group: None,
        }
    }

    /// The agent's mark on the control it acts through, captured: `<name>-before.png` is the
    /// frame before, `<name>-agent-marker.png` the one once the mark has come to rest, and the
    /// pixels of both come back for the test to hold the drawing to.
    fn capture_agent(
        staged: &mut Staged,
        event: &tilo_actions::SemanticInteraction,
        name: &str,
    ) -> (Vec<u8>, Vec<u8>) {
        let captures = marker_shots();
        std::fs::create_dir_all(&captures).expect("the captures' directory");
        let before = staged.harness().render_rgba();
        staged
            .harness()
            .render_png(&captures.join(format!("{name}-before.png")));
        let applied = staged.harness_mut().update_presence(|presence| {
            presence
                .names
                .insert(ui::permissions::UserId("scribe".into()), "Ada".into());
            presence
                .apply_interaction(event.clone())
                .expect("a well-formed interaction applies")
        });
        assert!(applied, "the agent's action shows");
        // The frames the pointer glides in, run out: the capture shows it at rest on the
        // control rather than mid-glide. Settling re-renders the whole window before it can
        // say the window is still, and that pass takes about a second here and longer on a
        // loaded machine, so the bound is generous on purpose: it fails an animation that
        // never ends, not a window that is slow to draw.
        assert!(
            staged
                .harness_mut()
                .settle_for_capture(ui::time::Duration::from_secs(30)),
            "the marker comes to rest"
        );
        staged.harness_mut().poll_frame().expect("a frame is drawn");
        staged.harness_mut().relayout();
        staged
            .harness()
            .render_png(&captures.join(format!("{name}-agent-marker.png")));
        (before, staged.harness().render_rgba())
    }

    /// The bar the desktop draws over the app's frame keeps one "…" and shows each control
    /// once: an app's own door to what does not fit is the bar's door, not a second one beside
    /// it.
    #[test]
    fn the_window_bar_shows_each_control_once() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let (width, height) = (960, 1080);
        let mut staged = Staged::over(
            &app,
            &packages,
            "a busy week",
            (width, height),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged
            .lay_out(
                (width, height),
                ui::Theme::light(),
                ui::interaction::InteractionEnvironment::POINTER,
                1.0,
            )
            .expect("the window lies out");
        let bar = staged.harness_mut().window_bar_layout(f64::from(width));
        let named = |bar: &ui::window_toolbar::BarLayout| -> String {
            bar.controls
                .iter()
                .map(|control| format!("{}:{}", control.slot, control.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let doors = bar
            .controls
            .iter()
            .filter(|control| {
                control.icon.as_deref() == Some(ui::icons::Icon::MoreHorizontal.name())
            })
            .count();
        assert_eq!(doors, 1, "one bar keeps one \"…\": {}", named(&bar));
        let problems = crate::layout_check::audit::bar(&bar);
        assert!(
            problems.is_empty(),
            "the bar shows each control once: {:?} ({})",
            problems.iter().map(|p| p.kind).collect::<Vec<_>>(),
            named(&bar),
        );
    }

    /// The titles of the notes the list shows, in the order it shows them.
    fn listed_titles(staged: &Staged) -> Vec<String> {
        fn walk(node: &ui::headless::InspectionNode, out: &mut Vec<String>) {
            let is_row = node
                .semantics
                .as_ref()
                .is_some_and(|semantics| semantics.role == ui::a11y::Role::ListItem);
            if is_row && let Some(name) = node.semantics.as_ref().and_then(|s| s.name.clone()) {
                out.push(name);
                return;
            }
            for child in &node.children {
                walk(child, out);
            }
        }
        let root = staged.harness().inspection().root;
        let mut out = Vec::new();
        walk(&root, &mut out);
        out
    }

    /// A menu the bar lists runs the row the person chooses there: the notes window's sort
    /// menu stands in the bar with its orders, and choosing "Title" in it lists the notes by
    /// title.
    #[test]
    fn a_menu_row_the_bar_lists_runs_where_it_stands() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("notes");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let (width, height) = (960, 1080);
        let mut staged = Staged::over(
            &app,
            &packages,
            "a week of notes",
            (width, height),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged
            .lay_out(
                (width, height),
                ui::Theme::light(),
                ui::interaction::InteractionEnvironment::POINTER,
                1.0,
            )
            .expect("the window lies out");
        let before = listed_titles(&staged);
        assert_eq!(
            before.first().map(String::as_str),
            Some("Q3 plan"),
            "the notes list in the order they were made: {before:?}"
        );
        let bar = staged.harness_mut().window_bar_layout(f64::from(width));
        assert!(
            bar.controls.iter().any(|control| control.name == "Sort"),
            "the bar lists the sort menu: {:?}",
            bar.controls
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
        );
        let toolbar = ui::window_toolbar::declared().unwrap_or_default();
        let row = toolbar
            .leading
            .iter()
            .chain(&toolbar.actions)
            .flat_map(|action| &action.items)
            .find(|row| row.name == "Title")
            .expect("the bar lists the sort menu's rows");
        staged
            .press_bar_slot(&row.id)
            .expect("the row's press runs");
        let after = listed_titles(&staged);
        let mut by_title = before;
        by_title.sort();
        assert_eq!(
            after, by_title,
            "choosing Title in the bar lists them by title"
        );
    }

    /// The agent's mark on the control the tilolang Tasks app binds: an action the app
    /// published lands on the control that runs it and nowhere else, and the frame draws the
    /// agent there — the same pixels rule the app's own tests hold a marker to.
    #[test]
    fn an_agents_marker_rests_on_the_control_the_tasks_app_binds() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let (width, height) = (1280, 800);
        let mut staged = Staged::over(
            &app,
            &packages,
            "a busy week",
            (width, height),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged.harness.relayout();
        let descriptors = publish_actions(&mut staged);
        assert!(
            !descriptors.is_empty(),
            "the app publishes the actions its views run"
        );

        let (descriptor, control_id, control, check) =
            bound_check(&staged, &descriptors).expect("the app binds a check box to an action");
        let event = interaction(&descriptor);
        let locus = staged
            .harness()
            .semantic_locus(&event)
            .expect("the control the app binds the action to");
        assert!(
            locus.contains(check.center()),
            "the locus is the check box the row offers: {locus:?} vs {check:?}"
        );
        assert!(
            control.contains(locus.center()),
            "the locus is the control bound to the action: {locus:?} vs {control:?}"
        );

        let (before, after) = capture_agent(&mut staged, &event, "tasks-tilolang");

        marked_only(&staged, &before, &after, width, control, check);

        // The press and the agent's action take the one path: the app is handed the action,
        // not a view event.
        staged
            .press_action(&control_id)
            .expect("the control presses");
        let chosen = staged.instance.take_chosen();
        assert_eq!(
            chosen.as_ref().map(|(_press, address, _argument)| address),
            Some(&descriptor.address),
            "pressing the control runs the action the app published"
        );
        assert!(
            staged.instance.take_chosen().is_none(),
            "one press runs the action once"
        );
    }

    /// The agent's mark shows inside the check box the app binds, and on no other check box the
    /// window shows: the agent is drawn where it acts, not where any control of the same kind
    /// stands.
    fn marked_only(
        staged: &Staged,
        before: &[u8],
        after: &[u8],
        width: u32,
        control: ui::kurbo::Rect,
        check: ui::kurbo::Rect,
    ) {
        changed_pixels(before, after, width, pixels(check))
            .expect("the agent's marker rests on the check box the app binds");
        let others: Vec<_> = staged
            .harness()
            .controls()
            .iter()
            .filter(|shown| shown.role == ui::accesskit::Role::CheckBox)
            .filter_map(|shown| shown.bounds)
            .filter(|bounds| !control.contains(bounds.center()))
            .collect();
        assert!(!others.is_empty(), "the scenario shows more than one row");
        for other in others {
            assert!(
                changed_pixels(before, after, width, pixels(other)).is_none(),
                "no mark rests on {other:?}, which the action does not act on"
            );
        }
    }

    /// `rect` as the pixels a frame draws it in: the whole pixels its edges cut.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a laid out edge inside a viewport's pixels"
    )]
    fn pixels(rect: ui::kurbo::Rect) -> (u32, u32, u32, u32) {
        (
            rect.x0.floor().max(0.0) as u32,
            rect.y0.floor().max(0.0) as u32,
            rect.x1.ceil().max(0.0) as u32,
            rect.y1.ceil().max(0.0) as u32,
        )
    }

    /// The bounds of the pixels that changed between `before` and `after`, within `region`.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "a pixel index and its neighbours inside a frame's own dimensions"
    )]
    fn changed_pixels(
        before: &[u8],
        after: &[u8],
        width: u32,
        region: (u32, u32, u32, u32),
    ) -> Option<(u32, u32, u32, u32)> {
        let mut bbox: Option<(u32, u32, u32, u32)> = None;
        for (index, (a, b)) in before
            .chunks_exact(4)
            .zip(after.chunks_exact(4))
            .enumerate()
        {
            if a == b {
                continue;
            }
            let index = u32::try_from(index).expect("a pixel index fits u32");
            let (x, y) = (index % width, index / width);
            if x < region.0 || y < region.1 || x >= region.2 || y >= region.3 {
                continue;
            }
            bbox = Some(match bbox {
                None => (x, y, x + 1, y + 1),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
            });
        }
        bbox
    }

    /// The control the window binds to a published action: its one route, the control the action
    /// lands on, and the check box a person sees inside it.
    fn bound_box(
        staged: &Staged,
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> Option<(String, ui::kurbo::Rect, ui::kurbo::Rect)> {
        let routes = staged.harness().action_ui_routes(&descriptor.address);
        let [route] = routes.as_slice() else {
            return None;
        };
        let control = route.bounds?;
        let check = staged
            .harness()
            .controls()
            .iter()
            .filter(|shown| shown.role == ui::accesskit::Role::CheckBox)
            .filter_map(|shown| shown.bounds)
            .find(|bounds| control.contains(bounds.center()))?;
        Some((route.control_id.clone(), control, check))
    }

    /// The button the counter's window binds to its action runs that action through the door,
    /// once, as the person's own: the press is the action's route, not a view event the app runs
    /// in the window instead.
    #[test]
    fn a_press_on_the_button_an_app_binds_runs_its_action_once_through_the_door() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("counter");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let (width, height) = (900, 600);
        let mut staged = Staged::over(
            &app,
            &packages,
            "a busy morning",
            (width, height),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged.harness.relayout();
        let descriptors = publish_actions(&mut staged);
        let increment = descriptors
            .iter()
            .find(|descriptor| descriptor.action_label == "Increment")
            .expect("the counter publishes the action its button runs");
        let routes = staged.harness().action_ui_routes(&increment.address);
        let [route] = routes.as_slice() else {
            panic!(
                "the window binds one button to Increment, as it binds one to every action it \
                 offers a control for: {routes:?}"
            );
        };
        let control = route.bounds.expect("the button is laid out");
        let event = interaction(increment);
        let locus = staged
            .harness()
            .semantic_locus(&event)
            .expect("the button the app binds the action to");
        assert!(
            control.contains(locus.center()),
            "the agent acts at the button: {locus:?} vs {control:?}"
        );
        let (before, after) = capture_agent(&mut staged, &event, "counter-tilolang");
        assert!(
            changed_pixels(&before, &after, width, pixels(control)).is_some(),
            "the agent's marker rests on the button the app binds"
        );

        staged
            .press(&route.control_id, false)
            .expect("the button presses");
        let chosen = staged.instance.take_chosen();
        assert_eq!(
            chosen.as_ref().map(|(_press, address, _argument)| address),
            Some(&increment.address),
            "one press runs the action once"
        );
        assert!(
            staged.instance.take_chosen().is_none(),
            "the press runs one action, not two"
        );
    }

    /// The window says it stands on `label`, a form the press opened.
    fn shows_dialog(node: &ui::headless::InspectionNode, label: &str) -> bool {
        fn within(access: &ui::a11y::AccessNode, label: &str) -> bool {
            if access.role == ui::a11y::Role::Dialog && access.name.as_deref() == Some(label) {
                return true;
            }
            access.children.iter().any(|child| within(child, label))
        }
        node.semantics
            .as_ref()
            .is_some_and(|access| within(access, label))
            || node.children.iter().any(|child| shows_dialog(child, label))
    }

    /// How many of the tasks of `app` the window's workspace reminds anyone of.
    fn reminded(staged: &Staged, app: &str) -> usize {
        let host = staged.instance.host().expect("a host");
        let root = host.root_of(app).expect("the tasks root");
        let tilolang_tilo::runtime::Value::List(items) =
            host.runtime().read(&root, "items").expect("the items")
        else {
            panic!("the tasks are a list");
        };
        items
            .into_iter()
            .filter(|item| {
                let tilolang_tilo::runtime::Value::Ref(task) = item else {
                    return false;
                };
                matches!(
                    host.runtime().read(&task.target, "remind"),
                    Ok(tilolang_tilo::runtime::Value::Opt(Some(_)))
                )
            })
            .count()
    }

    /// The reminder's form, opened from a task's row menu as a finger opens it: the menu's
    /// "Remind me…" row asks when.
    fn open_reminder_form(staged: &mut Staged, packages: &Rc<Packages>) {
        // The task stands open as its page: the reminder's menu is part of the document, and a
        // page takes the presses the controls of a row's document do not (yet) reach.
        let meta = &packages.0.last().expect("the app's meta").meta;
        let task_type = &meta
            .types
            .iter()
            .find(|ty| ty.name == "Task")
            .expect("a task type")
            .id;
        let page_view = &meta
            .views
            .iter()
            .find(|view| view.name == "task_page")
            .expect("a task page")
            .id;
        let task = staged
            .resources()
            .into_iter()
            .find(|resource| &resource.ty == task_type)
            .expect("the scenario holds a task");
        staged.open_with(&task, page_view);
        let reminder = {
            let root = &staged.harness().inspection().root;
            crate::layout_check::button_named(root, "Remind").expect("the task offers its reminder")
        };
        staged.press(&reminder, true).expect("its menu opens");
        staged.freshen().expect("the menu comes to rest");
        let row = {
            let root = &staged.harness().inspection().root;
            crate::layout_check::painted_row_named(root, "Remind me…")
                .expect("the menu offers its row")
        };
        press_at(staged, row);
        assert!(
            shows_dialog(&staged.harness().inspection().root, "Remind me…"),
            "the row opens the reminder's form"
        );
    }

    /// The control of the window announcing `label`, as a reader meets it.
    fn announced<'a>(
        node: &'a ui::headless::InspectionNode,
        label: &str,
    ) -> Option<&'a ui::a11y::AccessNode> {
        if node
            .semantics
            .as_ref()
            .is_some_and(|access| access.name.as_deref() == Some(label))
        {
            return node.semantics.as_ref();
        }
        node.children
            .iter()
            .find_map(|child| announced(child, label))
    }

    /// A form of one question is answered by the choice a person picks: the pick submits it, so
    /// it closes with the task reminded, with nothing else to press.
    #[test]
    fn a_quick_choice_answers_a_one_question_form_in_one_press() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let (width, height) = (390, 844);
        let mut staged = Staged::over(
            &app,
            &packages,
            "a busy week",
            (width, height),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged
            .lay_out(
                (width, height),
                ui::Theme::light(),
                ui::interaction::InteractionEnvironment::TOUCH,
                1.0,
            )
            .expect("the window lies out for a finger");
        let before = reminded(&staged, &app);
        open_reminder_form(&mut staged, &packages);
        let choice = {
            let root = &staged.harness().inspection().root;
            let moment = announced(root, "Tomorrow morning")
                .expect("the form offers the moments a person names");
            let when = moment.description.clone().unwrap_or_default();
            assert!(
                when.contains(':') && when.len() > 4,
                "each moment says when it is, in the person's own format: {when:?}"
            );
            assert!(
                crate::layout_check::button_named(root, "Remind me").is_none(),
                "one pick answers the form: its footer needs no confirm button"
            );
            assert!(
                crate::layout_check::button_named(root, "Cancel").is_some(),
                "Cancel stands there with nothing left to confirm"
            );
            crate::layout_check::button_named(root, "Tomorrow morning")
                .expect("the form offers the moments a person names")
        };
        staged.press(&choice, true).expect("the choice presses");
        staged.freshen().expect("the window comes to rest");
        assert!(
            !shows_dialog(&staged.harness().inspection().root, "Remind me…"),
            "picking when answers the form: it closes"
        );
        assert_eq!(
            reminded(&staged, &app),
            before + 1,
            "and the task is reminded"
        );
    }

    /// The person types the moment instead: the entry the form opens on takes what they type,
    /// the confirm button stands beside Cancel for the moment typed, and pressing it reminds.
    #[test]
    fn a_moment_typed_into_the_form_is_confirmed_by_its_button() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let (width, height) = (390, 844);
        let mut staged = Staged::over(
            &app,
            &packages,
            "a busy week",
            (width, height),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged
            .lay_out(
                (width, height),
                ui::Theme::light(),
                ui::interaction::InteractionEnvironment::TOUCH,
                1.0,
            )
            .expect("the window lies out for a finger");
        let before = reminded(&staged, &app);
        open_reminder_form(&mut staged, &packages);
        staged.harness_mut().type_text("tomorrow 9:30");
        staged.freshen().expect("the window comes to rest");
        let confirm = {
            let root = &staged.harness().inspection().root;
            assert!(
                shows_dialog(root, "Remind me…"),
                "typing leaves the form open to confirm"
            );
            crate::layout_check::button_named(root, "Remind me")
                .expect("a moment typed by hand is confirmed by the form's button")
        };
        staged.press(&confirm, true).expect("the button presses");
        staged.freshen().expect("the window comes to rest");
        assert!(
            !shows_dialog(&staged.harness().inspection().root, "Remind me…"),
            "the form closes once the moment typed is confirmed"
        );
        assert_eq!(
            reminded(&staged, &app),
            before + 1,
            "the task is reminded at the moment the person typed"
        );
    }

    /// A menu row on a touchscreen is pressed where it stands, by a finger: a mouse press there
    /// only dismisses the menu the finger opened, and the row runs nothing.
    #[test]
    fn a_menu_row_of_a_touchscreen_window_is_pressed_by_a_tap() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let (width, height) = (390, 844);
        let mut staged = Staged::over(
            &app,
            &packages,
            "a busy week",
            (width, height),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged
            .lay_out(
                (width, height),
                ui::Theme::light(),
                ui::interaction::InteractionEnvironment::TOUCH,
                1.0,
            )
            .expect("the window lies out for a finger");
        open_reminder_form(&mut staged, &packages);
    }

    /// Press `bounds` as a person presses them, and let the window and its app answer.
    fn press_at(staged: &mut Staged, bounds: ui::kurbo::Rect) {
        let center = bounds.center();
        staged
            .press_at(center.x, center.y)
            .expect("the window comes to rest");
    }

    /// The first menu button of the window's composed tree whose rows `wanted` picks, with its
    /// node and those rows: the menu the app composes as it does at a person's press.
    fn composed_menu(
        staged: &mut Staged,
        wanted: &dyn Fn(&[String]) -> bool,
    ) -> Option<(tilo_ui_host::UiNode, Vec<String>)> {
        fn find(
            node: &tilo_ui_host::UiNode,
            wanted: &dyn Fn(&[String]) -> bool,
        ) -> Option<tilo_ui_host::UiNode> {
            if node.kind == "ui.MenuButton" && wanted(&menu_rows_of(node)) {
                return Some(node.clone());
            }
            node.children.iter().find_map(|child| find(child, wanted))
        }
        let root = staged.composed()?;
        let menu = find(&root, wanted)?;
        let rows = menu_rows_of(&menu);
        Some((menu, rows))
    }

    fn menu_rows_of(node: &tilo_ui_host::UiNode) -> Vec<String> {
        fn rows(node: &tilo_ui_host::UiNode, out: &mut Vec<String>) {
            if node.kind == "ui.MenuItem" {
                if let Some(Value::Text(label)) = node.props.get("label") {
                    out.push(label.clone());
                }
                return;
            }
            for child in &node.children {
                rows(child, out);
            }
        }
        let mut out = Vec::new();
        rows(node, &mut out);
        out
    }

    /// A row of a menu the app binds to a published action runs that action once, through the
    /// same door an agent's invocation of it takes: the row is a part of its menu button, and
    /// the person's press is the action's own route.
    #[test]
    fn a_press_on_a_menu_row_the_app_binds_runs_its_action_once_through_the_door() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let task_type = packages
            .0
            .last()
            .expect("the app's meta")
            .meta
            .types
            .iter()
            .find(|ty| ty.name == "Task")
            .expect("a task type")
            .id
            .clone();
        let page_view = packages
            .0
            .last()
            .expect("the app's meta")
            .meta
            .views
            .iter()
            .find(|view| view.name == "task_page")
            .expect("a task page")
            .id
            .clone();
        let (width, height) = (1280, 800);
        let mut staged = Staged::over(
            &app,
            &packages,
            "a busy week",
            (width, height),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged.harness.relayout();
        // A task outside the inbox: its page offers the menu that files it back into it.
        let task = staged
            .resources()
            .into_iter()
            .find(|resource| resource.ty == task_type)
            .expect("the scenario holds a task");
        // Asked for as a page, as its row's menu asks: the page holds the menu that files it.
        staged.open_with(&task, &page_view);
        let descriptors = publish_actions(&mut staged);
        // The facts menu files the task into a project, its own included, so filing it back
        // into the inbox is a row bound to that action.
        let (menu, rows) = composed_menu(&mut staged, &|rows| {
            rows.iter().any(|row| row == "Inbox") && !rows.iter().any(|row| row == "Reopen")
        })
        .expect("the page offers the menu that files the task");
        assert!(
            rows.iter().any(|label| label == "Inbox"),
            "filing the task is one of its menu's rows: {rows:?}"
        );
        staged.press(&menu.key, false).expect("the menu opens");
        staged.harness.relayout();
        // The row a press reaches is the one the action is bound to: the open menu's Inbox.
        let (unfile, _control_id, control) = descriptors
            .iter()
            .filter(|descriptor| descriptor.action_label == "Inbox")
            .find_map(|descriptor| {
                let routes = staged.harness().action_ui_routes(&descriptor.address);
                let [route] = routes.as_slice() else {
                    return None;
                };
                Some((
                    descriptor,
                    route.control_id.clone(),
                    route.bounds.expect("the row is laid out"),
                ))
            })
            .unwrap_or_else(|| {
                panic!("the app binds one row to Inbox: {rows:?}");
            });
        let event = interaction(unfile);
        let locus = staged
            .harness()
            .semantic_locus(&event)
            .expect("the row the app binds the action to");
        assert!(
            control.contains(locus.center()),
            "the agent acts at the row: {locus:?} vs {control:?}"
        );
        let (before, after) = capture_agent(&mut staged, &event, "tasks-menu-tilolang");
        let marked = changed_pixels(before.as_slice(), after.as_slice(), width, pixels(control));
        let mark = marked.expect("the agent's marker rests on the row");
        assert!(
            control.contains(ui::kurbo::Point::new(f64::from(mark.0), f64::from(mark.1))),
            "the marker is drawn inside the row the action lands on"
        );

        press_at(&mut staged, control);
        let chosen = staged.instance.take_chosen();
        assert_eq!(
            chosen.as_ref().map(|(_press, address, _argument)| address),
            Some(&unfile.address),
            "one press runs the row's action once"
        );
        assert!(
            staged.instance.take_chosen().is_none(),
            "the press runs one action, not two"
        );
    }

    /// A section's add the app binds to a published action runs it once, through the same door:
    /// the heading's button is a part of the list, and the person's press is the action's own
    /// route.
    #[test]
    fn a_press_on_a_sections_add_the_app_binds_runs_its_action_once_through_the_door() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let (width, height) = (1280, 800);
        let mut staged = Staged::over(
            &app,
            &packages,
            "a busy week",
            (width, height),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged.harness.relayout();
        let descriptors = publish_actions(&mut staged);
        let (add, control) = descriptors
            .iter()
            .filter(|descriptor| descriptor.action_label == "New project")
            .find_map(|descriptor| {
                let routes = staged.harness().action_ui_routes(&descriptor.address);
                let route = routes
                    .into_iter()
                    .find(|route| route.control_id.starts_with("section-action:"))?;
                Some((
                    descriptor,
                    route.bounds.expect("the heading's add is laid out"),
                ))
            })
            .expect("the app binds its section's add to the action it publishes");
        let event = interaction(add);
        let locus = staged
            .harness()
            .semantic_locus(&event)
            .expect("the button the app binds the action to");
        assert!(
            control.contains(locus.center()),
            "the agent acts at the heading's add: {locus:?} vs {control:?}"
        );
        let (before, after) = capture_agent(&mut staged, &event, "tasks-section-add-tilolang");
        let marked = changed_pixels(before.as_slice(), after.as_slice(), width, pixels(control));
        assert!(marked.is_some(), "the agent's marker rests on the add");

        press_at(&mut staged, control);
        let chosen = staged.instance.take_chosen();
        assert_eq!(
            chosen.as_ref().map(|(_press, address, _argument)| address),
            Some(&add.address),
            "one press runs the add once"
        );
        assert!(
            staged.instance.take_chosen().is_none(),
            "the press runs one action, not two"
        );
    }

    /// A row's check box runs the check action the app binds to it, once, through the same door
    /// an agent's invocation of it takes — for a task the page around the list does not show, so
    /// the press is the row's own.
    #[test]
    fn a_press_on_a_rows_check_box_runs_the_actions_the_app_binds_to_it() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let mut staged = Staged::over(
            &app,
            &packages,
            "a busy week",
            (1280, 800),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        staged.harness.relayout();
        let descriptors = publish_actions(&mut staged);
        let (row, check) = descriptors
            .iter()
            .find_map(|descriptor| {
                bound_box(&staged, descriptor)
                    .map(|(_id, _control, check)| (descriptor.clone(), check))
            })
            .expect("a row's check box is bound to the check action it runs");

        // The box itself, where a person clicks it: the rest of the row is the row's own press.
        press_at(&mut staged, check);
        let chosen = staged.instance.take_chosen();
        assert_eq!(
            chosen.as_ref().map(|(_press, address, _argument)| address),
            Some(&row.address),
            "one press runs the check's action once"
        );
        assert!(
            staged.instance.take_chosen().is_none(),
            "the press runs one action, not two"
        );
    }

    #[test]
    fn a_press_on_a_tasks_row_opens_it_where_it_stands() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        // The task the scenario answers with is the one open; another row of the same list
        // takes its place when pressed.
        let answered = "Call the landlord about the boiler";
        let label = "Book a haircut";
        for (width, height) in [(1280_u32, 800_u32), (390, 844)] {
            let mut staged = Staged::over(
                &app,
                &packages,
                "today",
                (width, height),
                ui::Theme::light(),
                None,
                &[],
            )
            .expect("the scenario stages");
            staged.harness.relayout();
            let id = super::super::button_named(&staged.harness().inspection().root, label)
                .expect("the task's row is a row of the list");
            {
                let root = &staged.harness().inspection().root;
                row_named(root, label).expect("the row shows before it opens");
                assert!(
                    !shows_open(root, label),
                    "the row stands closed before anyone presses it"
                );
                assert!(
                    shows_open(root, answered),
                    "the task the scenario answers with is the one open"
                );
            }

            staged.press(&id, false).expect("the row presses");

            let root = &staged.harness().inspection().root;
            assert!(
                shows_open(root, label),
                "the row opened where it stands at {width}×{height}"
            );
            assert!(
                !shows_open(root, answered),
                "one row is open at a time: the other closed at {width}×{height}"
            );
            assert!(
                shown_named(root, "Subtasks"),
                "the row holds the task's document where it stands at {width}×{height}"
            );
            if width <= 390 {
                assert!(
                    crate::layout_check::back_chip(&mut staged, (width, height)),
                    "a window too narrow for the list beside the document fills with it, and \
                     carries the shell's way back at {width}×{height}"
                );
            }

            staged.key("Escape");
            let root = &staged.harness().inspection().root;
            assert!(!shows_open(root, label), "Escape closes the document again");
            assert!(
                !shown_named(root, "Subtasks"),
                "Escape puts the document away again"
            );
        }
    }

    /// A scenario that answers with a task stages that task's place with its row open where it
    /// stands: the window a script answers in is the list, not a page laid over it.
    #[test]
    fn a_scenario_that_answers_with_a_task_shows_its_row_open_in_its_place() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let label = "Call the landlord about the boiler";
        for (width, height) in [(1280_u32, 800_u32), (390, 844)] {
            let staged = Staged::over(
                &app,
                &packages,
                "a busy week",
                (width, height),
                ui::Theme::light(),
                None,
                &[],
            )
            .expect("the scenario stages");

            let root = &staged.harness().inspection().root;
            assert!(
                shows_open(root, label),
                "the task stands open where it stands at {width}×{height}"
            );
            assert!(
                shown_named(root, "Subtasks"),
                "the row holds the task's document at {width}×{height}"
            );
            // The window states the place the task belongs to, not the place it stood on: what
            // waits in the inbox does not stand in the list, and what is due today does.
            assert!(
                !shown_named(root, "Sort the paper mail"),
                "the inbox's work is not what the window shows at {width}×{height}"
            );
            assert!(
                shown_named(root, "Buy oat milk and eggs"),
                "today's work is what the window shows at {width}×{height}"
            );
            // Where the window shows its places beside the list, they say the same: the task's
            // place is the chosen one.
            if width > 390 {
                assert!(
                    chosen_named(root, "Today"),
                    "the sidebar follows the task to its place at {width}×{height}"
                );
            }
        }
    }

    /// A row opens as a page of its own from its menu, and Back returns to the list with the
    /// row still open where it stood: the page is a place to read the item whole, not a way out
    /// of the list.
    #[test]
    fn a_row_opens_as_a_page_and_back_returns_to_the_list_with_the_row_open() {
        tilo_ui_host::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.0.last().expect("the app").meta.app.clone();
        let task_type = packages
            .0
            .last()
            .expect("the app's meta")
            .meta
            .types
            .iter()
            .find(|ty| ty.name == "Task")
            .expect("a task type")
            .id
            .clone();
        let page_view = packages
            .0
            .last()
            .expect("the app's meta")
            .meta
            .views
            .iter()
            .find(|view| view.name == "task_page")
            .expect("a task page")
            .id
            .clone();
        let label = "Call the landlord about the boiler";
        for (width, height) in [(1280_u32, 800_u32), (390, 844)] {
            let mut staged = Staged::over(
                &app,
                &packages,
                "a busy week",
                (width, height),
                ui::Theme::light(),
                None,
                &[],
            )
            .expect("the scenario stages");
            // The row offers the page in the menu its view places, which a person opens beside
            // the row: the row's own menu, the same one its actions stand in.
            let rows = staged.rows();
            let (key, row) = rows
                .iter()
                .find(|(_key, row)| holds_title(row, label))
                .expect("the list draws the task's row");
            let offered = menu_labels(row);
            assert!(
                offered.iter().any(|row| row == "Open as page"),
                "opening the task as a page is one of its menu's rows: {offered:?}"
            );
            // A row is keyed by the view that draws it, which ends in the item it shows: that is
            // the task the row's menu opens as a page.
            let key = key.clone();
            let task = staged
                .resources()
                .into_iter()
                .find(|resource| resource.ty == task_type && key.ends_with(&resource.id))
                .unwrap_or_else(|| panic!("the row is the task {key} it shows"));
            staged.open_with(&task, &page_view);

            assert_eq!(
                staged.front_target().map(|front| front.ty),
                Some(task_type.clone()),
                "the window shows the task as a page at {width}×{height}"
            );
            assert!(
                crate::layout_check::back_chip(&mut staged, (width, height)),
                "the page carries the shell's way back at {width}×{height}, not one of its own"
            );
            assert!(
                titled(&staged.harness().inspection().root, label),
                "the page names the task in its heading at {width}×{height}"
            );

            staged
                .press_bar_slot(ui::widget::WINDOW_BACK)
                .expect("the shell's back presses");
            staged.freshen().expect("the list comes back to rest");
            assert_ne!(
                staged.front_target().map(|front| front.ty),
                Some(task_type.clone()),
                "Back returns to the list at {width}×{height}"
            );
            let root = &staged.harness().inspection().root;
            assert!(
                shows_open(root, label),
                "the task stands open where it stood at {width}×{height}"
            );
        }
    }

    /// Whether the window's place list stands its entry named `label` chosen.
    fn chosen_named(node: &ui::headless::InspectionNode, label: &str) -> bool {
        node.semantics
            .as_ref()
            .is_some_and(|access| access.name.as_deref() == Some(label) && access.state.selected)
            || node.children.iter().any(|child| chosen_named(child, label))
    }

    /// Whether a row of a list says `label` as its own title: a subtask's row names the task it
    /// stands under beside it, and is not that task's row.
    fn holds_title(node: &tilo_ui_host::UiNode, label: &str) -> bool {
        node.props.get("title") == Some(&Value::text(label))
            || node.children.iter().any(|child| holds_title(child, label))
    }

    /// Whether what `node` holds names `label` where a document is titled: its heading reads it
    /// as the title, or the window shows it under that name.
    fn titled(node: &ui::headless::InspectionNode, label: &str) -> bool {
        node.semantics.as_ref().is_some_and(|access| {
            access.value.as_deref() == Some(label) || access.name.as_deref() == Some(label)
        }) || node.children.iter().any(|child| titled(child, label))
    }

    /// Whether the window shows anything named `label` — a heading, a section or a control.
    fn shown_named(node: &ui::headless::InspectionNode, label: &str) -> bool {
        node.semantics
            .as_ref()
            .is_some_and(|access| access.name.as_deref() == Some(label))
            || node.children.iter().any(|child| shown_named(child, label))
    }

    /// The list row named `label`, as the window shows it.
    fn row_named<'a>(
        node: &'a ui::headless::InspectionNode,
        label: &str,
    ) -> Option<&'a ui::headless::InspectionNode> {
        let is_row = node.semantics.as_ref().is_some_and(|access| {
            matches!(
                access.role,
                ui::a11y::Role::ListItem | ui::a11y::Role::TreeItem
            ) && access.name.as_deref() == Some(label)
        });
        if is_row {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|child| row_named(child, label))
    }

    /// The rows of the menus a row carries: the one its view places on it, and the actions the
    /// host adds to it.
    fn menu_labels(row: &tilo_ui_host::UiNode) -> Vec<String> {
        fn labeled(node: &tilo_ui_host::UiNode, out: &mut Vec<String>) {
            if node.kind == "ui.MenuItem"
                && let Some(Value::Text(label)) = node.props.get("label")
            {
                out.push(label.clone());
            }
            node.children.iter().for_each(|child| labeled(child, out));
        }
        fn collect(node: &tilo_ui_host::UiNode, out: &mut Vec<String>) {
            if node.kind == "slot" && node.props.get("name") == Some(&Value::text("menu")) {
                labeled(node, out);
            }
            node.children.iter().for_each(|child| collect(child, out));
        }
        let mut out = Vec::new();
        collect(row, &mut out);
        out
    }

    /// Whether a list shows `label` as a row standing open into its item: the node the list
    /// publishes for that row, which is where a screen reader reads it.
    fn shows_open(node: &ui::headless::InspectionNode, label: &str) -> bool {
        let published = node.semantics.as_ref().is_some_and(|semantics| {
            semantics.children.iter().any(|item| {
                item.name.as_deref() == Some(label)
                    && matches!(
                        item.role,
                        ui::a11y::Role::ListItem | ui::a11y::Role::TreeItem
                    )
                    && item.state.expanded == Some(true)
            })
        });
        published || node.children.iter().any(|child| shows_open(child, label))
    }
}
