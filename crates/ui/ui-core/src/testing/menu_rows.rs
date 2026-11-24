//! A view whose record a menu's rows act on, for this crate's own menu tests: it publishes one
//! effect action per label and records each the window runs, as an application's `on_action`
//! does. A right-click changes it, as a canvas picking the mark under it does, so the next frame
//! rebuilds the tree.

use std::cell::RefCell;
use std::rc::Rc;

use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::widget::{DrawChild, ResourceOperation, ResourceOperationStart, Spacer, Widget};

/// The record the rows act on.
pub(crate) const MENU_ROW_RESOURCE: &str = "test:menu";
/// The operation a row's action is.
pub(crate) const MENU_ROW_TYPE: &str = "test.menu.row";

pub(crate) struct MenuRows {
    inner: Spacer,
    labels: &'static [&'static str],
    ran: Rc<RefCell<Vec<String>>>,
    picked: bool,
}

impl MenuRows {
    /// A view publishing one action per label of `labels`, recording the ones it runs in `ran`.
    pub(crate) fn new(labels: &'static [&'static str], ran: Rc<RefCell<Vec<String>>>) -> Self {
        Self {
            inner: Spacer::fixed(300.0),
            labels,
            ran,
            picked: false,
        }
    }

    /// The action the row labelled `label` runs.
    pub(crate) fn action_of(label: &str) -> String {
        label.to_lowercase()
    }
}

impl Widget for MenuRows {
    crate::provides!(
        dyn crate::resources::PublishesResources,
        dyn crate::widget::ResourceOperations
    );

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.inner.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.inner.apply_layout(taffy, node, origin);
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.inner
            .draw_child(scene, transform, text_renderer, theme, &env);
    }

    fn handle_event(&mut self, event: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            }
        ) {
            self.picked = true;
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.picked)
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }

    fn name(&self) -> &'static str {
        "MenuRows"
    }
}

impl crate::resources::PublishesResources for MenuRows {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        let mut record = tilo_actions::ResourceSpec::new(MENU_ROW_RESOURCE, "test.menu", "Menu");
        for (index, label) in self.labels.iter().enumerate() {
            record = record.with_action(
                tilo_actions::ActionSpec::operation(
                    Self::action_of(label),
                    *label,
                    tilo_actions::OperationKind::Effect,
                    MENU_ROW_TYPE,
                    &index,
                )
                .expect("a test action"),
            );
        }
        vec![record]
    }
}

impl crate::widget::ResourceOperations for MenuRows {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[MENU_ROW_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let request = op.request();
        assert_eq!(request.kind, tilo_actions::OperationKind::Effect);
        assert_eq!(request.type_id, MENU_ROW_TYPE);
        let index: usize = serde_json::from_slice(request.payload)
            .map_err(|error| format!("the row's action: {error}"))?;
        let label = self
            .labels
            .get(index)
            .ok_or_else(|| format!("no row at {index}"))?;
        self.ran.borrow_mut().push((*label).to_owned());
        Ok(Some(ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

/// A press bound to the published action `action` of `resource`, as a view binds a control or a
/// menu row: what choosing it runs is that action, at its address.
pub(crate) fn bound_press(resource: &str, action: &str) -> crate::widget::Press {
    let spec = tilo_actions::ResourceSpec::new(resource, "test.bound", resource).with_action(
        tilo_actions::ActionSpec::operation(
            action,
            action,
            tilo_actions::OperationKind::Effect,
            "test.bound.effect",
            &action,
        )
        .expect("a test action"),
    );
    let binding = tilo_actions::ActionBinding::unit_for(&spec, action)
        .expect("the resource publishes the action the press names");
    crate::widget::Press::of(crate::component::Event::Action(binding))
}
