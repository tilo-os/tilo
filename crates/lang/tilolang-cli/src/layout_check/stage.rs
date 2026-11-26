//! An app's window as the app window shows it, with nothing on screen ([`tilolang_window::Window`]):
//! its instance over a workspace of its own, staged with a scenario, its views laid out by the
//! toolkit's frame pipeline, in the desktop's window bar, and read as a person reads it. Nothing is
//! rasterised and no GPU is opened.

use std::rc::Rc;

use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::{ResourceRef, ToApp, Value};
use tilolang_tilo::runtime::value::{ResourceRef as RuntimeRef, Value as RuntimeValue};
use ui::headless::Driver;
use ui::interaction::InteractionEnvironment;

/// The motion a read window is allowed before it is a finding: two seconds of frames at the pace a
/// live window presents them, counted in frames so a loaded machine reads the same window a quiet
/// one does ([`ui::headless::capture_frames`]).
const SETTLE_BOUND: ui::time::Duration = ui::time::Duration::from_secs(2);

pub use tilolang_window::Packages;
use tilolang_window::{EXCHANGES, Opening, Window};

/// One window of the app, laid out by one frame pipeline that every configuration reuses.
pub struct Staged {
    #[cfg(test)]
    app: String,
    window: Window,
    /// What the window has cost so far, as [`Self::take_work`] reports it since the last call.
    work: Work,
}

#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaceShown {
    pub key: String,
    pub shows: String,
    pub record: String,
}

/// What laying a window out cost, in the toolkit's own units rather than in time: frames laid out,
/// layout nodes built, and text shaped that the renderer's cache did not hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Work {
    /// Frames the toolkit laid out (a layout is one, plus one per change the app answered it with).
    pub frames: u64,
    pub nodes: u64,
    pub shaped: u64,
    /// What the layout solver did for those frames: the trees it solved, the passes it took, the
    /// manual regions it ran and the manual regions it kept, what it probed and what it measured.
    pub layout: ui::widget::counts::LayoutCounts,
}

impl Work {
    /// Add `other` to this.
    pub fn add(&mut self, other: Self) {
        self.frames = self.frames.saturating_add(other.frames);
        self.nodes = self.nodes.saturating_add(other.nodes);
        self.shaped = self.shaped.saturating_add(other.shaped);
        self.layout.add(other.layout);
    }
}

/// The person a staged window opens as.
pub const ME: &str = "ada";

impl Staged {
    /// The app `app` of `packages` as the window's bar names it: its icon and its title, as the
    /// launcher and the desktop's bar wear them.
    fn name_window(harness: &mut Driver, app: &str, packages: &Rc<Packages>) {
        let meta = packages.platform.iter().find(|meta| meta.app == app);
        let title = meta.map_or_else(String::new, |meta| meta.title.clone());
        let icon = meta
            .and_then(|meta| meta.icon.as_deref())
            .and_then(|declared| ui::icons::Icon::from_name(&declared.replace('_', "-")))
            .unwrap_or(ui::icons::Icon::AppWindow);
        harness.set_window_app(ui::widget::AppKind::App(icon), title);
    }
}

/// What a window is staged over and for, beyond its app, its packages
/// and its size: the workspace it opens over, the layer it shows, the
/// scenario staged into it, and whether what its units run is counted.
#[derive(Default)]
pub struct Staging {
    pub scenario: Option<String>,
    /// The layer a package with more than one root shows (`Shell`, `Layout`).
    pub root: Option<String>,
    /// How many items the workspace holds when the window opens ([`tilolang_tilo::seed`]).
    pub seed: usize,
    /// Whether what the app's units run is counted ([`tilolang_tilo::runtime::cost`]).
    pub costs: bool,
    /// The view states the run stages, as the app's own code names them (`place=Task.today`), as
    /// `layout-check --state` takes them: which place a window stands on before anyone picks.
    pub states: Vec<String>,
}

impl Staged {
    /// The app `app` of `packages` with `scenario` staged, its window laid out at `viewport`, showing
    /// the layer `root` names rather than the package's first root: a package with more than one root
    /// (the shell's `Shell` beside its `Layout`) shows each, and each is laid out as a person sees
    /// it.
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

    /// As [`Self::over`], staged as `staging` says: its scenario, its layer, over the workspace it
    /// brings (a seeded one, as a person with ten thousand items has it), counting what the app's
    /// units run ([`tilolang_tilo::runtime::cost`]) when it says so.
    pub fn over_workspace(
        app: &str,
        packages: &Rc<Packages>,
        viewport: (u32, u32),
        theme: ui::Theme,
        staging: Staging,
    ) -> Result<Self, String> {
        let Staging {
            scenario,
            root,
            seed,
            costs,
            states,
        } = staging;
        // A package's first scenario stands on what its outside services hold: the samples a
        // program linking those services registered (`tools/layout-check` registers the Mail and
        // Tube apps'), taken in through the services' origins before it runs. With none, it
        // opens on what its scenario writes alone.
        let mut opening = Opening::new(viewport, theme);
        opening.root.root = root;
        opening.scenario = scenario.as_deref();
        opening.me = (ME, "Ada");
        opening.seed = seed;
        opening.costs = costs;
        opening.states = &states;
        let mut staged = Self {
            #[cfg(test)]
            app: app.to_owned(),
            window: Window::open(app, packages, opening)?,
            work: Work::default(),
        };
        // The window a person sees: the desktop's bar over the app's frame, the app's parts in it,
        // and the app told that the bar hosts them, as the desktop tells it.
        Self::name_window(&mut staged.window.harness, app, packages);
        staged.window.harness.present_in_window_bar();
        staged.exchange();
        Ok(staged)
    }

    fn window(&mut self) -> Option<&mut AppWindow> {
        self.window.app_window()
    }

    /// Let the window and its instance answer each other until neither has more to say
    /// ([`Window::exchange`]).
    fn exchange(&mut self) {
        self.window.exchange();
    }

    /// Lay the window out at `viewport` under `theme`, for a person using `input`.
    pub fn lay_out(
        &mut self,
        viewport: (u32, u32),
        mut theme: ui::Theme,
        input: InteractionEnvironment,
        display_scale: f64,
    ) -> Result<(), String> {
        theme.reduce_motion = true;
        self.window.harness.set_input_scale(display_scale)?;
        self.window
            .harness
            .relayout_window_themed_for(viewport, theme, input)
            .map_err(|error| format!("layout failed: {error:?}"))?;
        self.exchange();
        // A motion that lands even when reduced (a cross-fade) runs out on the toolkit's clock.
        self.settle()?;
        Ok(())
    }

    /// The window and its instance answer what the person just did, and the frames that follow
    /// settle, as a live window draws them: nothing is laid out anew that the input did not change.
    ///
    /// # Errors
    /// When the window still moves after the frames a capture waits for.
    pub fn answer(&mut self) -> Result<(), String> {
        self.exchange();
        self.settle()
    }

    /// Press `key` (`Tab`, `Down`, `Space`: a key as [`Driver::key`] names it) on the window,
    /// as a person at the keyboard does.
    pub fn key(&mut self, key: &str) {
        self.window.harness.key(key);
    }

    /// Rest the pointer at `x`, `y`, as a person pointing at what stands there does.
    pub fn hover(&mut self, x: f64, y: f64) {
        self.window.harness.hover_at(x, y);
    }

    /// What the app's units ran since the last [`Self::reset_costs`], by unit
    /// ([`tilolang_tilo::runtime::cost`]), when the costs are being counted.
    pub fn costs(&self) -> Option<tilolang_tilo::runtime::Costs> {
        self.window.instance.host()?.runtime().costs()
    }

    pub fn reset_costs(&self) {
        if let Some(host) = self.window.instance.host() {
            host.runtime().reset_costs();
        }
    }

    pub fn harness(&self) -> &Driver {
        &self.window.harness
    }

    /// The places the staged shell holds, as the shell's own layout has them: each the app it
    /// shows and the record it shows.
    #[cfg(test)]
    pub fn places(&self) -> Vec<PlaceShown> {
        let Some(host) = self.window.instance.host() else {
            return Vec::new();
        };
        let Some(meta) = host.package(&self.app).map(|package| package.meta) else {
            return Vec::new();
        };
        // The fields the layout and its places keep, by the names the shell declared them
        // under: a record is stored by the field's id.
        let field = |ty: &str, name: &str| -> Option<String> {
            meta.types
                .iter()
                .find(|held| held.name == ty)?
                .fields
                .iter()
                .find(|held| held.name == name)
                .map(|held| held.id.clone())
        };
        let (Some(places), Some(key), Some(shows), Some(record)) = (
            field("Layout", "places"),
            field("LayoutPlace", "key"),
            field("LayoutPlace", "shows"),
            field("LayoutPlace", "resource"),
        ) else {
            return Vec::new();
        };
        let layer = tilolang_tilo::host::RootView {
            root: Some("Layout".to_owned()),
            view: None,
        };
        let Ok(root) = host.root_of_layer(&self.app, &layer) else {
            return Vec::new();
        };
        let runtime = host.runtime();
        let Ok(held) = runtime.read(&root, &places) else {
            return Vec::new();
        };
        let text = |item: &RuntimeRef, id: &str| -> String {
            match runtime.read(item, id) {
                Ok(RuntimeValue::Text(said)) => said,
                _ => String::new(),
            }
        };
        refs_in(&held)
            .iter()
            .map(|item| PlaceShown {
                key: text(item, &key),
                shows: text(item, &shows),
                record: text(item, &record),
            })
            .collect()
    }

    /// What laying the window out has cost since the last call: frames laid out, layout nodes built,
    /// text shaped that the renderer's cache did not hold, and what the layout solver did for them.
    pub fn take_work(&mut self) -> Work {
        let frames = self.window.harness.relayout_frames();
        let nodes = self.window.harness.layout_nodes_built();
        let (_, misses, _) = self.window.harness.take_text_shape_stats();
        let delta = Work {
            frames: frames.saturating_sub(self.work.frames),
            nodes: nodes.saturating_sub(self.work.nodes),
            shaped: misses,
            layout: ui::widget::counts::take(),
        };
        self.work.frames = frames;
        self.work.nodes = nodes;
        self.work.shaped = self.work.shaped.saturating_add(misses);
        delta
    }

    pub fn harness_mut(&mut self) -> &mut Driver {
        &mut self.window.harness
    }

    fn records(&self) -> Vec<ResourceRef> {
        let mut out = Vec::new();
        let Some(host) = self.window.instance.host() else {
            return out;
        };
        let runtime = host.runtime();
        for (_, _, doc) in host.records() {
            let Some(ty) = host.store().type_of_id(&doc) else {
                continue;
            };
            let root = RuntimeRef::root(doc.clone(), ty.clone());
            out.push(ResourceRef {
                id: doc.clone(),
                ty: ty.clone(),
            });
            let Some(schema) = tilolang_tilo::schema::lookup(&ty) else {
                continue;
            };
            for field in &schema.fields {
                if field.kind != tilolang_tilo::MergeKind::Collection {
                    continue;
                }
                if let Ok(items) = runtime.read(&root, &field.id) {
                    out.extend(refs_in(&items).into_iter().map(|item| ResourceRef {
                        id: item.id_string(),
                        ty: item.ty,
                    }));
                }
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
        fn in_node(node: &tilo_app_window::UiNode, out: &mut Vec<ResourceRef>) {
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

    /// Tap the window at `at`, as a finger does, and let it answer: a row
    /// of a list is painted by the list, one widget without a place of its
    /// own to tap by name, so a person's hand lands on where the row is
    /// drawn.
    pub fn tap_at(&mut self, at: (f64, f64)) -> Result<(), String> {
        self.window.harness.tap_at(at.0, at.1);
        self.exchange();
        self.settle()
    }

    /// The resource the window shows where a person holding it is
    /// looking: nothing while the sidebar stands in front, and the page
    /// the window pushed in front of its list (or opened over it) once
    /// that does.
    pub fn front_target(&mut self) -> Option<ResourceRef> {
        let window = self.window()?;
        let sidebar = matches!(
            window.shell_ref().map(ui::widget::NavShell::front_pane),
            Some(ui::widget::NavPane::Sidebar | ui::widget::NavPane::Overview)
        );
        if sidebar {
            return None;
        }
        if let Some(detail) = window.detail_target() {
            return Some(detail.clone());
        }
        let chosen = window.composed().and_then(Self::chosen_in);
        chosen.or_else(|| window.root_target().cloned())
    }

    /// The resource the lists of `tree` stand on: the item a list of a window whose page is its own
    /// shows, by the list's own selection.
    fn chosen_in(tree: &tilo_app_window::UiNode) -> Option<ResourceRef> {
        let chosen = match tree.props.get("selection") {
            Some(Value::List(items)) => items.iter().find_map(|item| match item {
                Value::Ref(reference) => Some(reference.clone()),
                _ => None,
            }),
            _ => None,
        };
        chosen.or_else(|| tree.children.iter().find_map(Self::chosen_in))
    }

    /// The tree the window composed from what the app sent, as sent: every view that ran is in it,
    /// keyed by its view, even one that drew nothing (a bare `if let` whose value is absent leaves
    /// an empty fragment), where the laid-out widgets keep no trace of it.
    pub fn composed(&mut self) -> Option<tilo_app_window::UiNode> {
        self.window()?.composed().cloned()
    }

    /// The trees of the rows the window's lists show, each with its key, in the order the window
    /// lists them: the lists of the window's own level first and the lists of every other level after
    /// them, each level's by key, and each list's rows as its own model orders them.
    pub fn rows(&mut self) -> Vec<(String, tilo_app_window::UiNode)> {
        let Some(window) = self.window() else {
            return Vec::new();
        };
        let mut lists: Vec<(bool, String)> = window
            .lists()
            .into_iter()
            .map(|list| {
                let elsewhere = window
                    .rows_of(&list)
                    .is_some_and(|rows| rows.borrow().level() != tilo_app_window::Level::Root);
                (elsewhere, list)
            })
            .collect();
        lists.sort();
        let mut out = Vec::new();
        for (_, list) in lists {
            if let Some(rows) = window.rows_of(&list) {
                let rows = rows.borrow();
                for key in rows.in_order() {
                    if let Some(row) = rows.row(&key) {
                        out.push((key, row.clone()));
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
        self.window.harness.relayout();
    }

    /// Lay the window out at `viewport` under `theme` for a person using `input`, with motion as it
    /// is live: what a person's press then sets moving moves ([`Self::press`]).
    pub fn lay_out_moving(
        &mut self,
        viewport: (u32, u32),
        mut theme: ui::Theme,
        input: InteractionEnvironment,
    ) -> Result<(), String> {
        theme.reduce_motion = false;
        self.window
            .harness
            .relayout_window_themed_for(viewport, theme, input)
            .map_err(|error| format!("layout failed: {error:?}"))?;
        self.exchange();
        self.settle()
    }

    /// Press the control `id` of the window as a person does, with a finger on a touchscreen
    /// (`touch`) and the mouse otherwise (a click is a mouse's: the window then sizes itself for
    /// one), let the window and its instance answer it, and let the motion it starts run out: the
    /// frames a live window draws, and no layout afresh after them.
    pub fn press(&mut self, id: &str, touch: bool) -> Result<(), String> {
        let pressed = if touch {
            self.window.harness.tap_named(id)
        } else {
            self.window.harness.click_named(id)
        };
        pressed.map_err(|error| format!("pressing {id:?}: {error}"))?;
        self.exchange();
        self.settle()
    }

    /// Press the shell's own control `id` in the window's header row — its
    /// Back, its sidebar toggle — as the person holding the window presses
    /// it: a finger's tap where the window is held in the hand, a click
    /// otherwise.
    pub fn press_shell(&mut self, id: &str) -> Result<(), String> {
        let touch = self.window.harness.interaction_environment().modality
            == ui::interaction::InputModality::Touch;
        self.press(id, touch)
    }

    /// Press `x`, `y` as a person presses them, and let the window and its instance answer: what
    /// reaches a row a menu paints, which has no node of its own to name.
    pub fn press_at(&mut self, x: f64, y: f64) -> Result<(), String> {
        // A row is pressed the way the window expects its person to: a finger taps it, where a
        // mouse press on a menu a finger opened only dismisses the menu.
        if self.window.harness.interaction_environment().modality
            == ui::interaction::InputModality::Touch
        {
            self.window.harness.tap_at(x, y);
        } else {
            self.window.harness.click_at(x, y);
        }
        self.exchange();
        self.settle()
    }

    /// Lay the window out as it stands now, so that what a press opened over it — a menu, a
    /// popover, a sheet — is there to be read and pressed in turn.
    pub fn freshen(&mut self) -> Result<(), String> {
        self.exchange();
        self.window.harness.relayout();
        self.settle()
    }

    /// Press the action the control `id` publishes as a semantic child, as assistive technology
    /// activates it: a control a widget paints itself (an empty state's action) has no node of its
    /// own to tap, and the widget's own node is not where the action is.
    pub fn press_action(&mut self, id: &str) -> Result<(), String> {
        self.window
            .harness
            .perform_named(id, &ui::a11y::Action::Activate)
            .map_err(|error| format!("pressing {id:?}: {error}"))?;
        self.exchange();
        self.settle()
    }

    /// Press the control `target` a widget of `id` publishes as a semantic child, as assistive
    /// technology activates it: the box beside a row's title, which a person presses by its own
    /// words rather than by the row's.
    pub fn press_child(&mut self, id: &str, target: &str) -> Result<(), String> {
        self.window
            .harness
            .perform_semantic_named(id, Some(target), &ui::a11y::Action::Activate)
            .map_err(|error| format!("pressing {target:?}: {error}"))?;
        self.exchange();
        self.settle()
    }

    fn settle(&mut self) -> Result<(), String> {
        for _ in 0..EXCHANGES {
            // A widget that took a layout switch after its parts were placed asks for the frame
            // that lays them out again, as a live window draws it.
            if self.window.harness.root_widget().is_layout_dirty() {
                self.window.harness.relayout();
            }
            if !self.window.harness.frame_schedule().needs_animation_frame
                || self.window.harness.settle_for_capture(SETTLE_BOUND)
            {
                return Ok(());
            }
            // A live window sends the app what its frames asked for on every frame: a list laid
            // out where it stands asks for the rows it shows there. Answer it, then settle again.
            let owing = self.window.app_window_ref().is_some_and(AppWindow::owes)
                || self.window.instance.pending();
            if !owing {
                break;
            }
            self.exchange();
        }
        // A frame a widget asks for is named by the widget; one a motion asks for is named
        // where the motion was started; and what else the window is owed — a frame at rest, an
        // image on its way, work whose answer replaces a placeholder — is named, since none of
        // those is a motion that never ends.
        let now = ui::time::now();
        Err(format!(
            "the window still moves after {} frames: {:?} asked, {:?} started motions, \
             {:?} waiting on an answer, a press still drawn moving: {}, the frame held until \
             {:?}, the schedule {:?} owed for {:?} with {} images on their way",
            ui::headless::capture_frames(SETTLE_BOUND),
            self.window.harness.frame_requesters(),
            ui::motion::frames_wanted_by(now),
            self.window.harness.pending_visual_work(),
            (self.window.harness.presence())
                .interaction_animation_demand(now, self.window.harness.theme()),
            self.window.harness.root_widget().holds_frame_until(now),
            self.window.harness.frame_schedule(),
            self.window.harness.queued_work_sources(),
            ui::image_store::loads_in_flight(),
        ))
    }

    /// Show `resource` with the view `view` (an id) in the window's detail
    /// level: a view the system shows on a surface of its own (a row view as
    /// a search result), laid out as a window
    /// is.
    pub fn open_with(&mut self, resource: &ResourceRef, view: &str) {
        self.window.instance.open_with(resource, view);
        self.exchange();
        self.window.harness.relayout();
    }

    /// Show the view `view` (a name) as the window's own, over no resource: a screen a host shows
    /// by name (a phone's shell), which takes no arguments.
    pub fn open_screen(&mut self, view: &str) {
        self.window.instance.open_screen(view);
        self.exchange();
        self.window.harness.relayout();
    }

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

/// The resources a collection's reading names, in order.
fn refs_in(value: &RuntimeValue) -> Vec<RuntimeRef> {
    match value {
        RuntimeValue::Ref(target) => vec![RuntimeRef::clone(&target.target)],
        RuntimeValue::List(items) => items.iter().flat_map(refs_in).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tilo_render_protocol::patch::ToUi;

    /// The packages of the app `name` of the apps the
    /// platform ships (`apps/tl/<name>`) and what it
    /// uses.
    pub(crate) fn app_packages(name: &str) -> Rc<Packages> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps/tl")
            .join(name);
        let search = vec![dir.parent().expect("apps/tl").to_path_buf()];
        let (built, platform) =
            crate::layout_check::build(&dir, &search).expect("the app compiles");
        Rc::new(Packages::new(
            Rc::new(built.into_iter().map(|b| b.compiled).collect()),
            platform,
        ))
    }

    /// The app `name`'s window staged at `scenario` and `size`, in the light theme: its packages
    /// and the staged window.
    fn staged_over(name: &str, scenario: &str, size: (u32, u32)) -> (Rc<Packages>, Staged) {
        tilo_app_window::i18n::ensure();
        let packages = app_packages(name);
        let app = packages.compiled.last().expect("the app").meta.app.clone();
        let staged = Staged::over(
            &app,
            &packages,
            scenario,
            size,
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the scenario stages");
        (packages, staged)
    }

    /// An agent's invocation of `descriptor` lands on `control`, where the app binds the action,
    /// and its marker comes to rest there (captured as `name`): where the marker drew.
    fn agent_acts_at(
        staged: &mut Staged,
        descriptor: &tilo_actions::ActionDescriptor,
        control: ui::kurbo::Rect,
        width: u32,
        name: &str,
    ) -> (u32, u32, u32, u32) {
        let event = interaction(descriptor);
        let locus = staged
            .harness()
            .semantic_locus(&event)
            .expect("the control the app binds the action to");
        assert!(
            control.contains(locus.center()),
            "the agent acts at the control: {locus:?} vs {control:?}"
        );
        let (before, after) = capture_agent(staged, &event, name);
        changed_pixels(&before, &after, width, pixels(control))
            .expect("the agent's marker rests on the control the app binds")
    }

    /// The press just made ran `descriptor`'s action, once.
    fn ran_once(staged: &mut Staged, descriptor: &tilo_actions::ActionDescriptor) {
        let chosen = staged.window.instance.take_chosen();
        assert_eq!(
            chosen.as_ref().map(|(_press, address, _argument)| address),
            Some(&descriptor.address),
            "one press runs the action once"
        );
        assert!(
            staged.window.instance.take_chosen().is_none(),
            "the press runs one action, not two"
        );
    }

    /// The actions the app publishes at its window, offered the way its host offers them: the
    /// window's palette, action directory and agents read them off it from then on.
    fn publish_actions(staged: &mut Staged) -> Vec<tilo_actions::ActionDescriptor> {
        let host = staged.window.instance.host().expect("a host");
        let planner: std::sync::Arc<dyn tilolang_tilo::publish::Planner> =
            std::sync::Arc::new(host.handle());
        let mut publications = tilolang_tilo::publish::Publications::new(&host);
        // Records are published whole off the frame: a window with no frames waits for them.
        let resources: Vec<tilo_actions::ResourceSpec> = publications
            .everything(&host, &planner)
            .into_iter()
            .map(|tree| (*tree).clone())
            .collect();
        let descriptors = tilo_actions::api::survey(&resources);
        staged
            .window
            .instance
            .say(ToUi::Actions(tilo_app_window::catalogue::whole(
                &descriptors,
            )));
        staged.window.instance.flush();
        staged.exchange();
        staged.window.harness.relayout();
        descriptors
    }

    /// Where the captures of a frame go: the target
    /// directory's own, as the toolkit's marker captures
    /// do.
    fn marker_shots() -> std::path::PathBuf {
        match std::env::var_os("CARGO_TARGET_DIR") {
            Some(dir) => std::path::PathBuf::from(dir).join("tilolang-marker"),
            None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/tilolang-marker"),
        }
    }

    /// The agent's interaction with `descriptor`, as an agent states it: one action, applying.
    fn interaction(
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> tilo_actions::SemanticInteraction {
        tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: tilo_actions::InvocationId::new(),
            sequence: 1,
            actor: tilo_actions::Acting::agent("scribe", "ada"),
            phase: tilo_actions::InteractionPhase::Applying,
            action: tilo_actions::InteractionAction {
                id: descriptor.address.action.clone(),
                label: descriptor.action_label.clone(),
                app: None,
                step: None,
                creates: None,
                did: descriptor.did.clone(),
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

    /// The agent's mark on the control it acts through, captured: `<name>-before.png` is the frame
    /// before, `<name>-agent-marker.png` the one once the mark has come to rest, and the pixels of
    /// both come back for the test to hold the drawing to.
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
                .insert(ui::permissions::MemberId::person("scribe"), "Ada".into());
            presence
                .apply_interaction(event.clone())
                .expect("a well-formed interaction applies")
        });
        assert!(applied, "the agent's action shows");
        // The frames the pointer glides in, run out: the capture
        // shows it at rest on the control rather than mid-glide.
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
        let faults = ui::inspection::tag_faults(&staged.harness().inspection().root);
        assert!(faults.is_empty(), "the agent's tag: {faults:?}");
        (before, staged.harness().render_rgba())
    }

    /// The window's header row keeps one "…" and shows each control once, and the bar over it
    /// offers none of them again: what does not fit the row goes behind the row's one overflow button.
    #[test]
    fn the_window_shows_each_control_once() {
        let (width, height) = (960, 1080);
        let (_, mut staged) = staged_over("tasks", "a busy week", (width, height));
        staged
            .lay_out(
                (width, height),
                ui::Theme::light(),
                ui::interaction::InteractionEnvironment::POINTER,
                1.0,
            )
            .expect("the window lies out");
        let bar = staged.harness_mut().window_bar_layout();
        let row = crate::layout_check::header_row_controls(&staged, (width, height));
        let named = row
            .iter()
            .map(|control| control.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let overflow = row.iter().filter(|control| control.menu).count();
        assert_eq!(overflow, 1, "the window's row keeps one \"…\": {named}");
        let mut distinct: Vec<&str> = row.iter().map(|control| control.name.as_str()).collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            row.len(),
            "the row shows each control once: {named}"
        );
        let buttons: Vec<String> = row.iter().map(|control| control.name.clone()).collect();
        let problems: Vec<_> = crate::layout_check::audit::bar(&bar)
            .into_iter()
            .chain(crate::layout_check::audit::offered_twice(&bar, &buttons))
            .collect();
        assert!(
            problems.is_empty(),
            "the bar offers nothing the row does: {:?} ({named})",
            problems.iter().map(|p| p.kind).collect::<Vec<_>>(),
        );
    }

    /// A message is written before there is an account to send it from: it waits in Drafts and
    /// goes once the person adds one. So Mail with no account yet offers New message as its
    /// primary button, once, at every width the header row has room for the words or not, as a
    /// window with an account does: the control acts, so it stands.
    #[test]
    fn a_mail_window_with_no_account_offers_new_message() {
        tilo_app_window::i18n::ensure();
        let packages = app_packages("mail");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
        let shown = |scenario: &str, (width, height): (u32, u32), text: f32| {
            let theme = ui::Theme::light().with_text_scale(text);
            let mut staged = Staged::over(
                &app,
                &packages,
                scenario,
                (width, height),
                theme.clone(),
                None,
                &[],
            )
            .expect("the scenario stages");
            staged
                .lay_out(
                    (width, height),
                    theme,
                    ui::interaction::InteractionEnvironment::TOUCH,
                    1.0,
                )
                .expect("the window lies out");
            crate::layout_check::header_row_controls(&staged, (width, height))
                .into_iter()
                .filter(|control| control.name == "New message")
                .map(|control| control.primary)
                .collect::<Vec<_>>()
        };
        for (size, text) in [((320, 568), 1.0_f32), ((390, 844), 1.5), ((1280, 800), 1.0)] {
            assert_eq!(
                shown("no account yet", size, text),
                [true],
                "a window with no account offers New message, which starts a draft that waits \
                 for one, as its primary button: {size:?} at text {text}"
            );
        }
        assert_eq!(
            shown("a busy inbox", (1280, 800), 1.0),
            [true],
            "an account's window offers New message as its primary button"
        );
    }

    /// A window has a way back only when it went somewhere: the scenario that stages the feed of
    /// the app's own root stands where the app opens, and its header row carries no Back, while a
    /// page opened over the feed does.
    #[test]
    fn a_window_on_its_own_root_offers_no_way_back() {
        tilo_app_window::i18n::ensure();
        let packages = app_packages("tube");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
        let back = |scenario: &str| -> bool {
            let (width, height) = (1280, 800);
            let theme = ui::Theme::light();
            let mut staged = Staged::over(
                &app,
                &packages,
                scenario,
                (width, height),
                theme.clone(),
                None,
                &[],
            )
            .expect("the scenario stages");
            staged
                .lay_out(
                    (width, height),
                    theme,
                    ui::interaction::InteractionEnvironment::POINTER,
                    1.0,
                )
                .expect("the window lies out");
            crate::layout_check::header_row_controls(&staged, (width, height))
                .iter()
                .any(|control| control.id == ui::widget::WINDOW_BACK)
        };
        assert!(
            !back("YouTube cannot be reached"),
            "a window where the app opens has nothing to go back to"
        );
        assert!(
            back("a channel"),
            "a channel's page went somewhere, and Back leaves it"
        );
    }

    /// Every notice's message under `node`, its whole tree walked.
    fn notice_said(node: &tilo_app_window::UiNode, out: &mut Vec<String>) {
        if node.kind == "ui.Banner"
            && let Some(Value::Text(said)) = node.props.get("message")
        {
            out.push(said.clone());
        }
        for child in &node.children {
            notice_said(child, out);
        }
    }

    /// The clock a window says a moment in is the person's own: the notice over a mail list says
    /// when the server last answered in the style they chose in Settings, as the list beside it
    /// reads the times of its messages.
    #[test]
    fn a_notice_says_the_time_in_the_clock_the_person_reads() {
        tilo_app_window::i18n::ensure();
        ui::clock::freeze_for_capture();
        let packages = app_packages("mail");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
        let notice = |style: ui::clock::TimeStyle| -> String {
            ui::format::region::install(ui::format::region::RegionFormats {
                time: style,
                ..ui::format::region::RegionFormats::default()
            });
            let mut staged = Staged::over(
                &app,
                &packages,
                "an account that cannot be reached",
                (1280, 800),
                ui::Theme::light(),
                None,
                &[],
            )
            .expect("the scenario stages");
            let composed = staged.composed().expect("the window composes");
            let mut said = Vec::new();
            notice_said(&composed, &mut said);
            said.pop().unwrap_or_else(|| {
                panic!("the window says why it cannot reach the server: {composed:?}")
            })
        };
        // 12-hour where the person chose it: the notice reads the clock the list beside it reads.
        assert_eq!(
            notice(ui::clock::TimeStyle::H12),
            "Connection refused. Last fetched 10:41 AM",
            "the notice says the time as the person reads clocks"
        );
        // 24-hour where they chose that: one clock for everything the window says.
        assert_eq!(
            notice(ui::clock::TimeStyle::H24),
            "Connection refused. Last fetched 10:41"
        );
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
    fn changed_pixels(
        before: &[u8],
        after: &[u8],
        width: u32,
        region: (u32, u32, u32, u32),
    ) -> Option<(u32, u32, u32, u32)> {
        changed_pixels_clear_of(before, after, width, region, ui::kurbo::Rect::ZERO)
    }

    /// The bounds of the pixels that changed between `before` and `after` within `region`, leaving
    /// out those `chrome` covers: what a frame draws over the region rather than in it.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "a pixel index and its neighbours inside a frame's own dimensions"
    )]
    fn changed_pixels_clear_of(
        before: &[u8],
        after: &[u8],
        width: u32,
        region: (u32, u32, u32, u32),
        chrome: ui::kurbo::Rect,
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
            // A pixel is the area the frame draws between its whole edges: one the chrome covers
            // whole is the chrome's, and one it only partly covers is still the frame's.
            let centre = ui::kurbo::Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            if chrome.contains(centre) {
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
        staged: &mut Staged,
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> Option<(String, ui::kurbo::Rect, ui::kurbo::Rect)> {
        let routes = staged.harness_mut().action_ui_routes(&descriptor.address);
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

    /// The button the counter's window binds to its action runs that
    /// action through the action API, once, as the person's own: the press is
    /// the action's route, not a view event the app runs in the window
    /// instead.
    #[test]
    fn a_press_on_the_button_an_app_binds_runs_its_action_once_through_the_api() {
        let (width, height) = (900, 600);
        let (_, mut staged) = staged_over("counter", "a busy morning", (width, height));
        staged.window.harness.relayout();
        let descriptors = publish_actions(&mut staged);
        let increment = descriptors
            .iter()
            .find(|descriptor| descriptor.action_label == "Increment")
            .expect("the counter publishes the action its button runs");
        let routes = staged.harness_mut().action_ui_routes(&increment.address);
        let [route] = routes.as_slice() else {
            panic!(
                "the window binds one button to Increment, as it binds one to every action it \
                 offers a control for: {routes:?}"
            );
        };
        let control = route.bounds.expect("the button is laid out");
        agent_acts_at(&mut staged, increment, control, width, "counter-tilolang");
        staged
            .press(&route.control_id, false)
            .expect("the button presses");
        ran_once(&mut staged, increment);
    }

    /// The nodes of the window `node`, each before what it holds.
    fn preorder<'a>(
        node: &'a ui::headless::InspectionNode,
        out: &mut Vec<&'a ui::headless::InspectionNode>,
    ) {
        out.push(node);
        for child in &node.children {
            preorder(child, out);
        }
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
        let host = staged.window.instance.host().expect("a host");
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

    /// The reminder's form, opened from a task's row menu as a
    /// finger opens it: the menu's "Remind me…" row asks when.
    ///
    /// A menu row on a touchscreen is pressed where it stands, by a finger: a mouse press there
    /// only dismisses the menu the finger opened, and the row runs nothing. Both tests of the
    /// form open it this way, in a window laid out for a finger, and each holds the form open.
    fn open_reminder_form(staged: &mut Staged, packages: &Rc<Packages>) {
        // The task stands open as its page: the reminder's menu is part of the document, and a page
        // takes the presses the controls of a row's document do not (yet) reach.
        let meta = &packages.compiled.last().expect("the app's meta").meta;
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
            .is_some_and(|access| access.answers_to(label))
        {
            return node.semantics.as_ref();
        }
        node.children
            .iter()
            .find_map(|child| announced(child, label))
    }

    /// A form of one question is answered by the choice a person picks: the pick submits it, so it
    /// closes with the task reminded, with nothing else to press.
    #[test]
    fn a_quick_choice_answers_a_one_question_form_in_one_press() {
        let (width, height) = (390, 844);
        let (packages, mut staged) = staged_over("tasks", "a busy week", (width, height));
        staged
            .lay_out(
                (width, height),
                ui::Theme::light(),
                ui::interaction::InteractionEnvironment::TOUCH,
                1.0,
            )
            .expect("the window lies out for a finger");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
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

    /// The person types the moment instead: the entry the form opens on takes what they type, the
    /// confirm button stands beside Cancel for the moment typed, and pressing it reminds.
    #[test]
    fn a_moment_typed_into_the_form_is_confirmed_by_its_button() {
        let (width, height) = (390, 844);
        let (packages, mut staged) = staged_over("tasks", "a busy week", (width, height));
        staged
            .lay_out(
                (width, height),
                ui::Theme::light(),
                ui::interaction::InteractionEnvironment::TOUCH,
                1.0,
            )
            .expect("the window lies out for a finger");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
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

    /// Press `bounds` as a person presses them, and let the window and its app answer.
    fn press_at(staged: &mut Staged, bounds: ui::kurbo::Rect) {
        let center = bounds.center();
        staged
            .press_at(center.x, center.y)
            .expect("the window comes to rest");
    }

    /// The first menu button of the window's composed tree whose rows `wanted` picks, with its node
    /// and those rows: the menu the app composes as it does at a person's press.
    fn composed_menu(
        staged: &mut Staged,
        wanted: &dyn Fn(&[String]) -> bool,
    ) -> Option<(tilo_app_window::UiNode, Vec<String>)> {
        fn find(
            node: &tilo_app_window::UiNode,
            wanted: &dyn Fn(&[String]) -> bool,
        ) -> Option<tilo_app_window::UiNode> {
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

    fn menu_rows_of(node: &tilo_app_window::UiNode) -> Vec<String> {
        fn rows(node: &tilo_app_window::UiNode, out: &mut Vec<String>) {
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

    /// A row of a menu the app binds to a published action runs that action once, through the same
    /// action API an agent's invocation of it takes: the row is a part of its menu button, and the
    /// person's press is the action's own route.
    #[test]
    fn a_press_on_a_menu_row_the_app_binds_runs_its_action_once_through_the_api() {
        let (width, height) = (1280, 800);
        let (packages, mut staged) = staged_over("tasks", "a busy week", (width, height));
        let meta = &packages.compiled.last().expect("the app's meta").meta;
        let task_type = meta
            .types
            .iter()
            .find(|ty| ty.name == "Task")
            .expect("a task type")
            .id
            .clone();
        let page_view = meta
            .views
            .iter()
            .find(|view| view.name == "task_page")
            .expect("a task page")
            .id
            .clone();
        staged.window.harness.relayout();
        // A task outside the inbox: its page offers the menu that files it back into it.
        let task = staged
            .resources()
            .into_iter()
            .find(|resource| resource.ty == task_type)
            .expect("the scenario holds a task");
        // Asked for as a page, as its row's menu asks: the page holds the menu that files it.
        staged.open_with(&task, &page_view);
        let descriptors = publish_actions(&mut staged);
        // The facts menu files the task into a project, its own included, so filing it back into
        // the inbox is a row bound to that action.
        let (menu, rows) = composed_menu(&mut staged, &|rows| {
            rows.iter().any(|row| row == "Inbox") && !rows.iter().any(|row| row == "Reopen")
        })
        .expect("the page offers the menu that files the task");
        assert!(
            rows.iter().any(|label| label == "Inbox"),
            "filing the task is one of its menu's rows: {rows:?}"
        );
        staged.press(&menu.key, false).expect("the menu opens");
        staged.window.harness.relayout();
        // The row a press reaches is the one the action is bound to: the open menu's Inbox.
        let (unfile, _control_id, control) = descriptors
            .iter()
            .filter(|descriptor| descriptor.action_label == "Inbox")
            .find_map(|descriptor| {
                let routes = staged.harness_mut().action_ui_routes(&descriptor.address);
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
        let mark = agent_acts_at(&mut staged, unfile, control, width, "tasks-menu-tilolang");
        assert!(
            control.contains(ui::kurbo::Point::new(f64::from(mark.0), f64::from(mark.1))),
            "the marker is drawn inside the row the action lands on"
        );
        press_at(&mut staged, control);
        ran_once(&mut staged, unfile);
    }

    /// A section's add the app binds to a published action runs it once, through the same actior: the
    /// heading's button is a part of the list, and the person's press is the action's own route.
    #[test]
    fn a_press_on_a_sections_add_the_app_binds_runs_its_action_once_through_the_api() {
        let (width, height) = (1280, 800);
        let (_, mut staged) = staged_over("tasks", "a busy week", (width, height));
        staged.window.harness.relayout();
        let descriptors = publish_actions(&mut staged);
        let (add, control) = descriptors
            .iter()
            .filter(|descriptor| descriptor.action_label == "New project")
            .find_map(|descriptor| {
                let routes = staged.harness_mut().action_ui_routes(&descriptor.address);
                let route = routes
                    .into_iter()
                    .find(|route| route.control_id.starts_with("section-action:"))?;
                Some((
                    descriptor,
                    route.bounds.expect("the heading's add is laid out"),
                ))
            })
            .expect("the app binds its section's add to the action it publishes");
        agent_acts_at(
            &mut staged,
            add,
            control,
            width,
            "tasks-section-add-tilolang",
        );
        press_at(&mut staged, control);
        ran_once(&mut staged, add);
    }

    /// A row's check box runs the check action the app binds to it, once,
    /// through the same action API an agent's invocation of it takes — for a task
    /// the page around the list does not show, so the press is the row's
    /// own.
    #[test]
    fn a_press_on_a_rows_check_box_runs_the_actions_the_app_binds_to_it() {
        let (_, mut staged) = staged_over("tasks", "a busy week", (1280, 800));
        staged.window.harness.relayout();
        let descriptors = publish_actions(&mut staged);
        let (row, check) = descriptors
            .iter()
            .find_map(|descriptor| {
                bound_box(&mut staged, descriptor)
                    .map(|(_id, _control, check)| (descriptor.clone(), check))
            })
            .expect("a row's check box is bound to the check action it runs");

        // The box itself, where a person clicks it: the rest of the row is the row's own press.
        press_at(&mut staged, check);
        ran_once(&mut staged, &row);
    }

    #[test]
    fn a_press_on_a_tasks_row_opens_it_where_it_stands() {
        tilo_app_window::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
        for size in [(1280_u32, 800_u32), (390, 844)] {
            // A window too narrow for the list and the item at once shows one thing at a time: pressing a
            // row opens the task's page in front of the list, where the shell's back returns to the rows.
            let narrow = size.0 <= 390;
            let mut staged = Staged::over(
                &app,
                &packages,
                "today",
                size,
                ui::Theme::light(),
                None,
                &[],
            )
            .expect("the scenario stages");
            staged.window.harness.relayout();
            if narrow {
                // The task the scenario answers with stands in front of the list, as a row
                // pressed there does; the shell's back shows the list behind it.
                assert!(
                    page_shows(&staged.harness().inspection().root, ANSWERED),
                    "a narrow window shows the task the scenario answers with in front at {}×{}",
                    size.0,
                    size.1
                );
                back_to_the_list(&mut staged, size);
            }
            let id = super::super::button_named(&staged.harness().inspection().root, PRESSED)
                .expect("the task's row is a row of the list");
            stands_before_the_press(&staged.harness().inspection().root, narrow, size);

            staged.press(&id, false).expect("the row presses");

            let root = &staged.harness().inspection().root;
            assert!(
                !shows_open(root, ANSWERED),
                "one row is open at a time: the other closed at {}×{}",
                size.0,
                size.1
            );
            if narrow {
                opens_in_front_and_back_returns(&mut staged, size);
            } else {
                opens_beside_the_list(root, size);
            }
        }
    }

    /// The task the "today" scenario answers with: the one its window holds open.
    const ANSWERED: &str = "Call the landlord about the boiler";
    /// Another row of the same list, which takes the answered task's place when pressed.
    const PRESSED: &str = "Book a haircut";

    /// What the window of the "today" scenario shows before anyone presses a row: at a narrow
    /// `size`, back on the list behind the answered task, the list alone with that task a row of
    /// it; wider, the answered task's page in the content column beside the list, whose row
    /// stands chosen.
    fn stands_before_the_press(
        root: &ui::headless::InspectionNode,
        narrow: bool,
        size: (u32, u32),
    ) {
        let (width, height) = size;
        named(root, ROWS, PRESSED).expect("the row shows before it opens");
        assert!(
            !shows_open(root, PRESSED),
            "the row stands closed before anyone presses it"
        );
        if narrow {
            assert!(
                !shows_open(root, ANSWERED),
                "the list behind the task's page opens no row into its item at {width}×{height}"
            );
            assert!(
                named(root, ROWS, ANSWERED).is_some(),
                "the task the scenario answers with is a row of the place's list at \
                 {width}×{height}"
            );
            return;
        }
        // The list is the window's overview and the task's page stands in the content column
        // beside it, so the row stays a row and says by its choice that its task is the one open.
        assert!(
            page_shows(root, ANSWERED),
            "the task the scenario answers with is the one the content column holds"
        );
        assert!(
            chosen_named(root, ANSWERED),
            "the task's row stands chosen, so the list says what is open"
        );
        assert!(
            !page_shows(root, PRESSED),
            "no other task's page stands beside the list"
        );
    }

    /// After a press on a row of a narrow window: the task's page stands in front of the list,
    /// carrying the shell's way back, and Back puts it away and shows the rows again.
    fn opens_in_front_and_back_returns(staged: &mut Staged, size: (u32, u32)) {
        let (width, height) = size;
        let root = &staged.harness().inspection().root;
        assert!(
            !shows_open(root, PRESSED),
            "the list behind the page keeps no row open at {width}×{height}"
        );
        assert!(
            shown_named(root, "Subtasks"),
            "pressing the row opens the task in front of the list at {width}×{height}"
        );
        assert!(
            crate::layout_check::back_chip(staged, size),
            "the page carries the shell's way back at {width}×{height}"
        );
        staged
            .press_shell(ui::widget::WINDOW_BACK)
            .expect("the shell's back presses");
        staged.freshen().expect("the list comes back to rest");
        let root = &staged.harness().inspection().root;
        assert!(
            !shown_named(root, "Subtasks"),
            "the shell's back puts the page away again at {width}×{height}"
        );
        assert!(
            named(root, ROWS, PRESSED).is_some(),
            "the list comes back with its rows at {width}×{height}"
        );
    }

    /// The shell's back of a narrow window that shows a task's page in front of its list: the
    /// page carries it, and it shows the list again.
    fn back_to_the_list(staged: &mut Staged, size: (u32, u32)) {
        let (width, height) = size;
        assert!(
            shown_named(&staged.harness().inspection().root, "Subtasks"),
            "the task's page stands in front of the list at {width}×{height}"
        );
        assert!(
            crate::layout_check::back_chip(staged, size),
            "the task's page carries the shell's way back at {width}×{height}"
        );
        staged
            .press_shell(ui::widget::WINDOW_BACK)
            .expect("the shell's back presses");
        staged.freshen().expect("the list comes back to rest");
        assert!(
            !shown_named(&staged.harness().inspection().root, "Subtasks"),
            "the shell's back puts the page away at {width}×{height}"
        );
    }

    /// After a press on a row of a window wide enough for both: the pressed task's page takes
    /// the content column, its row stands chosen in place of the answered one's, and the list
    /// keeps the row.
    fn opens_beside_the_list(root: &ui::headless::InspectionNode, size: (u32, u32)) {
        let (width, height) = size;
        assert!(
            page_shows(root, PRESSED),
            "the pressed task stands in the content column at {width}×{height}"
        );
        assert!(
            chosen_named(root, PRESSED),
            "the pressed row is the one the list says is open at {width}×{height}"
        );
        assert!(
            !chosen_named(root, ANSWERED),
            "the row of the task before it is no longer chosen at {width}×{height}"
        );
        assert!(
            !page_shows(root, ANSWERED),
            "the task before it left the content column at {width}×{height}"
        );
        assert!(
            shown_named(root, "Subtasks"),
            "the content column holds the task's document at {width}×{height}"
        );
        assert!(
            named(root, ROWS, PRESSED).is_some(),
            "the list keeps the row while its page stands beside it at {width}×{height}"
        );
    }

    /// A scenario that answers with a task stages that task's place with its page in the content
    /// column and its row chosen in the list: the window a script answers in is the list and the
    /// task beside it, not a page laid over the list. A window too narrow for both shows the
    /// task's page in front, as a press on its row does, with the list a step back.
    #[test]
    fn a_scenario_that_answers_with_a_task_shows_its_row_open_in_its_place() {
        tilo_app_window::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
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
            // The window is laid out before the person reads it: a list
            // too narrow for an item beside its row settles as it draws.
            staged.window.harness.relayout();

            if width <= 390 {
                // One thing at a time in a window too narrow for the list beside the item: the
                // task's page stands in front, and the shell's back shows the place's list behind
                // it, the task a row of it.
                assert!(
                    page_shows(&staged.harness().inspection().root, label),
                    "a narrow window shows the task in front of its list at {width}×{height}"
                );
                back_to_the_list(&mut staged, (width, height));
                assert!(
                    named(&staged.harness().inspection().root, ROWS, label).is_some(),
                    "the task the scenario answers with is a row of the place's list at \
                     {width}×{height}"
                );
            }
            let root = &staged.harness().inspection().root;
            if width > 390 {
                // The task the scenario answers with stands in the content column beside the
                // list, and its row says by its choice that it is the one open.
                assert!(
                    page_shows(root, label),
                    "the task stands in the content column at {width}×{height}"
                );
                assert!(
                    chosen_named(root, label),
                    "the task's row stands chosen at {width}×{height}"
                );
                assert!(
                    shown_named(root, "Subtasks"),
                    "the content column holds the task's document at {width}×{height}"
                );
                // The document lists the task's subtasks, and the list keeps each of them as a
                // row of its own beside it.
                let outline =
                    named(root, &[ui::a11y::Role::Tree], "Subtasks").expect("the task's outline");
                for subtask in [
                    "Find the last service date",
                    "Ask whether Tuesday morning works",
                ] {
                    let row = named(outline, ROWS, subtask).unwrap_or_else(|| {
                        panic!("the outline shows {subtask:?} at {width}×{height}")
                    });
                    let ([_, top, _, tall], [_, start, _, extent]) = (row.rect, outline.rect);
                    assert!(
                        tall > 0.0 && top >= start - 0.5 && top + tall <= start + extent + 0.5,
                        "the outline holds its row {subtask:?} at {width}×{height}: {:?} in {:?}",
                        row.rect,
                        outline.rect
                    );
                    // The document lists the step once, and the list keeps it as one row of its
                    // own: the window holds it once in each place, never twice in one.
                    assert_eq!(
                        count_named(outline, ROWS, subtask),
                        1,
                        "{subtask:?} stands once in the document at {width}×{height}"
                    );
                    assert_eq!(
                        count_named(root, ROWS, subtask),
                        2,
                        "{subtask:?} stands in the document and in the list beside it at \
                         {width}×{height}, once in each"
                    );
                }
                // The outline's row says how far its branch got, as the list's row of the same task
                // does: "Find the last service date" holds a step of its own, undone.
                let held = named(outline, ROWS, "Find the last service date")
                    .expect("the outline shows the step");
                assert_eq!(
                    held.semantics
                        .as_ref()
                        .and_then(|access| access.value.as_deref()),
                    Some("0/1"),
                    "the outline counts what stands under the step at {width}×{height}"
                );
            }
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
            // Where the window shows its places beside the list,
            // they say the same: the task's place is the chosen
            // one.
            if width > 390 {
                assert!(
                    chosen_named(root, "Today"),
                    "the sidebar follows the task to its place at {width}×{height}"
                );
            }
        }
    }

    /// A task's steps stand on the column of the words over them when none of them holds a step of its
    /// own: a tree keeps a column for a chevron only where a row of that level has something to fold.
    ///
    /// The narrow window's list is read first, as it stands behind the task the scenario
    /// answers with and before its row is pressed: where a
    /// step another day heads stands in it
    /// (`a_step_due_another_day_stands_at_its_lists_first_level`).
    #[test]
    fn a_tasks_steps_that_fold_nothing_stand_on_their_sections_column() {
        tilo_app_window::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
        let task = "Call the landlord about the boiler";
        let steps = ["Find the last service date", "Book the engineer's visit"];
        for (width, height) in [(1280_u32, 800_u32), (390, 844)] {
            let mut staged = Staged::over(
                &app,
                &packages,
                "a task that is late",
                (width, height),
                ui::Theme::light(),
                None,
                &[],
            )
            .expect("the scenario stages");
            staged.window.harness.relayout();
            if width <= 390 {
                // The scenario answers with the task, which a narrow window shows in front of the
                // list: the shell's back reads the list it stands over.
                back_to_the_list(&mut staged, (width, height));
                a_step_due_another_day_stands_at_its_lists_first_level(&staged);
                // A window too narrow for the list and the item at once: the task's row opens its
                // page in front of the list, and the steps stand in the page.
                let id = super::super::button_named(&staged.harness().inspection().root, task)
                    .expect("the task's row is a row of the list");
                staged.press(&id, false).expect("the row presses");
            }
            let root = &staged.harness().inspection().root;
            let heading = named(root, &[ui::a11y::Role::Heading { level: 2 }], "Subtasks")
                .unwrap_or_else(|| panic!("the section names itself at {width}×{height}"));
            let outline = named(root, &[ui::a11y::Role::Tree], "Subtasks")
                .unwrap_or_else(|| panic!("the section lists its steps at {width}×{height}"));
            for step in steps {
                let row = named(outline, ROWS, step)
                    .unwrap_or_else(|| panic!("the outline shows {step:?} at {width}×{height}"));
                let Some(ui::widget::LayoutInfo::Row(row)) = &row.layout else {
                    panic!("a row of an outline says the column it stands on");
                };
                let left = f64::from(row.content_left);
                // A row keeps the bleed every row of a list keeps, and
                // no step for a chevron: what it holds folds nothing.
                let bleed = ui::chrome::row_bleed(&ui::Theme::light());
                assert!(
                    (left - heading.rect[0]).abs() <= bleed + 1.0,
                    "the step {step:?} starts at x {left:.0} where the words naming its section \
                     stand at x {:.0}: a level with nothing to fold keeps no chevron's column \
                     at {width}×{height}",
                    heading.rect[0]
                );
            }
        }
    }

    /// A step the sort put under another day's heading stands on the level its list starts at, and
    /// says which task it is a step of: indented, it would stand under a row that is not there, and
    /// the person reading the day could not tell what it belongs to.
    ///
    /// `staged` is the tasks app's "a task that is late" at 390×844 in the light theme, laid out
    /// once and not yet pressed: this reads it and leaves it as it stood.
    fn a_step_due_another_day_stands_at_its_lists_first_level(staged: &Staged) {
        let (width, height) = (390_u32, 844_u32);
        // A step due the day its task is due keeps its place under
        // it; one due another day opens the group its own day
        // heads.
        let task = "Call the landlord about the boiler";
        let same_day = "Find the last service date";
        let other_day = "Book the engineer's visit";
        let root = &staged.harness().inspection().root;
        let row_of = |title: &str| {
            let row = named(root, ROWS, title).unwrap_or_else(|| {
                panic!("{title:?} is a row of the place's list at {width}×{height}")
            });
            let Some(ui::widget::LayoutInfo::Row(grid)) = &row.layout else {
                panic!("a row of a list says the column it stands on");
            };
            (row, grid.depth)
        };
        let (_, top) = row_of(task);
        let (_, nested) = row_of(same_day);
        assert!(
            nested > top,
            "a step due the day its task is due stands under it at {width}×{height}: depth \
             {nested} against {top}"
        );
        let (row, found) = row_of(other_day);
        assert_eq!(
            found, top,
            "a step another day's heading opens stands at the first level of its list at \
             {width}×{height}, not indented under a row of another day"
        );
        let said = spoken(row);
        assert!(
            said.contains("in ") && said.contains(task),
            "the row says which task it is a step of: {said:?}"
        );
    }

    /// What a reader hears of a row and the nodes it holds, as one line.
    fn spoken(node: &ui::headless::InspectionNode) -> String {
        fn heard(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
            out.extend(node.value.clone());
            out.extend(node.name.clone());
            for child in &node.children {
                heard(child, out);
            }
        }
        fn walk(node: &ui::headless::InspectionNode, out: &mut Vec<String>) {
            if let Some(access) = &node.semantics {
                heard(access, out);
            }
            for child in &node.children {
                walk(child, out);
            }
        }
        let mut said = Vec::new();
        walk(node, &mut said);
        said.join(", ")
    }

    /// A row opens as a page of its own from its menu, and Back returns to the list with the row
    /// still open where it stood: the page is a place to read the item whole, not a way out of the
    /// list.
    #[test]
    fn a_row_opens_as_a_page_and_back_returns_to_the_list_with_the_row_open() {
        tilo_app_window::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
        let task_type = packages
            .compiled
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
            .compiled
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
            // The row offers the page in the menu its view places, which a person opens beside the
            // row: the row's own menu, the same one its actions stand in.
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
                crate::layout_check::back_chip(&staged, (width, height)),
                "the page carries the shell's way back at {width}×{height}, not one of its own"
            );
            assert!(
                titled(&staged.harness().inspection().root, label),
                "the page names the task in its heading at {width}×{height}"
            );

            staged
                .press_shell(ui::widget::WINDOW_BACK)
                .expect("the shell's back presses");
            staged.freshen().expect("the list comes back to rest");
            assert_ne!(
                staged.front_target().map(|front| front.ty),
                Some(task_type.clone()),
                "Back returns to the list at {width}×{height}"
            );
            let root = &staged.harness().inspection().root;
            // A phone keeps one thing on screen: the list comes back at rest, its rows closed.
            if width <= 390 {
                assert!(
                    named(root, ROWS, label).is_some(),
                    "the place's list comes back with its rows at {width}×{height}"
                );
                assert!(
                    !shown_named(root, "Subtasks"),
                    "no row holds the task's document at {width}×{height}"
                );
            } else {
                assert!(
                    shows_open(root, label),
                    "the task stands open where it stood at {width}×{height}"
                );
            }
        }
    }

    /// Whether the window holds `label`'s page in its content column: a page titles its record
    /// in the field at its head, which a reader sees as the field's value.
    fn page_shows(node: &ui::headless::InspectionNode, label: &str) -> bool {
        node.semantics.as_ref().is_some_and(|access| {
            access.role == ui::a11y::Role::TextInput && access.value.as_deref() == Some(label)
        }) || node.children.iter().any(|child| page_shows(child, label))
    }

    /// Whether the window's place list stands its entry named `label` chosen.
    fn chosen_named(node: &ui::headless::InspectionNode, label: &str) -> bool {
        node.semantics
            .as_ref()
            .is_some_and(|access| access.answers_to(label) && access.state.selected)
            || node.children.iter().any(|child| chosen_named(child, label))
    }

    /// Whether a row of a list says `label` as its own title: a subtask's row names the task it
    /// stands under beside it, and is not that task's row.
    fn holds_title(node: &tilo_app_window::UiNode, label: &str) -> bool {
        node.props.get("title") == Some(&Value::text(label))
            || node.children.iter().any(|child| holds_title(child, label))
    }

    /// Whether what `node` holds names `label` where a document is titled: its heading reads it as
    /// the title, or the window shows it under that name.
    fn titled(node: &ui::headless::InspectionNode, label: &str) -> bool {
        node.semantics.as_ref().is_some_and(|access| {
            access.value.as_deref() == Some(label) || access.name.as_deref() == Some(label)
        }) || node.children.iter().any(|child| titled(child, label))
    }

    /// Whether the window shows anything named `label` — a heading, a section or a control.
    fn shown_named(node: &ui::headless::InspectionNode, label: &str) -> bool {
        node.semantics
            .as_ref()
            .is_some_and(|access| access.answers_to(label))
            || node.children.iter().any(|child| shown_named(child, label))
    }

    /// The rows of a list or a tree, as the window reads them.
    const ROWS: &[ui::a11y::Role] = &[ui::a11y::Role::ListItem, ui::a11y::Role::TreeItem];

    /// How many nodes the window shows named `label` in one of `roles`.
    fn count_named(
        node: &ui::headless::InspectionNode,
        roles: &[ui::a11y::Role],
        label: &str,
    ) -> usize {
        let here = usize::from(
            node.semantics
                .as_ref()
                .is_some_and(|access| roles.contains(&access.role) && access.answers_to(label)),
        );
        node.children
            .iter()
            .map(|child| count_named(child, roles, label))
            .fold(here, usize::saturating_add)
    }

    /// What the window shows named `label` in one of `roles`.
    fn named<'a>(
        node: &'a ui::headless::InspectionNode,
        roles: &[ui::a11y::Role],
        label: &str,
    ) -> Option<&'a ui::headless::InspectionNode> {
        // A list row is named by its title and its secondary line; its title
        // is the line it is found by.
        let is_it = node
            .semantics
            .as_ref()
            .is_some_and(|access| roles.contains(&access.role) && access.answers_to(label));
        if is_it {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|child| named(child, roles, label))
    }

    /// The rows of the menus a row carries: the one its
    /// view places on it, and the actions the host adds to
    /// it.
    fn menu_labels(row: &tilo_app_window::UiNode) -> Vec<String> {
        fn labeled(node: &tilo_app_window::UiNode, out: &mut Vec<String>) {
            if node.kind == "ui.MenuItem"
                && let Some(Value::Text(label)) = node.props.get("label")
            {
                out.push(label.clone());
            }
            node.children.iter().for_each(|child| labeled(child, out));
        }
        fn collect(node: &tilo_app_window::UiNode, out: &mut Vec<String>) {
            if node.kind == "part" && node.props.get("name") == Some(&Value::text("menu")) {
                labeled(node, out);
            }
            node.children.iter().for_each(|child| collect(child, out));
        }
        let mut out = Vec::new();
        collect(row, &mut out);
        out
    }

    /// Whether a list shows `label` as a row held open into its item: the node the list
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

    /// A window is staged from a scenario whose changes the store derives off the frame: its frame
    /// is read only once none of it is still being read, so the same record and scenario show the
    /// same frame on every run, whatever thread finished when.
    #[test]
    fn a_staged_window_settles_on_the_workspace_before_its_frame_is_read() {
        tilo_app_window::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
        for scenario in ["a task that is late", "a busy week"] {
            let mut staged = Staged::over(
                &app,
                &packages,
                scenario,
                (320, 568),
                ui::Theme::light(),
                None,
                &[],
            )
            .expect("the scenario stages");
            assert!(
                !staged.window.instance.pending(),
                "the staged \"{scenario}\" window still reads the workspace off the frame"
            );
            staged
                .lay_out(
                    (320, 568),
                    ui::Theme::light(),
                    InteractionEnvironment::TOUCH,
                    1.5,
                )
                .expect("lays out");
            // Laid out, the frame is the person's: the store has read the record whole, nothing it
            // publishes is being read, and the window owes the app no message that would change what it
            // shows.
            assert!(
                !staged.window.instance.pending(),
                "the \"{scenario}\" window is laid out before the store read it whole"
            );
            assert!(
                !staged.window().expect("a window").owes(),
                "the \"{scenario}\" window is laid out while it still owes the app rows"
            );
            // What the frame shows is the workspace's: a workspace read so
            // far shows the platform's loading state instead of its rows.
            assert!(
                !staged.rows().is_empty(),
                "the \"{scenario}\" window shows no row of a workspace it has read"
            );
        }
    }

    /// A task's check box is a finger's width wherever a person reaches
    /// it: on the page a row opens as, and in the row that stands open
    /// where it is — at every window and text size a finger holds one
    /// in.
    #[test]
    fn a_tasks_check_box_is_a_target_at_every_layout_a_finger_reaches_it_in() {
        tilo_app_window::i18n::ensure();
        let packages = app_packages("tasks");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
        let mut short = Vec::new();
        for (width, height) in [(320_u32, 568_u32), (768, 1024)] {
            let layouts: Vec<crate::layout_check::Layout> = crate::layout_check::layouts()
                .into_iter()
                .filter(|layout| layout.size.width == width && layout.size.height == height)
                .filter(|layout| {
                    (layout.text_scale - 1.5).abs() < f32::EPSILON
                        || (layout.display_scale - 1.5).abs() < f64::EPSILON
                })
                .collect();
            let first = layouts[0];
            let mut staged = Staged::over(
                &app,
                &packages,
                "a task that is late",
                (width, height),
                first.theme(),
                None,
                &[],
            )
            .expect("the scenario stages");
            staged
                .lay_out(
                    (width, height),
                    first.theme(),
                    first.input(),
                    first.display_scale,
                )
                .expect("lays out");
            // The task the scenario answers with opens where it stands, or as a page of its own
            // from the list a finger comes back to.
            if crate::layout_check::back_chip(&staged, (width, height)) {
                staged
                    .press_shell(ui::widget::WINDOW_BACK)
                    .expect("back to the list");
            }
            staged
                .lay_out(
                    (width, height),
                    first.theme(),
                    first.input(),
                    first.display_scale,
                )
                .expect("lays out");
            if let Some(at) = crate::layout_check::tappable_row(&staged, (width, height)) {
                staged.tap_at(at).expect("the row opens");
            }
            for layout in layouts {
                staged
                    .lay_out(
                        (layout.size.width, layout.size.height),
                        layout.theme(),
                        layout.input(),
                        layout.display_scale,
                    )
                    .expect("lays out");
                let root = &staged.harness().inspection().root;
                for problem in crate::layout_check::audit::inspect(
                    root,
                    (layout.size.width, layout.size.height),
                    Some(48.0),
                    layout.display_scale,
                )
                .into_iter()
                .filter(|problem| problem.kind == crate::layout_check::Kind::SmallHitTarget)
                {
                    short.push(format!("{layout}: {} {}", problem.widget, problem.detail));
                }
            }
        }
        assert!(
            short.is_empty(),
            "a check box a finger cannot reach:\n  {}",
            short.join("\n  ")
        );
    }

    /// A service that cannot be reached says so in the page's one content
    /// gutter: the notice stands as far under its column's header row as the
    /// list under it stands below the notice, in every app and at every
    /// width.
    #[test]
    fn a_service_notice_keeps_one_gutter_over_and_under_it() {
        tilo_app_window::i18n::ensure();
        let gaps = |packages: &Rc<Packages>, scenario: &str, (width, height): (u32, u32)| {
            let id = packages.compiled.last().expect("the app").meta.app.clone();
            let theme = ui::Theme::light();
            let mut staged = Staged::over(
                &id,
                packages,
                scenario,
                (width, height),
                theme.clone(),
                None,
                &[],
            )
            .expect("the scenario stages");
            staged
                .lay_out(
                    (width, height),
                    theme,
                    ui::interaction::InteractionEnvironment::POINTER,
                    1.0,
                )
                .expect("the window lies out");
            let root = &staged.harness().inspection().root;
            let mut order = Vec::new();
            preorder(root, &mut order);
            let at = order
                .iter()
                .position(|node| node.widget == "Banner")
                .expect("the notice shows");
            let notice = order[at];
            // The notice stands under the header row of the column it is in.
            let top = staged
                .harness()
                .column_frames()
                .iter()
                .find(|frame| {
                    frame.column.x0 <= notice.rect[0] + 0.5 && notice.rect[0] < frame.column.x1
                })
                .map_or(0.0, |frame| frame.header.y1);
            // What stands under the notice: the first box after it that starts below it.
            let below = order[at..]
                .iter()
                .find(|node| {
                    let [_, y, w, h] = node.rect;
                    w > 0.0 && h > 0.0 && y >= notice.rect[1] + notice.rect[3] - 0.5
                })
                .map(|node| node.rect[1] - (notice.rect[1] + notice.rect[3]))
                .expect("the list under the notice stands");
            (notice.rect[1] - top, below)
        };
        let gutter = f64::from(ui::Theme::light().tokens.spacing.rhythm.group);
        // Each app is built once, and staged from that build at both sizes.
        let apps = [
            ("mail", "an account that cannot be reached"),
            ("tube", "YouTube cannot be reached"),
        ]
        .map(|(app, scenario)| (app, scenario, app_packages(app)));
        for size in [(390, 844), (1280, 800)] {
            for &(app, scenario, ref packages) in &apps {
                let (over, under): (f64, f64) = gaps(packages, scenario, size);
                assert!(
                    (over - gutter).abs() < 0.5,
                    "{app} at {size:?}: {over} over the notice, where the page's gutter is {gutter}"
                );
                assert!(
                    (under - gutter).abs() < 0.5,
                    "{app} at {size:?}: {under} under the notice, where the page's gutter is {gutter}"
                );
            }
        }
    }

    /// The gallery is one window: each group stands in its sidebar, choosing one shows that
    /// group's page in the content beside it, and the window opens on a page rather than on
    /// nothing.
    #[test]
    fn the_gallery_shows_each_group_from_its_sidebar() {
        use tilo_render_protocol::patch::Level;
        use tilolang_tilo::runtime::{UiNode, Value};

        fn find_key(node: &UiNode, place: &str) -> Option<String> {
            if node.kind == "ui.NavItem" && node.props.get("label") == Some(&Value::text(place)) {
                return Some(node.key.clone());
            }
            node.children
                .iter()
                .find_map(|child| find_key(child, place))
        }

        fn shows(node: &UiNode, kind: &str) -> bool {
            node.kind == kind || node.children.iter().any(|child| shows(child, kind))
        }

        tilo_app_window::i18n::ensure();
        ui::clock::freeze_for_capture();
        let packages = app_packages("gallery");
        let (width, height) = (1280, 800);
        let mut staged = Staged::over(
            "so.tilo.lang.gallery",
            &packages,
            "every family",
            (width, height),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the gallery stages");
        staged
            .lay_out(
                (width, height),
                ui::Theme::light(),
                ui::interaction::InteractionEnvironment::POINTER,
                1.0,
            )
            .expect("the gallery lays out");
        // It opens on a page with content: the first group, Basics, rather than an empty one.
        let tree = staged
            .window
            .instance
            .view(Level::Root)
            .expect("the gallery's root view");
        assert!(
            shows(tree.borrow().tree(), "ui.Card"),
            "the window opens on a page: {:#?}",
            tree.borrow().tree()
        );
        for (place, marker) in [
            ("Basics", "ui.Card"),
            ("Controls", "ui.NumberInput"),
            ("Feedback", "ui.Banner"),
            ("Backlog", "ui.Docked"),
            ("Release board", "ui.Board"),
            ("Data", "ui.DataTable"),
            ("Media", "ui.MediaGrid"),
            ("Places", "ui.Map"),
            ("Shell parts", "ui.TabStrip"),
            ("Charts and views", "ui.Chart"),
        ] {
            let key = {
                let tree = staged
                    .window
                    .instance
                    .view(Level::Root)
                    .expect("the gallery's root view");
                let tree = tree.borrow();
                find_key(tree.tree(), place)
                    .unwrap_or_else(|| panic!("the sidebar names {place}: {:#?}", tree.tree()))
            };
            let view = staged
                .window
                .instance
                .view(Level::Root)
                .expect("the gallery's root view");
            view.borrow()
                .fire(&key, "select")
                .unwrap_or_else(|error| panic!("choosing {place}: {error}"));
            view.borrow_mut()
                .poll_patches()
                .expect("chosen place answers");
            drop(view);
            staged.exchange();
            staged
                .lay_out(
                    (width, height),
                    ui::Theme::light(),
                    ui::interaction::InteractionEnvironment::POINTER,
                    1.0,
                )
                .expect("the chosen page lays out");
            let tree = staged
                .window
                .instance
                .view(Level::Root)
                .expect("the gallery's root view");
            let tree = tree.borrow();
            assert!(
                shows(tree.tree(), marker),
                "choosing {place} shows its page, with a {marker} on it: {:#?}",
                tree.tree()
            );
        }
    }
}

#[cfg(test)]
mod maps_layout {
    use super::*;

    const SIZE: (u32, u32) = (1280, 800);

    /// The Maps package, compiled from `apps/tl/maps`, and the app's id.
    fn maps_packages() -> (Rc<Packages>, String) {
        tilo_app_window::i18n::ensure();
        let packages = super::tests::app_packages("maps");
        let app = packages.compiled.last().expect("the app").meta.app.clone();
        (packages, app)
    }

    /// The Maps window at a laptop's size, laid out as a person's window is, with the view `states`
    /// staged as the layout check's `--state` stages them.
    fn maps(packages: &Rc<Packages>, app: &str, states: &[&str]) -> Staged {
        let theme = ui::Theme::light();
        let mut staged = Staged::over_workspace(
            app,
            packages,
            SIZE,
            theme.clone(),
            Staging {
                states: states.iter().map(|state| (*state).to_owned()).collect(),
                ..Staging::default()
            },
        )
        .expect("the map opens");
        staged
            .lay_out(
                SIZE,
                theme,
                ui::interaction::InteractionEnvironment::POINTER,
                1.0,
            )
            .expect("the map lies out");
        staged
    }

    /// The path from `node` to the first node named `name`.
    fn path_to<'a>(
        node: &'a ui::headless::InspectionNode,
        name: &str,
        path: &mut Vec<&'a ui::headless::InspectionNode>,
    ) -> bool {
        path.push(node);
        if node
            .semantics
            .as_ref()
            .is_some_and(|access| access.answers_to(name))
        {
            return true;
        }
        for child in &node.children {
            if path_to(child, name, path) {
                return true;
            }
        }
        path.pop();
        false
    }

    /// The rect of the node named `name`, and of the stack that holds it: the layer a view placed
    /// there. Each rect is `[x, y, width, height]`, as the toolkit's inspection reads it.
    fn holding(root: &ui::headless::InspectionNode, name: &str) -> ([f64; 4], [f64; 4]) {
        let mut path = Vec::new();
        assert!(path_to(root, name, &mut path), "the window shows {name:?}");
        let shown = path.last().expect("the named node").rect;
        let layer = path
            .iter()
            .rev()
            .skip(1)
            .find(|node| node.test_id.is_some() && node.widget == "Stack")
            .unwrap_or_else(|| panic!("a stack holds {name:?}"))
            .rect;
        (layer, shown)
    }

    /// The rect of the first widget of kind `widget`.
    fn widget(root: &ui::headless::InspectionNode, widget: &str) -> [f64; 4] {
        root.nodes()
            .into_iter()
            .find(|node| node.widget == widget)
            .unwrap_or_else(|| panic!("the window draws a {widget}"))
            .rect
    }

    /// Where a rect `[x, y, width, height]` ends on either axis.
    fn right(rect: [f64; 4]) -> f64 {
        rect[0] + rect[2]
    }

    fn bottom(rect: [f64; 4]) -> f64 {
        rect[1] + rect[3]
    }

    /// Whether two edges stand at the same place: layout arithmetic is in fractions of a pixel, so
    /// the same edge reads within half a one.
    fn same(a: f64, b: f64) -> bool {
        (a - b).abs() <= 0.5
    }

    /// Every part of the map's own layers stands where it says: the map fills them, the controls
    /// stand on its trailing edge and the footer on its bottom edge, across its width. A view call
    /// the layers held as one box of its own left the footer's rows at the map's top leading
    /// corner, over the map, rather than under it.
    #[test]
    fn maps_places_its_map_controls_and_footer_on_their_layers() {
        let (packages, app) = maps_packages();
        let staged = maps(&packages, &app, &[]);
        let root = &staged.harness().inspection().root;
        let map = widget(root, "Map");
        let (controls, zoom_in) = holding(root, "Zoom in");
        let (footer, copyright) = holding(root, "© OpenStreetMap contributors");

        assert!(
            same(right(controls), right(map)),
            "the controls stand on the map's trailing edge: {controls:?} in {map:?}"
        );
        assert!(
            same(controls[1] + controls[3] / 2.0, map[1] + map[3] / 2.0),
            "the controls stand at the map's middle: {controls:?} in {map:?}"
        );
        assert!(
            zoom_in[0] >= controls[0] && right(zoom_in) <= right(controls),
            "the zoom control stands in the controls' stack: {zoom_in:?} in {controls:?}"
        );
        assert!(
            same(bottom(footer), bottom(map)),
            "the footer stands on the map's bottom edge: {footer:?} in {map:?}"
        );
        assert!(
            same(footer[0], map[0]) && same(footer[2], map[2]),
            "the footer spans the map: {footer:?} in {map:?}"
        );
        assert!(
            footer[1] > map[1] + map[3] / 2.0,
            "the footer stands in the map's lower half: {footer:?} in {map:?}"
        );
        assert!(
            copyright[1] > map[1] + map[3] / 2.0,
            "the footer's words stand in the map's lower half: {copyright:?} in {map:?}"
        );
    }

    /// What a view call under the map's layers opens over it stands where its layer says: the
    /// status line across the map's top when a control the person presses puts one there, and the
    /// offline sheet and the attribution dialog over the map rather than beyond the window.
    #[test]
    fn maps_places_the_status_line_and_the_sheets_over_the_map() {
        let (packages, app) = maps_packages();
        let (map, status, window) = {
            let mut staged = maps(&packages, &app, &[]);
            let id = crate::layout_check::button_named(
                &staged.harness().inspection().root,
                "Use my location",
            )
            .expect("the map offers the location control");
            staged
                .press(&id, false)
                .expect("the location control presses");
            staged.freshen().expect("the map comes to rest");
            let root = &staged.harness().inspection().root;
            (
                widget(root, "Map"),
                holding(root, "Dismiss").0,
                widget(root, "TilolangWindow"),
            )
        };
        assert!(
            same(status[0], map[0]) && same(status[1], map[1]) && same(status[2], map[2]),
            "the status line stands across the map's top: {status:?} in {map:?}"
        );

        // A sheet the map opens in its outer layers stands in the window, drawn over the map.
        for (state, drawn) in [
            ("sheet=offline", "FloatingPanel"),
            ("sheet=info", "ModalDialog"),
        ] {
            let staged = maps(&packages, &app, &[state]);
            let root = &staged.harness().inspection().root;
            let sheet = widget(root, drawn);
            assert!(
                sheet[2] > 0.0 && sheet[3] > 0.0,
                "the {drawn} {state} is drawn: {sheet:?}"
            );
            assert!(
                sheet[0] >= window[0]
                    && sheet[1] >= window[1]
                    && right(sheet) <= right(window)
                    && bottom(sheet) <= bottom(window),
                "the {drawn} {state} stands in the window: {sheet:?} in {window:?}"
            );
        }
    }
}
