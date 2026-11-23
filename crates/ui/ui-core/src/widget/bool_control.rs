//! What a two-state control (checkbox, switch) is apart from its face: where its value lives, how a
//! click or an assistive action flips it, and how it reports itself to presence, permissions and the
//! action broker.

use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::widget::binding::{BoxedBinding, Pulled};
use crate::widget::{ActionEmitter, AppModel, ControlBindingMetadata, ModelPath};
use vello::kurbo::Rect;
use winit::event::{ElementState, MouseButton, WindowEvent};

pub(super) type BoolViewHandler = Box<dyn FnMut(bool)>;

pub(crate) enum BoolAction {
    Value(ActionEmitter<bool>),
    Press(ActionEmitter<()>),
}

impl BoolAction {
    /// Queue the invocation of a flip to `flipped`; unavailable and refused flips queue nothing.
    pub(crate) fn emit(&mut self, flipped: bool) {
        let queued = match self {
            Self::Value(action) => action.emit(flipped),
            Self::Press(action) => action.emit(()),
        };
        if let Err(error) = queued {
            tracing::warn!(%error, "a control's action refused its arguments");
        }
    }

    pub(crate) fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        match self {
            Self::Value(action) => action.take(),
            Self::Press(action) => action.take(),
        }
    }

    pub(crate) fn is_available(&self) -> bool {
        match self {
            Self::Value(action) => action.is_available(),
            Self::Press(action) => action.is_available(),
        }
    }

    pub(crate) fn attach(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_domain::DomainAttachment,
    ) {
        let _ = match self {
            Self::Value(action) => action.attach(domain, attachment),
            Self::Press(action) => action.attach(domain, attachment),
        };
    }

    pub(crate) fn refresh_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        match self {
            Self::Value(action) => action.refresh_availability(allowed),
            Self::Press(action) => action.refresh_availability(allowed),
        }
    }

    pub(crate) fn metadata(&self, control_id: impl Into<String>) -> ControlBindingMetadata {
        match self {
            Self::Value(action) => action.metadata(control_id),
            Self::Press(action) => action.metadata(control_id),
        }
    }
}

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
    /// The focus came from a click on this control, not from the keyboard:
    /// it is shown as no ring, only the keyboard's is.
    pub pressed_into_focus: bool,
    /// This control took a press whose release has not come: the release
    /// over it completes that click, so the row it stands in does not read
    /// the click as one of its own.
    pub holds_press: bool,
    pub presence_path: Vec<PathSegment>,
    pub permission_scope: Option<PermissionScope>,
    pub enabled: bool,
    /// Override for `Widget::test_id()`, which otherwise reports the
    /// control's kind.
    pub id_override: Option<String>,
    pub accessible_name: Option<String>,
    pub name_resolved: bool,
    pub binding: Option<BoxedBinding<bool>>,
    /// What a flip, a press or a screen reader's set calls with the value it sets.
    pub on_set: Option<BoolViewHandler>,
    pub action: Option<BoolAction>,
    pub interaction: crate::interaction::InteractionEnvironment,
}

impl BoolControl {
    /// A control reading and writing the boolean under `document_key` in the app document.
    ///
    /// An empty key is a programming error, not a control with no value: a document key nobody
    /// can name reads missing and shows every control as off, whatever it was given. A control
    /// with a value of its own is built with [`Self::given`] or [`Self::local`].
    #[track_caller]
    pub fn document(document_key: &str, accessible_name: Option<String>) -> Self {
        assert!(
            !document_key.is_empty(),
            "a bound control needs a document key; a value of its own is built with `given`"
        );
        Self {
            binding: Some(crate::doc_source::bool_key(document_key)),
            ..Self::bare(accessible_name)
        }
    }

    /// A control showing `on`, with no document binding: its action, view or callback answers a
    /// flip, and a sync leaves the value as given.
    pub fn given(on: bool, accessible_name: Option<String>) -> Self {
        Self {
            is_checked: on,
            ..Self::bare(accessible_name)
        }
    }

    fn bare(accessible_name: Option<String>) -> Self {
        Self {
            rect: Rect::ZERO,
            is_hovered: false,
            is_checked: false,
            is_focused: false,
            pressed_into_focus: false,
            holds_press: false,
            presence_path: Vec::new(),
            permission_scope: None,
            enabled: true,
            id_override: None,
            accessible_name,
            name_resolved: false,
            binding: None,
            on_set: None,
            action: None,
            interaction: crate::interaction::InteractionEnvironment::default(),
        }
    }

    pub fn local(state: &crate::LocalState<bool>, accessible_name: Option<String>) -> Self {
        let (checked, binding) = super::binding::local(state);
        Self {
            is_checked: checked,
            binding: Some(binding),
            ..Self::bare(accessible_name)
        }
    }

    #[must_use]
    pub fn focus_visible(&self) -> bool {
        self.is_focused && !self.pressed_into_focus && crate::focus_visible::shown()
    }

    pub fn effectively_enabled(&self) -> bool {
        self.enabled && self.action.as_ref().is_none_or(BoolAction::is_available)
    }

    pub fn spoken_name(&self) -> Option<String> {
        let name = self.accessible_name.as_ref()?;
        crate::i18n::spoken_name(name, self.name_resolved)
    }

    pub fn test_id<'a>(&'a self, kind: &'a str) -> &'a str {
        self.id_override.as_deref().unwrap_or(kind)
    }

    /// Whether nothing but the value's own binding answers a flip, so an
    /// assistive tool may target the control directly.
    pub fn is_bare(&self) -> bool {
        self.action.is_none()
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
    /// one, else through the binding.
    ///
    /// A control whose action writes a value it also follows (an action and
    /// a binding) shows the value the store holds, never its own guess: it
    /// changes when the binding reports the stored change, so a write that
    /// never lands is never shown as made.
    pub fn flip(&mut self) {
        self.choose(!self.is_checked);
    }

    pub fn choose(&mut self, flipped: bool) {
        if let Some(action) = &mut self.action {
            action.emit(flipped);
        } else if let Some(binding) = &mut self.binding {
            binding.publish(&flipped);
        }
        if self.action.is_none() || self.binding.is_none() {
            self.is_checked = flipped;
        }
        if let Some(on_set) = &mut self.on_set {
            on_set(flipped);
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
                let logical_pos = ctx.logical(position);
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
                self.holds_press = true;
                self.pressed_into_focus |= !self.is_focused;
                *ctx.focus_target = Some(ctx.node_id);
                self.flip();
            }
            // The release of the press this control took is its own: a row
            // that decides a click on its release would otherwise act on
            // the row as well as on the control pressed.
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if std::mem::take(&mut self.holds_press) && self.is_hovered => {
                *ctx.handled = true;
            }
            _ => {}
        }
    }

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
        let id = self.test_id(kind);
        if let Some(action) = &self.action {
            return vec![action.metadata(id)];
        }
        crate::widget::controls::value_control_bindings::<bool>(
            id,
            None,
            self.binding.as_ref().and_then(|b| b.document_key()),
        )
    }
}

macro_rules! bool_control_api {
    () => {
        /// Called with the value a flip, a press or a screen reader sets, for
        /// a value of the window's own; the control is known as `id`.
        #[must_use]
        pub fn on_choose(mut self, id: &str, callback: impl FnMut(bool) + 'static) -> Self {
            self.control.id_override = Some(id.to_owned());
            self.control.action = None;
            self.control.on_set = Some(Box::new(callback));
            self
        }

        #[must_use]
        pub fn on_action(mut self, binding: tilo_actions::ActionBinding<bool>) -> Self {
            self.control.action = Some(crate::widget::BoolAction::Value(
                crate::widget::ActionEmitter::new(binding),
            ));
            self.control.on_set = None;
            self
        }

        #[must_use]
        pub fn on_choose_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
            self.control.action = Some(crate::widget::BoolAction::Press(
                crate::widget::ActionEmitter::new(binding),
            ));
            self.control.on_set = None;
            self
        }

        pub fn is_checked(&self) -> bool {
            self.control.is_checked
        }

        /// Replace the presented value and publish it to a bound local model.
        ///
        /// This is a programmatic model mutation and does not run the
        /// control's `on_choose` callback. Document-bound controls should mutate their
        /// Automerge path instead and let [`Widget::sync`] adopt the result.
        pub fn set_checked(&mut self, checked: bool) {
            self.control.set_checked(checked);
        }

        #[must_use]
        pub fn with_presence(mut self, path: Vec<crate::presence::PathSegment>) -> Self {
            self.control.presence_path = path;
            self
        }

        #[must_use]
        pub fn with_permission_scope(mut self, scope: crate::permissions::PermissionScope) -> Self {
            self.control.permission_scope = Some(scope);
            self
        }

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
        /// announces what it controls rather than just its state: `name` is
        /// a message id, resolved in the language this thread reads.
        #[must_use]
        pub fn with_accessible_name(mut self, name: &str) -> Self {
            self.control.accessible_name = Some(name.to_string());
            self.control.name_resolved = false;
            self
        }

        /// [`Self::with_accessible_name`] with text that is already resolved
        /// (a row's caption, a label the app localized itself): spoken as
        /// given, never looked up as a message id.
        #[must_use]
        pub fn with_resolved_accessible_name(mut self, name: impl Into<String>) -> Self {
            self.control.accessible_name = Some(name.into());
            self.control.name_resolved = true;
            self
        }

        pub fn set_enabled(&mut self, enabled: bool) {
            self.control.enabled = enabled;
        }
    };
}
pub(super) use bool_control_api;

/// The `Widget` methods a [`BoolControl`] answers on its own; `$kind` is the
/// default test id. Events are the widget's own: a checkbox hands them to
/// [`BoolControl::handle_event`], a switch also lets its knob be dragged.
macro_rules! bool_control_widget_methods {
    ($kind:literal) => {
        fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
            if request.wants::<dyn crate::widget::controls::ActionControls>() {
                request.provide::<dyn crate::widget::controls::ActionControls>(self);
            } else if request.wants::<dyn crate::permissions::capability::PermissionGuarded>() {
                request.provide::<dyn crate::permissions::capability::PermissionGuarded>(self);
            } else if request.wants::<dyn crate::presence::capability::PublishesPresence>() {
                request.provide::<dyn crate::presence::capability::PublishesPresence>(self);
            }
        }

        fn provide_mut<'a>(
            &'a mut self,
            request: &mut crate::widget::capability::RequestMut<'a, '_>,
        ) {
            if request.wants::<dyn crate::widget::controls::ActionControls>() {
                request.provide::<dyn crate::widget::controls::ActionControls>(self);
            } else if request.wants::<dyn crate::permissions::capability::PermissionGuarded>() {
                request.provide::<dyn crate::permissions::capability::PermissionGuarded>(self);
            } else if request.wants::<dyn crate::presence::capability::PublishesPresence>() {
                request.provide::<dyn crate::presence::capability::PublishesPresence>(self);
            }
        }

        fn set_interaction_environment(
            &mut self,
            environment: crate::interaction::InteractionEnvironment,
        ) {
            self.control.set_interaction_environment(environment);
        }

        fn sync(&mut self, state: &crate::widget::AppModel, root: &crate::widget::ModelPath) {
            self.control.sync(state, root);
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
            self.control.pressed_into_focus &= focused;
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
            _key_path: &[u64],
            _state: &mut crate::widget::Edits<'_>,
            _path: &crate::widget::ModelPath,
        ) -> bool {
            self.control.perform_action(action)
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

        impl crate::permissions::capability::PermissionGuarded for $ty {
            fn permission_scope(&self) -> Option<crate::permissions::PermissionScope> {
                self.control.permission_scope.clone()
            }
        }

        impl crate::widget::controls::ActionControls for $ty {
            fn attach_controls(
                &mut self,
                domain: &crate::ControlDomain,
                attachment: crate::control_domain::DomainAttachment,
            ) {
                if let Some(action) = &mut self.control.action {
                    action.attach(domain, attachment);
                }
            }
            fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
                self.control
                    .action
                    .as_mut()
                    .and_then(crate::widget::BoolAction::take)
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
