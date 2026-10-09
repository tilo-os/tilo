//! Toolkit-owned host for a small app's typed resource/action surface.
//!
//! Apps provide their retained root, one discoverable resource, and an
//! immediate operation closure. The host supplies the widget
//! forwarding and invocation lifecycle; scaffolds do not need to copy that
//! infrastructure into every new project.
use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use crate::automerge::{AutoCommit, ObjId};
use crate::kurbo::{Affine, Point, Rect};
use crate::taffy::{NodeId, TaffyTree};
use crate::tilo_actions::{
    ActionBinding, ActionBindingError, ActionContract, ActionOutput, ActionParam, ActionSpec,
    EncodedAction, ExecutionMode, OperationKind, ParamType, Resource, ResultSpec,
};
use crate::vello::Scene;
use crate::widget::{ResourceOperation, ResourceOperationStart, TaffyTreeExt, Widget};

/// Shared effect-operation type used by the one-resource app host.
pub const OPERATION_TYPE: &str = "tilo.app.operation";

/// Build one resource with a unit operation.
///
/// # Panics
///
/// Panics only if serializing Rust's unit value fails, which the JSON encoder
/// represents unconditionally as `null`.
#[expect(
    clippy::expect_used,
    reason = "serializing the unit operation payload is infallible"
)]
pub fn resource(kind: &str, label: &str, action_id: &str, action_label: &str) -> Resource {
    let mut action = ActionSpec::operation(
        action_id,
        action_label,
        OperationKind::Effect,
        OPERATION_TYPE,
        &(),
    )
    .expect("the unit operation is serializable");
    action.contract = ActionContract {
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        ..ActionContract::default()
    };
    Resource::new(format!("{kind}:root"), kind, label).with_action(action)
}

/// Build one resource whose arguments are schema-bound.
///
/// # Panics
///
/// Panics only if the already-validated JSON argument object cannot be encoded
/// as JSON again.
#[expect(
    clippy::expect_used,
    reason = "the bound arguments are already a serde_json value"
)]
pub fn typed_resource(
    kind: &str,
    label: &str,
    action_id: &str,
    action_label: &str,
    params: Vec<ActionParam>,
) -> Resource {
    let action = ActionSpec::typed_operation(
        action_id,
        action_label,
        "Enter action arguments",
        OperationKind::Effect,
        OPERATION_TYPE,
        params,
        |args| {
            EncodedAction::encode_operation(OPERATION_TYPE, &args.to_json())
                .expect("bound arguments are serializable")
        },
    );
    Resource::new(format!("{kind}:root"), kind, label).with_action(action)
}

/// A value that can connect a [`LocalState`](crate::LocalState) field directly
/// to a typed resource action.
pub trait ActionValue: Clone + Send + Sync + 'static {
    fn param_type() -> ParamType;
    fn into_json(self) -> serde_json::Value;

    /// Decode a validated action argument.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` does not have this action value's type.
    fn from_json(value: &serde_json::Value) -> Result<Self, String>;
}

impl ActionValue for String {
    fn param_type() -> ParamType {
        ParamType::Text
    }

    fn into_json(self) -> serde_json::Value {
        serde_json::Value::String(self)
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        value
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| "expected text".into())
    }
}

impl ActionValue for bool {
    fn param_type() -> ParamType {
        ParamType::Boolean
    }

    fn into_json(self) -> serde_json::Value {
        serde_json::Value::Bool(self)
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        value.as_bool().ok_or_else(|| "expected boolean".into())
    }
}

impl ActionValue for f64 {
    fn param_type() -> ParamType {
        ParamType::Number
    }

    fn into_json(self) -> serde_json::Value {
        serde_json::Number::from_f64(self)
            .map_or(serde_json::Value::Null, serde_json::Value::Number)
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        value.as_f64().ok_or_else(|| "expected number".into())
    }
}

impl ActionValue for i64 {
    fn param_type() -> ParamType {
        ParamType::Integer
    }

    fn into_json(self) -> serde_json::Value {
        serde_json::Value::Number(self.into())
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        value.as_i64().ok_or_else(|| "expected integer".into())
    }
}

type FieldReader = std::sync::Arc<dyn Fn() -> serde_json::Value + Send + Sync>;
type FieldWriter = std::sync::Arc<dyn Fn(&serde_json::Value) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
struct FormField {
    param: ActionParam,
    initial: serde_json::Value,
    read: FieldReader,
    write: FieldWriter,
}

/// One action whose arguments and live UI state are declared together.
///
/// This is intentionally an app-framework abstraction: controls, action
/// discovery, external invocation, and local state all share one declaration.
#[derive(Clone)]
pub struct ActionForm {
    kind: String,
    label: String,
    action_id: String,
    action_label: String,
    fields: Vec<FormField>,
}

impl ActionForm {
    pub fn new(
        kind: impl Into<String>,
        label: impl Into<String>,
        action_id: impl Into<String>,
        action_label: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            label: label.into(),
            action_id: action_id.into(),
            action_label: action_label.into(),
            fields: Vec::new(),
        }
    }

    /// Add a required action argument backed by live local state.
    #[must_use]
    pub fn field<T: ActionValue>(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        state: &crate::LocalState<T>,
    ) -> Self {
        let name = name.into();
        let state_for_read = state.clone();
        let state_for_write = state.clone();
        self.fields.push(FormField {
            param: ActionParam::required_owned(name, T::param_type(), description.into()),
            initial: state.get().into_json(),
            read: std::sync::Arc::new(move || state_for_read.get().into_json()),
            write: std::sync::Arc::new(move |value| {
                state_for_write.set(T::from_json(value)?);
                Ok(())
            }),
        });
        self
    }

    pub fn resource(&self) -> Resource {
        typed_resource(
            &self.kind,
            &self.label,
            &self.action_id,
            &self.action_label,
            self.fields
                .iter()
                .map(|field| field.param.clone())
                .collect(),
        )
    }

    /// Bind one control's typed output while filling every other argument
    /// from its current live state.
    ///
    /// # Errors
    ///
    /// Returns an error when the field is absent, has another action type, or
    /// the generated binding does not match the exported action.
    pub fn binding<T: ActionValue>(&self, name: &str) -> Result<ActionBinding<T>, String> {
        let field = self
            .fields
            .iter()
            .find(|field| field.param.name == name)
            .ok_or_else(|| format!("action form has no `{name}` field"))?;
        if field.param.ty != T::param_type() {
            return Err(format!(
                "action form field `{name}` is {}, not {}",
                field.param.ty.label(),
                T::param_type().label()
            ));
        }
        let resource = self.resource();
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == self.action_id)
            .ok_or_else(|| format!("generated action `{}` is missing", self.action_id))?;
        let fields = self.fields.clone();
        let name = name.to_owned();
        ActionBinding::new(&resource, action, move |value: T| {
            let mut arguments = fields
                .iter()
                .map(|field| (field.param.name.to_string(), (field.read)()))
                .collect::<serde_json::Map<_, _>>();
            arguments.insert(name.clone(), value.into_json());
            arguments
        })
        .map_err(|error: ActionBindingError| error.to_string())
    }

    /// A button binding that restores the values captured when the form was
    /// declared.
    ///
    /// # Errors
    ///
    /// Returns an error only if the generated initial values violate the
    /// generated action contract.
    pub fn reset_binding(&self) -> Result<ActionBinding<()>, String> {
        ActionBinding::static_json_for(
            &self.resource(),
            &self.action_id,
            self.fields
                .iter()
                .map(|field| (field.param.name.to_string(), field.initial.clone()))
                .collect(),
        )
        .map_err(|error| error.to_string())
    }

    fn apply(&self, payload: &[u8]) -> Result<ActionOutput, String> {
        let arguments: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let resource = self.resource();
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == self.action_id)
            .ok_or_else(|| format!("generated action `{}` is missing", self.action_id))?;
        action
            .build_json(&arguments)
            .map_err(|error| error.to_string())?;
        for field in &self.fields {
            let value = arguments
                .get(field.param.name.as_ref())
                .ok_or_else(|| format!("missing `{}`", field.param.name))?;
            (field.write)(value)?;
        }
        Ok(ActionOutput::default())
    }
}

/// Mount a connected action form around its visible controls.
pub fn form(root: Box<dyn Widget>, form: ActionForm) -> Box<dyn Widget> {
    let resource = form.resource();
    immediate(root, resource, move |payload| form.apply(payload))
}

type Immediate = Box<dyn FnMut(&[u8]) -> Result<ActionOutput, String>>;

/// Transparent retained root that publishes and executes one app resource.
pub struct ResourceActionHost {
    root: Box<dyn Widget>,
    resource: Resource,
    run: Immediate,
    rect: Rect,
}

/// Wrap a root with a synchronous typed resource operation.
pub fn immediate(
    root: Box<dyn Widget>,
    resource: Resource,
    run: impl FnMut(&[u8]) -> Result<ActionOutput, String> + 'static,
) -> Box<dyn Widget> {
    Box::new(ResourceActionHost {
        root,
        resource,
        run: Box::new(run),
        rect: Rect::ZERO,
    })
}

impl Widget for ResourceActionHost {
    crate::provides!(dyn crate::resources::PublishesResources);

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.root.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &crate::Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.root.build_layout(taffy, theme, node_map);
        let style = crate::taffy::Style {
            size: crate::taffy::Size {
                width: crate::taffy::style_helpers::percent(1.0_f32),
                height: crate::taffy::style_helpers::percent(1.0_f32),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            self.root
                .apply_layout(taffy, *child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.root
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &crate::winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        let node_id = ctx
            .taffy
            .children(ctx.node_id)
            .ok()
            .and_then(|children| children.first().copied())
            .unwrap_or(ctx.node_id);
        let path = ctx.path;
        self.root
            .handle_event(event, &mut ctx.reborrow(path, node_id));
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.root.as_ref()]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.root.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.root.as_mut()]
    }

    fn is_focusable(&self) -> bool {
        self.root.is_focusable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.root.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.root.is_focused()
    }

    fn is_enabled(&self) -> bool {
        self.root.is_enabled()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.root.ime_cursor_rect()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.root.ime_edit_fingerprint(key)
    }

    fn ime_purpose(&self) -> Option<crate::winit::window::ImePurpose> {
        self.root.ime_purpose()
    }

    fn traps_focus(&self) -> bool {
        self.root.traps_focus()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.root.accessibility()
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.root.synthetic_accessibility_rect(path)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.root.perform_action(action, state, path)
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.root.perform_action_at(action, key_path, state, path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.root
            .perform_semantic_action(target, action, state, path)
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if let Some(started) = crate::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op
            .unscoped()
            .filter(|request| request.kind != OperationKind::Document)
            .filter(|request| request.type_id == OPERATION_TYPE)
        else {
            return Ok(None);
        };
        (self.run)(request.payload)
            .map(ResourceOperationStart::Completed)
            .map(Some)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.root.wants_pointer_capture()
    }

    fn needs_animation_frame(&self) -> bool {
        self.root.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.root.next_wake_deadline()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.root.take_structure_dirty()
    }

    fn needs_redraw(&self) -> bool {
        self.root.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.root.mark_drawn();
    }

    fn is_layout_dirty(&self) -> bool {
        self.root.is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.root.layout_needs_measure()
    }

    fn name(&self) -> &'static str {
        "ResourceActionHost"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl crate::resources::PublishesResources for ResourceActionHost {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        vec![self.resource.clone()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_form_validates_controls_and_connects_invocation() {
        // Case `action_form_connects_controls_discovery_and_external_invocation`.
        {
            let name = crate::LocalState::new(String::from("Ada"));
            let enabled = crate::LocalState::new(true);
            let form = ActionForm::new("profile", "Profile", "save", "Save")
                .field("name", "Display name", &name)
                .field("enabled", "Enabled", &enabled);

            let invocation = form
                .binding::<String>("name")
                .expect("text binding")
                .invoke(String::from("Grace"))
                .expect("valid invocation");
            assert_eq!(invocation.arguments["name"], "Grace");
            assert_eq!(invocation.arguments["enabled"], true);

            form.apply(br#"{"name":"Lin","enabled":false}"#)
                .expect("valid external invocation");
            assert_eq!(name.get(), "Lin");
            assert!(!enabled.get());

            name.set(String::from("Changed"));
            let reset = form
                .reset_binding()
                .expect("reset binding")
                .invoke(())
                .expect("reset invocation");
            assert_eq!(reset.arguments["name"], "Ada");
            assert_eq!(reset.arguments["enabled"], true);
        }

        // Case `action_form_rejects_wrong_control_types_before_mounting`.
        {
            let value = crate::LocalState::new(String::new());
            let form = ActionForm::new("profile", "Profile", "save", "Save").field(
                "name",
                "Display name",
                &value,
            );
            let error = form
                .binding::<bool>("name")
                .err()
                .expect("wrong field type must fail");
            assert!(error.contains("text, not boolean"));
        }
    }
}
