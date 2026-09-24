//! `org.freedesktop.impl.portal.Access` backend — also serves as the
//! Camera consent surface.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.Access.xml`.
//! Camera reuses `Access.AccessDialog` rather than owning a separate
//! `impl.portal.Camera` interface. This backend is therefore a camera consent
//! surface. It never guesses a security capability from translated display
//! text; microphone access has its own PipeWire/portal path.

#![allow(
    clippy::used_underscore_binding,
    reason = "the D-Bus AccessDialog signature requires unused positional parameters (handle, \
              parent_window); zbus's #[interface] macro deserialises them into these named \
              parameters and forwards them by identifier into its generated dispatcher, so the \
              lint fires on macro-generated code a per-function #[allow] cannot reach"
)]

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::names::OwnedUniqueName;
#[cfg(target_os = "linux")]
use zbus::object_server::SignalEmitter;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;
use zbus::zvariant::OwnedValue;

use crate::shell::state::{Capability, GRANT_ONCE};

#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
use super::{CapabilityKind, PromptOutcome};
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call, flow, lockdown};

#[cfg(target_os = "linux")]
pub struct AccessBackend {
    ctx: PortalContext,
    lifecycle: PortalLifecycle,
}

#[cfg(target_os = "linux")]
impl AccessBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new())
    }
    pub fn with_lifecycle(ctx: PortalContext, lifecycle: PortalLifecycle) -> Self {
        Self { ctx, lifecycle }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Access")]
#[cfg(target_os = "linux")]
impl AccessBackend {
    /// `AccessDialog` — generic consent prompt the host portal uses for Camera, Background, and similar capability gates.
    #[allow(
        clippy::too_many_arguments,
        reason = "argument list mirrors the fixed org.freedesktop.impl.portal.Access.AccessDialog D-Bus signature"
    )]
    async fn access_dialog(
        &self,
        #[zbus(signal_emitter)] _ctx: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        _subtitle: String,
        body: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "Access.AccessDialog: app={} title={:?} body={:?}",
            app_id,
            title,
            body
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) =
            authenticate_call(&self.ctx, sender.as_ref(), &app_id, Some(&parent_window)).await
        else {
            return (2, HashMap::new());
        };
        if lockdown::current_policy().disable_camera {
            return (1, HashMap::new());
        }
        let request = handle.to_string();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = cancelled.clone();
        let prompts = self.ctx.prompts.clone();
        let cancellation_request = request.clone();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id: auth.caller.app_id.clone(),
            resource: LifecycleResource::Prompt,
        };
        if !matches!(
            self.lifecycle
                .register_request(connection, &request, entry, move |_| {
                    cancellation.store(true, Ordering::Release);
                    prompts.cancel_request(&cancellation_request);
                })
                .await,
            Ok(true)
        ) {
            return (2, HashMap::new());
        }
        let (kind, cap_template) = camera_capability();
        let outcome =
            flow::resolve_for_request(&self.ctx, auth.caller, kind, cap_template, request.clone())
                .await;
        self.lifecycle
            .complete_request_object(connection, &request)
            .await;
        if cancelled.load(Ordering::Acquire) {
            return (1, HashMap::new());
        }
        outcome_to_access_response(outcome)
    }
}

/// The fixed security meaning of `impl.portal.Access` in Tilo. Human-facing
/// title/subtitle/body strings are presentation only and cannot affect policy.
pub(super) fn camera_capability() -> (CapabilityKind, Capability) {
    (
        CapabilityKind::Camera,
        Capability::Camera { scope: GRANT_ONCE },
    )
}

fn outcome_to_access_response(outcome: PromptOutcome) -> (u32, HashMap<String, OwnedValue>) {
    let results: HashMap<String, OwnedValue> = HashMap::new();
    let code = u32::from(!outcome.is_allowed());
    (code, results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_is_always_camera() {
        let (kind, cap) = camera_capability();
        assert_eq!(kind, CapabilityKind::Camera);
        assert!(matches!(cap, Capability::Camera { .. }));
    }

    #[test]
    fn translated_or_hostile_ui_text_cannot_change_the_capability() {
        let (kind, cap) = camera_capability();
        assert_eq!(kind, CapabilityKind::Camera);
        assert!(matches!(cap, Capability::Camera { .. }));
    }

    #[test]
    fn access_allow_returns_zero() {
        let (code, results) = outcome_to_access_response(PromptOutcome::AllowOnce);
        assert_eq!(code, 0);
        assert!(results.is_empty());
    }

    #[test]
    fn access_deny_returns_one() {
        let (code, _) = outcome_to_access_response(PromptOutcome::Deny);
        assert_eq!(code, 1);
    }
}
