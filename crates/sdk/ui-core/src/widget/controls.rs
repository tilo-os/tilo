//! Action-bound controls: the capability a widget provides when it owns an
//! [`ActionEmitter`](super::ActionEmitter) (a button, a checkbox, a custom
//! painted control), and the walks the host uses to reach every control in a
//! retained tree.
//!
//! A widget that provides [`ActionControls`] answers for its whole subtree:
//! its overrides decide which children to reach, through the walks below. The
//! trait defaults do what an ordinary container does, so an override names
//! only what differs.

use std::collections::HashSet;

use super::capability::{capability, capability_mut};
use super::{
    ControlBindingKind, ControlBindingMetadata, ResourceOperationStart, Widget,
    semantic_action_bindings,
};

/// How the host answered a control's invocation: started (done at once with
/// its output, or accepted to finish later), or refused.
pub type ControlResult = Result<ResourceOperationStart, tilo_actions::DoorError>;

/// A control intent and the effect invocation it continues, when an
/// effect's handler or completion queued it.
pub type AttributedControlInvocation = (
    tilo_actions::ControlInvocation,
    Option<tilo_actions::InvocationId>,
);

/// A widget that owns action-bound controls.
pub trait ActionControls: Widget {
    /// Bind retained producers before their first callback. Private holders
    /// must override this; child traversal alone never certifies full coverage.
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        for child in self.children_mut() {
            install_control_custody(child, domain, attachment);
        }
    }

    /// Drain one queued action-bound control intent from this subtree.
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.children_mut()
            .into_iter()
            .find_map(take_control_invocation)
    }

    /// [`Self::take_control_invocation`], with the effect invocation the
    /// intent continues when an effect's handler or completion queued it:
    /// the host applies it for that invocation's invoker.
    fn take_attributed_control_invocation(&mut self) -> Option<AttributedControlInvocation> {
        self.take_control_invocation()
            .map(|invocation| (invocation, None))
    }

    /// Report the admission result of one queued local control invocation.
    /// `Accepted` means queued, not completed; confirmation remains an error
    /// while the host offers its normal confirmation UI. This notification
    /// grants no document authority and must not retry the invocation itself.
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &ControlResult,
    ) {
        result_to_children(self, address, result);
    }

    /// Inventory action-native and explicitly legacy controls. A widget that
    /// owns an emitter prepends its [`metadata`](super::ActionEmitter::metadata)
    /// to its descendants' inventory.
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        default_bindings(self)
    }

    /// Whether this exposed subtree contains controls consuming action
    /// availability. An override must match whether `control_action_bindings`
    /// contains an `Action`, including the same modal child exposure,
    /// regardless of current availability.
    fn has_action_control_bindings(&self) -> bool {
        self.control_action_bindings()
            .iter()
            .any(|binding| matches!(binding.kind, ControlBindingKind::Action(_)))
    }

    /// Push the fresh local permission-filtered action surface into every
    /// retained control: refresh this widget's own emitter, then its children.
    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        for child in self.children_mut() {
            refresh_control_action_availability(child, allowed);
        }
    }

    /// Local control intent queued behind asynchronous startup preparation.
    /// A scheduling hint only; the host still checks live authority.
    fn has_pending_control_invocation(&self) -> bool {
        self.children()
            .into_iter()
            .any(has_pending_control_invocation)
    }
}

/// The custody, drain and availability [`ActionControls`] methods of a
/// widget whose only producer is its optional `self.action` emitter. Pass
/// `custody` to leave availability to a widget that refreshes it itself.
macro_rules! optional_emitter_controls {
    () => {
        crate::widget::controls::optional_emitter_controls!(custody);
        fn refresh_control_action_availability(
            &mut self,
            allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
        ) {
            if let Some(action) = &mut self.action {
                action.refresh_availability(allowed);
            }
        }
    };
    (custody) => {
        fn install_control_custody(
            &mut self,
            domain: &crate::ControlDomain,
            attachment: crate::control_custody::CustodyAttachment,
        ) {
            if let Some(action) = &mut self.action {
                let _ = action.install_custody(domain, attachment);
            }
        }
        fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
            self.action
                .as_mut()
                .and_then(crate::widget::ActionEmitter::take)
        }
    };
}
pub(crate) use optional_emitter_controls;

/// A value control's binding: its action, else the collaborative document
/// key it writes, else its legacy change callback, else its semantic action.
pub(crate) fn value_control_bindings<I>(
    control_id: &str,
    action: Option<&super::ActionEmitter<I>>,
    document_key: Option<&str>,
    has_callback: bool,
) -> Vec<ControlBindingMetadata> {
    if let Some(action) = action {
        return vec![action.metadata(control_id)];
    }
    let kind = if let Some(key) = document_key {
        ControlBindingKind::CollaborativeDocument(key.to_owned())
    } else if has_callback {
        ControlBindingKind::LegacyCallback
    } else {
        ControlBindingKind::SemanticAction(control_id.to_owned())
    };
    vec![ControlBindingMetadata {
        control_id: control_id.to_owned(),
        kind,
    }]
}

fn default_bindings(widget: &(impl Widget + ?Sized)) -> Vec<ControlBindingMetadata> {
    let mut bindings = semantic_action_bindings(widget.accessibility());
    widget.for_each_child(&mut |child| bindings.extend(control_action_bindings(child)));
    bindings
}

fn result_to_children(
    widget: &mut (impl Widget + ?Sized),
    address: &tilo_actions::ActionAddress,
    result: &ControlResult,
) {
    for child in widget.children_mut() {
        if !child.has_separate_document_owner() {
            on_control_invocation_result(child, address, result);
        }
    }
}

/// Bind every retained control producer under `widget`.
pub fn install_control_custody(
    widget: &mut dyn Widget,
    domain: &crate::ControlDomain,
    attachment: crate::control_custody::CustodyAttachment,
) {
    if let Some(controls) = capability_mut::<dyn ActionControls>(&mut *widget) {
        controls.install_control_custody(domain, attachment);
        return;
    }
    for child in widget.children_mut() {
        install_control_custody(child, domain, attachment);
    }
}

/// Drain one queued control intent from `widget`'s subtree.
pub fn take_control_invocation(widget: &mut dyn Widget) -> Option<tilo_actions::ControlInvocation> {
    if let Some(controls) = capability_mut::<dyn ActionControls>(&mut *widget) {
        return controls.take_control_invocation();
    }
    widget
        .children_mut()
        .into_iter()
        .find_map(take_control_invocation)
}

/// [`take_control_invocation`], with the effect invocation the intent
/// continues ([`ActionControls::take_attributed_control_invocation`]).
pub fn take_attributed_control_invocation(
    widget: &mut dyn Widget,
) -> Option<AttributedControlInvocation> {
    if let Some(controls) = capability_mut::<dyn ActionControls>(&mut *widget) {
        return controls.take_attributed_control_invocation();
    }
    widget
        .children_mut()
        .into_iter()
        .find_map(take_attributed_control_invocation)
}

/// Report a control invocation's admission result through `widget`'s subtree.
pub fn on_control_invocation_result(
    widget: &mut dyn Widget,
    address: &tilo_actions::ActionAddress,
    result: &ControlResult,
) {
    if let Some(controls) = capability_mut::<dyn ActionControls>(&mut *widget) {
        controls.on_control_invocation_result(address, result);
        return;
    }
    result_to_children(widget, address, result);
}

/// The control inventory of `widget`'s subtree.
pub fn control_action_bindings(widget: &dyn Widget) -> Vec<ControlBindingMetadata> {
    if let Some(controls) = capability::<dyn ActionControls>(widget) {
        return controls.control_action_bindings();
    }
    default_bindings(widget)
}

/// Whether `widget`'s subtree contains controls consuming action availability.
pub fn has_action_control_bindings(widget: &dyn Widget) -> bool {
    if let Some(controls) = capability::<dyn ActionControls>(widget) {
        return controls.has_action_control_bindings();
    }
    // Without ActionControls, this widget contributes only semantic bindings.
    // Those never consume ActionEmitter availability; inspect exposed children
    // directly rather than materializing the widget's accessibility subtree.
    let mut found = false;
    widget.for_each_child(&mut |child| {
        if !found {
            found = has_action_control_bindings(child);
        }
    });
    found
}

/// Push the permission-filtered action surface into every control under `widget`.
#[allow(
    clippy::implicit_hasher,
    reason = "the host publishes its action surface as one std HashSet"
)]
pub fn refresh_control_action_availability(
    widget: &mut dyn Widget,
    allowed: &HashSet<tilo_actions::ActionAddress>,
) {
    if let Some(controls) = capability_mut::<dyn ActionControls>(&mut *widget) {
        controls.refresh_control_action_availability(allowed);
        return;
    }
    for child in widget.children_mut() {
        refresh_control_action_availability(child, allowed);
    }
}

/// Whether a control intent under `widget` waits on startup preparation.
pub fn has_pending_control_invocation(widget: &dyn Widget) -> bool {
    if let Some(controls) = capability::<dyn ActionControls>(widget) {
        return controls.has_pending_control_invocation();
    }
    widget
        .children()
        .into_iter()
        .any(has_pending_control_invocation)
}

#[cfg(test)]
#[path = "controls/presence_tests.rs"]
mod presence_tests;
