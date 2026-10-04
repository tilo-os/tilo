//! The app's widget: hosts the page, drives the measurement and answers the
//! operations the resource publishes.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use ui::automerge::{AutoCommit, ObjId};
use ui::presence::PresenceCtx;
use ui::taffy::{NodeId, TaffyTree};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{
    ActionContract, ActionOutput, ActionSpec, DiscoveryScope, OperationKind, Resource,
};
use ui::time::{Duration, Instant};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::widget::{AppModel, Element, ModelPath, Widget};
use ui::winit::event::WindowEvent;

use crate::model::Model;
use crate::view::{self, ScrollOffset, Shared};

pub(crate) const OPERATION_TYPE: &str = "speed-test.operation";

/// The longest wait between frames while the dial is moving.
const FRAME: Duration = Duration::from_millis(16);

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpeedOperation {
    Start,
    Stop,
}

pub(crate) fn action(id: &str, label: &str, op: &SpeedOperation) -> Option<ActionSpec> {
    let mut spec =
        ActionSpec::operation(id, label, OperationKind::Effect, OPERATION_TYPE, op).ok()?;
    spec.contract = ActionContract {
        discovery: DiscoveryScope::Launchable,
        ..ActionContract::default()
    };
    Some(spec)
}

pub struct SpeedTestApp {
    shared: Shared,
    scroll: ScrollOffset,
    /// The page for the model's current shape; built with the theme at
    /// layout time and again whenever the shape changes.
    root: Option<Element>,
    built: Option<u64>,
    structure_dirty: bool,
}

impl SpeedTestApp {
    pub fn new() -> Self {
        Self {
            shared: Rc::new(RefCell::new(Model::new())),
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            root: None,
            built: None,
            structure_dirty: false,
        }
    }

    pub(crate) fn model(&self) -> std::cell::Ref<'_, Model> {
        self.shared.borrow()
    }

    pub(crate) fn model_mut(&self) -> std::cell::RefMut<'_, Model> {
        self.shared.borrow_mut()
    }

    fn answer_operation(&self, op: &SpeedOperation) {
        match op {
            SpeedOperation::Start => self.model_mut().start(),
            SpeedOperation::Stop => self.model_mut().stop(),
        }
    }

    /// Stop while a measurement is running; not a second `speed-test` root.
    fn live_run_extra(&self) -> Vec<Resource> {
        if !self.model().is_running() {
            return Vec::new();
        }
        let mut run = Resource::new("speed-test.run", "speed-test.run", "Running test");
        if let Some(spec) = action("stop", "Stop speed test", &SpeedOperation::Stop) {
            run.actions.push(spec);
        }
        vec![run]
    }
}

impl Default for SpeedTestApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SpeedTestApp {
    ui::provides!(dyn ui::resources::PublishesResources);

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        if let Some(root) = &mut self.root {
            root.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let mut changed = {
            let mut model = self.shared.borrow_mut();
            let polled = model.poll();
            polled | model.tick(ui::time::now())
        };
        if self.built != Some(self.shared.borrow().revision()) {
            self.structure_dirty = true;
            changed = true;
        }
        if let Some(root) = &mut self.root {
            changed |= root.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let revision = self.shared.borrow().revision();
        if self.built != Some(revision) {
            self.root = None;
            self.built = Some(revision);
        }
        let (shared, scroll) = (&self.shared, &self.scroll);
        self.root
            .get_or_insert_with(|| view::build(shared, scroll, theme))
            .build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Some(root) = &mut self.root {
            root.apply_layout(taffy, node, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(root) = &mut self.root {
            root.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(root) = &mut self.root {
            root.handle_event(event, ctx);
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|root| root.wants_pointer_capture())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let inner = self
            .root
            .as_mut()
            .is_some_and(|root| root.take_structure_dirty());
        own | inner
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if request.kind == OperationKind::Document || request.type_id != OPERATION_TYPE {
            return Ok(None);
        }
        let operation = serde_json::from_slice::<SpeedOperation>(request.payload)
            .map_err(|error| error.to_string())?;
        self.answer_operation(&operation);
        Ok(Some(ui::widget::ResourceOperationStart::Completed(
            ActionOutput::default(),
        )))
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.model().is_animating().then(|| ui::time::now() + FRAME)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.root.iter().map(AsRef::as_ref).collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.root
            .iter_mut()
            .map(|root| root.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn name(&self) -> &'static str {
        "SpeedTestApp"
    }

    fn get_rect(&self) -> Rect {
        self.root
            .as_ref()
            .map_or(Rect::ZERO, |root| root.get_rect())
    }
}

impl ui::resources::PublishesResources for SpeedTestApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        self.live_run_extra()
    }

    fn resource(&self, _doc: &AutoCommit, id: &str) -> Option<Resource> {
        self.live_run_extra()
            .into_iter()
            .find(|resource| resource.id.as_str() == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::resource::{SpeedTest, SpeedTestState};

    fn published() -> Vec<Resource> {
        vec![ui::tilo_resource::publish::<SpeedTest>(
            "speed-test:root",
            "Internet speed test",
            &SpeedTestState,
        )]
    }

    // Speed Test keeps no document: it publishes only effect operations, so
    // the contract is the surface sweep (protocol validation) and Ctrl+K.
    ui::action_contract_tests! {
        seed: AutoCommit::new(),
        registry: ui::tilo_actions::ActionRegistry::new(),
        resources: |_| published(),
        palette: ["Run speed test"],
    }

    #[test]
    fn the_run_is_published_only_while_it_runs() {
        let app = SpeedTestApp::new();
        let doc = AutoCommit::new();
        assert!(ui::resources::resources(&app, &doc).is_empty());

        app.model_mut().stage_scenario(Some("running"));
        let extras = ui::resources::resources(&app, &doc);
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].id.as_str(), "speed-test.run");
        assert!(
            extras[0]
                .actions
                .iter()
                .any(|action| action.id.as_str() == "stop")
        );
        assert!(ui::resources::resource(&app, &doc, "speed-test:root").is_none());
    }
}
