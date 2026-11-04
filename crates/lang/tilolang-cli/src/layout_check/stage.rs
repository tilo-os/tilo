//! An app's window as the UI host shows it, with nothing on screen: its instance over a workspace of
//! its own, staged with a scenario, sending its views over the patch protocol to the UI host's
//! half of the window (`tilo_ui_host::AppWindow`), which the toolkit's frame pipeline lays out
//! (`ui::headless::FrameHarness`). Nothing is rasterised and no GPU is opened.

use std::rc::Rc;
use std::sync::Arc;

use tilo_render_protocol::patch::local::pair;
use tilo_render_protocol::patch::{ResourceRef, ToApp, Value};
use tilo_ui_host::AppWindow;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::tilolang::Compiled;
// `DynAction::apply` is the action protocol's.
use ui::headless::FrameHarness;
use ui::interaction::InteractionEnvironment;
use ui::tilo_actions::Action as _;

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
    /// the package ships it, the package's source language otherwise.
    fn add_words(&self, catalogues: &[(String, String)]) {
        ui::i18n::speak_window_language(catalogues);
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
        ui::clock::local_now().offset().local_minus_utc()
    }

    fn zone_name(&self) -> Option<String> {
        ui::time::local_zone_name()
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        Arc::new(|| {})
    }
}

/// One window of the app, laid out by one frame pipeline that every configuration reuses.
pub struct Staged {
    instance: Instance,
    harness: FrameHarness,
}

/// How often the window and its instance may answer each other before a frame is taken as it
/// is: the rows a list shows, then the rows those rows show.
const EXCHANGES: usize = 8;

impl Staged {
    /// The app `app` of `packages` with `scenario` staged, its window laid out at `viewport`,
    /// showing the layer `root` names rather than the package's first root: a package with more
    /// than one root (the shell's `Desk` beside its `Arrangement`) shows each, and each is laid
    /// out as a person sees it.
    pub fn over(
        app: &str,
        packages: &Rc<Packages>,
        scenario: &str,
        viewport: (u32, u32),
        mut theme: ui::Theme,
        root: Option<&str>,
    ) -> Result<Self, String> {
        // Layout at rest: what changes land at once rather than moving there.
        theme.reduce_motion = true;
        let (app_end, window_end) = pair();
        let mut instance = Instance::new(
            Config {
                app: app.to_owned(),
                me: "ada".to_owned(),
                mode: Mode::Live,
                device: None,
                index_dir: None,
                record_loader: None,
                root: tilolang_tilo::host::RootView {
                    root: root.map(str::to_owned),
                    view: None,
                },
                sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            },
            Rc::clone(packages) as Rc<dyn Platform>,
            // People are called as a window calls them when nobody told it their names: a
            // scenario's sample person (`person("Leo")`) by that name, never by their id.
            Rc::new(tilolang_tilo::names::Unnamed),
            Box::new(app_end),
        );
        let mut record = ui::automerge::AutoCommit::new();

        instance.sync(Some(&record));
        // The package's own root resources — every layer it has (the shell's `Desk` beside its
        // arrangement) and a person's record kept beside them (`root shell: Shell`) — are made
        // and its migrations run, as the live session does when a window opens: a scenario
        // that seeds another layer than the one laid out, or seeds a root through a migration,
        // shows them. Making a root queues a change; `sync` after the origin's say-so runs the
        // migrations, and each queued change is committed into the record as the door commits
        // an app's.
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
        let settle = |instance: &mut Instance, record: &mut ui::automerge::AutoCommit| {
            instance.sync(Some(record));
            instance.commit_planned();
            if let Some(host) = instance.host() {
                for committed in host.take_committed() {
                    committed
                        .action
                        .apply(record)
                        .map_err(|error| format!("a change could not be committed: {error}"))?;
                }
            }
            record.commit();
            instance.sync(Some(record));
            Ok::<(), String>(())
        };
        settle(&mut instance, &mut record)?;
        instance.stage(Some(scenario));
        settle(&mut instance, &mut record)?;
        if let Some(why) = instance.take_stage_failure() {
            return Err(format!("the scenario \"{scenario}\" failed: {why}"));
        }
        instance.update();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        window.receive();
        if let Some(why) = window.failed() {
            return Err(format!("the window refused what the app sent: {why}"));
        }
        let harness =
            FrameHarness::new_with_theme(ui::State::new(), Box::new(window), viewport, theme);
        let mut staged = Self { instance, harness };
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
            .relayout_themed(viewport, theme)
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

    pub fn harness(&self) -> &FrameHarness {
        &self.harness
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
                if let (Some(item_ty), Some(tilolang_tilo::DynValue::Collection(items))) =
                    (&field.record, record.get(&field.id))
                {
                    out.extend(items.ids().map(|id| ResourceRef {
                        id: format!("{doc}/{}", id.as_str()),
                        ty: item_ty.clone(),
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
            .relayout_themed(viewport, theme)
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

    /// Run the toolkit's clock until nothing in the window moves.
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
