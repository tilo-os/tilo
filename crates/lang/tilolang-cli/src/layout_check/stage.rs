//! An app's window as the UI host shows it, with nothing on screen: its instance over a room of
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
use tilolang_tilo::runtime::{NameHost, Principal};
use tilolang_tilo::tilolang::Compiled;
use ui::headless::FrameHarness;
use ui::interaction::InteractionEnvironment;

/// The compiled packages an instance loads: the app and what it uses, the app last.
pub struct Packages(pub Rc<Vec<Compiled>>);

impl Platform for Packages {
    fn load(&self, host: &Host) -> u64 {
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
            .map(|(name, value)| (name.as_str(), value.as_str().into()))
            .collect();
        tilo_ui_host::i18n::t_args(id, &args)
    }

    /// A fixed instant, so every run lays out the same dates.
    fn now(&self) -> String {
        "2026-10-27T10:00:00Z".to_owned()
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        Arc::new(|| {})
    }
}

/// A room nobody else is in.
struct Alone;

impl NameHost for Alone {
    fn name(&self, who: &Principal) -> String {
        who.0.clone()
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
    /// The app `app` of `packages` with `scenario` staged, its window laid out at `viewport`.
    pub fn new(
        app: &str,
        packages: &Rc<Packages>,
        scenario: &str,
        viewport: (u32, u32),
        mut theme: ui::Theme,
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
                sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            },
            Rc::clone(packages) as Rc<dyn Platform>,
            Rc::new(Alone),
            Box::new(app_end),
        );
        let record = ui::automerge::AutoCommit::new();
        instance.sync(Some(&record));
        instance.stage(Some(scenario));
        instance.sync(Some(&record));
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
        self.settle()
    }

    pub fn harness(&self) -> &FrameHarness {
        &self.harness
    }

    pub fn harness_mut(&mut self) -> &mut FrameHarness {
        &mut self.harness
    }

    /// The room's records and the items of their collections, as references.
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
            let (Some(schema), Ok(record)) = (
                tilolang_tilo::schema::lookup(&ty),
                host.store().record(&doc),
            ) else {
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

    /// Every resource the room holds, the window's views and the rows of their lists name.
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
