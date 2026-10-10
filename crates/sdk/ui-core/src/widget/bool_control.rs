//! What a two-state control (checkbox, switch) is apart from its face: where
//! its value lives, how a click or an assistive action flips it, and how it
//! reports itself to presence, permissions and the action broker.

use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::widget::binding::{BoxedBinding, Pulled};
use crate::widget::{ActionEmitter, AppModel, ControlBindingMetadata, ModelPath};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

pub(super) type BoolChangeCallback = Box<dyn FnMut(bool)>;

/// `is_hovered`, `is_checked`, `is_focused` and `enabled` are four
/// independent state axes (pointer, CRDT-synced value, keyboard focus,
/// build-time enablement), each set/read by name — not positional bool
/// arguments — so bundling them into a bitflags type would touch every
/// read/write site for no behavioural change.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent state axes, all set/read by name, not positional bool args"
)]
pub(super) struct BoolControl {
    pub rect: Rect,
    pub is_hovered: bool,
    pub is_checked: bool,
    pub is_focused: bool,
    pub presence_path: Vec<PathSegment>,
    pub permission_scope: Option<PermissionScope>,
    /// When `false` the control is skipped by focus, rejects input (no flip,
    /// no hover feedback) and renders dimmed.
    pub enabled: bool,
    /// Override for `Widget::test_id()`, which otherwise reports the
    /// control's kind.
    pub id_override: Option<String>,
    /// Spoken name for a control with no owned visible label; unset falls
    /// back to a sibling label resolved by the tree builder.
    pub accessible_name: Option<String>,
    /// Where the checked value lives: a document key or process-local state.
    pub binding: Option<BoxedBinding<bool>>,
    /// Observes user intent without owning model synchronization.
    pub on_change: Option<BoolChangeCallback>,
    pub action: Option<ActionEmitter<bool>>,
    pub interaction: crate::interaction::InteractionEnvironment,
}

impl BoolControl {
    /// A control bound to the document boolean at `on_toggle_path`.
    pub fn new(on_toggle_path: &str, accessible_name: Option<String>) -> Self {
        Self {
            rect: Rect::ZERO,
            is_hovered: false,
            is_checked: false,
            is_focused: false,
            presence_path: Vec::new(),
            permission_scope: None,
            enabled: true,
            id_override: None,
            accessible_name,
            binding: Some(crate::doc_source::bool_key(on_toggle_path)),
            on_change: None,
            action: None,
            interaction: crate::interaction::InteractionEnvironment::default(),
        }
    }

    /// A control two-way bound to process-local `state`.
    pub fn local(state: &crate::LocalState<bool>, accessible_name: Option<String>) -> Self {
        let (checked, binding) = super::binding::local(state);
        Self {
            is_checked: checked,
            binding: Some(binding),
            ..Self::new("", accessible_name)
        }
    }

    pub fn effectively_enabled(&self) -> bool {
        self.enabled && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }

    pub fn test_id<'a>(&'a self, kind: &'a str) -> &'a str {
        self.id_override.as_deref().unwrap_or(kind)
    }

    /// Whether nothing but the value's own binding answers a flip, so an
    /// assistive tool may target the control directly.
    pub fn is_bare(&self) -> bool {
        self.action.is_none() && self.on_change.is_none()
    }

    pub fn set_checked(&mut self, checked: bool) {
        self.is_checked = checked;
        if self.action.is_none()
            && let Some(binding) = &mut self.binding
        {
            binding.publish(&self.is_checked);
        }
    }

    /// Flip the value the way a click does: through the action when there is
    /// one, else through the binding. `on_change` hears the new value only
    /// when no action owns it.
    pub fn flip(&mut self) {
        let flipped = !self.is_checked;
        if let Some(action) = &mut self.action {
            let _ = action.emit(flipped);
        } else if let Some(binding) = &mut self.binding {
            binding.publish(&flipped);
        }
        self.is_checked = flipped;
        if self.action.is_none()
            && let Some(callback) = self.on_change.as_mut()
        {
            callback(flipped);
        }
    }

    pub fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
        self.is_hovered &= environment.supports_hover();
    }

    pub fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if let Some(binding) = &mut self.binding {
            match binding.pull(state, root) {
                Pulled::Value(checked) => self.is_checked = checked,
                Pulled::Missing => self.is_checked = false,
                Pulled::Unchanged => {}
            }
        }
    }

    pub fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.effectively_enabled() {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                // A hover change is a repaint, not consumed input: a parent
                // that stops at the first handled child must still deliver
                // this move to the control the pointer went to, or a control
                // beside this one never learns it is under the pointer.
                self.is_hovered = self.rect.contains(logical_pos);
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } if self.is_hovered && *element_state == ElementState::Pressed => {
                *ctx.handled = true;
                *ctx.focus_target = Some(ctx.node_id);
                self.flip();
            }
            _ => {}
        }
    }

    /// An assistive tool flips the control with `ToggleCheck` or the generic
    /// `Activate`, through the same write a click performs. A disabled
    /// control rejects both.
    pub fn perform_action(&mut self, action: &crate::a11y::Action) -> bool {
        use crate::a11y::Action;
        if !self.effectively_enabled() {
            return false;
        }
        match action {
            Action::ToggleCheck | Action::Activate => {
                self.flip();
                true
            }
            _ => false,
        }
    }

    pub fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }

    pub fn control_action_bindings(&self, kind: &str) -> Vec<ControlBindingMetadata> {
        crate::widget::controls::value_control_bindings(
            self.test_id(kind),
            self.action.as_ref(),
            self.binding.as_ref().and_then(|b| b.document_key()),
            self.on_change.is_some(),
        )
    }
}

/// The public value, wiring and enablement API of a widget holding a
/// [`BoolControl`] in `self.control`.
macro_rules! bool_control_api {
    () => {
        /// Observe changes produced by user interaction.
        ///
        /// Model synchronization and [`Self::set_checked`] do not invoke this
        /// callback, so validation or action dispatch cannot mistake a model
        /// refresh for new user intent.
        #[must_use]
        pub fn on_change(mut self, callback: impl FnMut(bool) + 'static) -> Self {
            if self.control.action.is_none() {
                self.control.on_change = Some(Box::new(callback));
            }
            self
        }

        /// Route pointer, keyboard, and accessibility flips through an
        /// exported resource action.
        #[must_use]
        pub fn on_action(mut self, binding: tilo_actions::ActionBinding<bool>) -> Self {
            self.control.action = Some(crate::widget::ActionEmitter::new(binding));
            self.control.on_change = None;
            self
        }

        /// Return the value currently presented.
        pub fn is_checked(&self) -> bool {
            self.control.is_checked
        }

        /// Replace the presented value and publish it to a bound local model.
        ///
        /// This is a programmatic model mutation and does not invoke
        /// [`Self::on_change`]. Document-bound controls should mutate their
        /// Automerge path instead and let [`Widget::sync`] adopt the result.
        pub fn set_checked(&mut self, checked: bool) {
            self.control.set_checked(checked);
        }

        /// Opt this widget into presence tracking by binding it to a
        /// CRDT-style path.
        #[must_use]
        pub fn with_presence(mut self, path: Vec<crate::presence::PathSegment>) -> Self {
            self.control.presence_path = path;
            self
        }

        /// Gate this widget behind a permission scope.
        #[must_use]
        pub fn with_permission_scope(mut self, scope: crate::permissions::PermissionScope) -> Self {
            self.control.permission_scope = Some(scope);
            self
        }

        /// Override the value `Widget::test_id()` reports, to tell several
        /// such controls in one surface apart.
        #[must_use]
        pub fn with_id(mut self, id: &str) -> Self {
            self.control.id_override = Some(id.to_string());
            self
        }

        /// Set the enabled state at build time. A disabled control is
        /// skipped by focus, rejects input and renders dimmed. Default `true`.
        #[must_use]
        pub fn with_enabled(mut self, enabled: bool) -> Self {
            self.control.enabled = enabled;
            self
        }

        /// Give a label-less control a spoken name so a screen reader
        /// announces what it controls rather than just its state.
        #[must_use]
        pub fn with_accessible_name(mut self, name: &str) -> Self {
            self.control.accessible_name = Some(name.to_string());
            self
        }

        /// Imperatively flip the enabled state.
        pub fn set_enabled(&mut self, enabled: bool) {
            self.control.enabled = enabled;
        }
    };
}
pub(super) use bool_control_api;

/// The `Widget` methods a [`BoolControl`] answers on its own; `$kind` is the
/// default test id.
macro_rules! bool_control_widget_methods {
    ($kind:literal) => {
        crate::provides!(
            dyn crate::widget::controls::ActionControls,
            dyn crate::permissions::capability::PermissionGated,
            dyn crate::presence::capability::PublishesPresence
        );

        fn set_interaction_environment(
            &mut self,
            environment: crate::interaction::InteractionEnvironment,
        ) {
            self.control.set_interaction_environment(environment);
        }

        fn sync(&mut self, state: &crate::widget::AppModel, root: &crate::widget::ModelPath) {
            self.control.sync(state, root);
        }

        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            ctx: &mut crate::widget::EventCtx<'_>,
        ) {
            self.control.handle_event(event, ctx);
        }

        fn test_id(&self) -> Option<&str> {
            Some(self.control.test_id($kind))
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            self.control.rect
        }

        fn is_focusable(&self) -> bool {
            true
        }
        fn set_focus(&mut self, focused: bool) {
            self.control.is_focused = focused;
        }
        fn is_focused(&self) -> bool {
            self.control.is_focused
        }
        fn is_enabled(&self) -> bool {
            self.control.effectively_enabled()
        }

        fn perform_action(
            &mut self,
            action: &crate::a11y::Action,
            _state: &mut crate::widget::Edits<'_>,
            _path: &crate::widget::ModelPath,
        ) -> bool {
            self.control.perform_action(action)
        }

        fn draw_with_presence(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text_renderer: &mut crate::text_render::TextRenderer,
            theme: &crate::theme::Theme,
            presence: &crate::presence::PresenceCtx,
            permissions: &crate::permissions::PermissionContext,
        ) {
            crate::widget::draw_gated_leaf(
                self,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
            );
        }
    };
}
pub(super) use bool_control_widget_methods;

/// The presence, permission and action-broker capabilities of `$ty`, read
/// from its [`BoolControl`]; `$kind` is the default test id.
macro_rules! bool_control_capabilities {
    ($ty:ty, $kind:literal) => {
        impl crate::presence::capability::PublishesPresence for $ty {
            fn presence_key(&self) -> Option<crate::presence::PresenceKey> {
                self.control.presence_key()
            }
        }

        impl crate::permissions::capability::PermissionGated for $ty {
            fn permission_scope(&self) -> Option<crate::permissions::PermissionScope> {
                self.control.permission_scope.clone()
            }
        }

        impl crate::widget::controls::ActionControls for $ty {
            fn install_control_custody(
                &mut self,
                domain: &crate::ControlDomain,
                attachment: crate::control_custody::CustodyAttachment,
            ) {
                if let Some(action) = &mut self.control.action {
                    let _ = action.install_custody(domain, attachment);
                }
            }
            fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
                self.control
                    .action
                    .as_mut()
                    .and_then(crate::widget::ActionEmitter::take)
            }
            fn refresh_control_action_availability(
                &mut self,
                allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
            ) {
                if let Some(action) = &mut self.control.action {
                    action.refresh_availability(allowed);
                }
            }
            fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
                self.control.control_action_bindings($kind)
            }
        }
    };
}
pub(super) use bool_control_capabilities;
