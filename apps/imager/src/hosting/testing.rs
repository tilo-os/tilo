//! The installer's windows as a test drives them: built as a run builds them, drawn headless,
//! and invoked as a console caller invokes them.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_resource::Effect;
use tilo_ui_base::widget::Widget;
use ui::headless::Driver;
use ui::tilo_actions::{ActionAddress, ActionOutput, ApiError, ResourceSpec};

use super::effect::Answer;
use super::{Flow, Opened, Reply, SHOT_VIEWPORT, Store, invoke, published, scenario_store, window};
use crate::resource::Imager;

/// How long a test waits for an effect whose worker answers later.
const ANSWER_BOUND: std::time::Duration = std::time::Duration::from_secs(60);

/// One window of the installer, run headless.
pub(crate) struct TestHost {
    store: Store,
    harness: Driver,
}

impl TestHost {
    /// The window a launch with `arguments` opens: `--setup`, `--demo`, `--destination`.
    #[cfg(any(feature = "installation", feature = "destination"))]
    pub(crate) fn launched(arguments: &[&str]) -> Self {
        let has = |flag: &str| arguments.contains(&flag);
        #[allow(
            unused_mut,
            reason = "the image writer alone builds without the other flows"
        )]
        let mut flow = Flow::Wizard;
        #[cfg(feature = "installation")]
        if has("--setup") || has("--demo") {
            flow = Flow::Setup;
        }
        #[cfg(feature = "destination")]
        if has("--destination") {
            flow = Flow::Destination;
        }
        let store = Store::in_memory();
        let root = window(
            flow,
            &store,
            &ui::WakeHandle::new(),
            Opened::Live {
                demo: has("--demo"),
                restore: None,
            },
        );
        Self::run(store, root, SHOT_VIEWPORT)
    }

    /// The window a shot of `scenario` draws.
    pub(crate) fn capture(scenario: Option<&str>) -> Self {
        Self::capture_at(scenario, SHOT_VIEWPORT)
    }

    /// [`Self::capture`] at `viewport`.
    pub(crate) fn capture_at(scenario: Option<&str>, viewport: (u32, u32)) -> Self {
        let store = scenario_store(scenario);
        let root = window(
            Flow::of_scenario(scenario),
            &store,
            &ui::WakeHandle::new(),
            Opened::Shot(scenario),
        );
        Self::run(store, root, viewport)
    }

    /// A window `build` makes on a fresh resource, opened as a person opens it.
    pub(crate) fn hosting(build: impl FnOnce(&Store, &ui::WakeHandle) -> Box<dyn Widget>) -> Self {
        let store = Store::in_memory();
        let root = build(&store, &ui::WakeHandle::new());
        Self::run(store, root, SHOT_VIEWPORT)
    }

    /// `root`, shown on `store`.
    pub(crate) fn run(store: Store, root: Box<dyn Widget>, viewport: (u32, u32)) -> Self {
        let mut host = Self {
            store,
            harness: Driver::new(ui::State::new(), root, viewport),
        };
        host.settle();
        host
    }

    pub(crate) fn harness(&mut self) -> &mut Driver {
        &mut self.harness
    }

    /// The resource as the windows left it.
    pub(crate) fn state(&self) -> Imager {
        self.store.state()
    }

    /// Run the frame the window runs after an event: what its controls pressed and its
    /// workers finished taken up, and drawn.
    pub(crate) fn settle(&mut self) {
        tilo_ui_base::component_inputs::forcing(|| {
            self.harness.wait_until(ui::time::Duration::ZERO, |_| true);
        });
    }

    /// Visit the first widget of type `T` in the window, a hosted window's own included, then
    /// run the frame after it.
    pub(crate) fn with_widget<T: Widget + 'static, O>(
        &mut self,
        visit: impl FnOnce(&mut T) -> O,
    ) -> Option<O> {
        let output = find::<T>(self.harness.root_widget_mut()).map(visit);
        self.settle();
        output
    }

    /// The first widget of type `T` in the window, a hosted window's own included.
    pub(crate) fn surface_mut<T: Widget + 'static>(&mut self) -> &mut T {
        find::<T>(self.harness.root_widget_mut()).expect("the window shows this widget")
    }

    /// The resource every window publishes now, as a console caller reads it.
    pub(crate) fn published(&mut self) -> ResourceSpec {
        published(&self.store, self.harness.root_widget_mut())
    }

    /// Invoke `effect` on the resource, as a console caller does, and wait for its answer.
    ///
    /// # Errors
    ///
    /// Why it was not admitted, or why it failed.
    pub(crate) fn effect<E: Effect>(&mut self, effect: &E) -> Result<ActionOutput, ApiError> {
        let resources = self.resources();
        let resource = resources
            .iter()
            .find(|resource| resource.actions.iter().any(|spec| spec.id == effect.id()))
            .map_or_else(
                || resources[0].id.0.clone(),
                |resource| resource.id.0.clone(),
            );
        self.address(&resource, effect.id(), &effect.arguments(), false)
    }

    /// [`Self::effect`] after the caller confirmed it.
    ///
    /// # Errors
    ///
    /// As [`Self::effect`].
    pub(crate) fn effect_on_confirmed<E: Effect>(
        &mut self,
        row: &str,
        effect: &E,
    ) -> Result<ActionOutput, ApiError> {
        self.address(row, effect.id(), &serde_json::Map::new(), true)
    }

    /// Call the resource's own action `id` with `arguments`, as a console caller does.
    ///
    /// # Errors
    ///
    /// As [`Self::effect`].
    pub(crate) fn call(
        &mut self,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<ActionOutput, ApiError> {
        let resource = self.resources()[0].id.0.clone();
        self.call_on(&resource, id, arguments)
    }

    /// Call the action `id` of the published resource `resource`.
    ///
    /// # Errors
    ///
    /// As [`Self::effect`].
    pub(crate) fn call_on(
        &mut self,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<ActionOutput, ApiError> {
        self.address(resource, id, &object(arguments), false)
    }

    /// [`Self::call_on`] after the caller confirmed it, as the person does on this computer.
    ///
    /// # Errors
    ///
    /// As [`Self::effect`].
    #[cfg(feature = "destination")]
    pub(crate) fn call_confirmed_on(
        &mut self,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<ActionOutput, ApiError> {
        self.address(resource, id, &object(arguments), true)
    }

    /// Read the stored document itself.
    pub(crate) fn read_document<T>(&self, read: impl FnOnce(&tilo_resource::Doc) -> T) -> T {
        self.store.read_document(read)
    }

    /// Merge an edit another member made of the resource, as sync delivers it, and show it.
    pub(crate) fn merge_peer_edit(&mut self, edit: impl FnOnce(&mut tilo_resource::Doc)) {
        self.store.merge_peer_edit(edit);
        self.settle();
    }

    /// Invoke `effect` on the published row `row`.
    ///
    /// # Errors
    ///
    /// As [`Self::effect`].
    #[cfg(feature = "destination")]
    pub(crate) fn effect_on<E: Effect>(
        &mut self,
        row: &str,
        effect: &E,
    ) -> Result<ActionOutput, ApiError> {
        self.address(row, effect.id(), &serde_json::Map::new(), false)
    }

    /// Run one of the resource's own actions, as a control or a console caller does.
    ///
    /// # Errors
    ///
    /// Why the resource refused it.
    pub(crate) fn invoke(
        &mut self,
        action: impl Into<tilo_resource::Invoke<Imager>>,
    ) -> Result<(), String> {
        self.store.apply(action)?;
        self.settle();
        Ok(())
    }

    /// Every resource the window publishes now: the installer's on the live medium, or the one
    /// every other window publishes together.
    pub(crate) fn resources(&mut self) -> Vec<ResourceSpec> {
        #[cfg(feature = "destination")]
        if let Some(app) = self
            .harness
            .widget_mut::<crate::setup::destination::InstallerApp>()
        {
            return console_control::ConsoleApp::published(
                &crate::setup::destination::installer_app::console::Console(app),
            );
        }
        vec![self.published()]
    }

    fn address(
        &mut self,
        resource: &str,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<ActionOutput, ApiError> {
        let address = ActionAddress::new(resource, id);
        let resources = self.resources();
        let confirmation = confirmed
            .then(|| ui::tilo_actions::api::resolve(&resources, &address))
            .flatten()
            .map(|spec| match spec.confirmation_for_json(arguments) {
                ui::tilo_actions::ConfirmationPolicy::Phrase { phrase } => {
                    ui::tilo_actions::remote::Confirmation::Phrase(phrase)
                }
                _ => ui::tilo_actions::remote::Confirmation::Confirmed,
            });
        let encoded =
            ui::tilo_actions::api::admit(&resources, &address, arguments, confirmation.as_ref())?;
        #[cfg(feature = "destination")]
        if let Some(app) = self
            .harness
            .widget_mut::<crate::setup::destination::InstallerApp>()
        {
            let answer = app.apply_encoded(&encoded).map_err(ApiError::Failed);
            self.settle();
            return answer;
        }
        let answer = Rc::new(RefCell::new(None));
        let reply = Reply::new(Answer::Test(Rc::clone(&answer)));
        invoke(&self.store, self.harness.root_widget_mut(), &encoded, reply);
        let started = std::time::Instant::now();
        loop {
            self.settle();
            if let Some(answer) = answer.borrow_mut().take() {
                return answer.map_err(ApiError::Failed);
            }
            assert!(
                started.elapsed() < ANSWER_BOUND,
                "`{address:?}` did not answer within {ANSWER_BOUND:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

fn object(arguments: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match arguments {
        serde_json::Value::Object(arguments) => arguments,
        serde_json::Value::Null => serde_json::Map::new(),
        other => panic!("an action's arguments are an object, not {other}"),
    }
}

/// The first widget of type `T` under `widget`, looking into the windows it hosts.
pub(crate) fn find<T: 'static>(widget: &mut dyn Widget) -> Option<&mut T> {
    if widget.as_any_mut().is_some_and(|any| any.is::<T>()) {
        return widget.as_any_mut()?.downcast_mut();
    }
    if tilo_ui_base::widget::capability::capability_mut::<dyn super::Served>(widget)
        .is_some_and(|served| served.window_mut().is::<T>())
    {
        return tilo_ui_base::widget::capability::capability_mut::<dyn super::Served>(widget)?
            .window_mut()
            .downcast_mut();
    }
    widget.children_mut().into_iter().find_map(find::<T>)
}
