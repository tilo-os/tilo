//! `org.freedesktop.impl.portal.ScreenCast` backend.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.ScreenCast.xml`.
//! Plan: Sandbox Phase 4.3 + §5.3.
//!
//! The screencast handshake is a three-step state machine:
//!
//! 1. `CreateSession(handle, session_handle, app_id, options) → (response, results)`
//!    Allocates a session object; backend stores it for later. Response=0,
//!    results is opaque — frontend caches `session_handle` for the next
//!    calls.
//! 2. `SelectSources(handle, session_handle, app_id, options) → (response, results)`
//!    Lets the app declare *what* it wants to capture. This is where we
//!    surface the consent prompt — `flow::resolve` with
//!    `CapabilityKind::Screencast`.
//! 3. `Start(handle, session_handle, app_id, parent_window, options) → (response, results)`
//!    Final commit step. Returns the streams the app should pull from
//!    Pipewire. In v1 we return an empty `streams` array on Allow — the
//!    full pipewire wiring is deferred. Deny mirrors `SelectSources`.

use std::collections::HashMap;

use zbus::interface;
use zbus::object_server::SignalContext;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::shell::state::{Capability, GrantScope};

use super::{
    flow, file_chooser::FileChooserBackend, CapabilityKind, PortalContext, PromptOutcome,
};

pub struct ScreenCastBackend {
    ctx: PortalContext,
}

impl ScreenCastBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self { ctx }
    }
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastBackend {
    /// `CreateSession` — no-op consent-wise. Returns response=0 + an
    /// empty results dict so the frontend can move on to
    /// `SelectSources`.
    async fn create_session(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        _handle: ObjectPath<'_>,
        _session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!("ScreenCast.CreateSession: app={app_id}");
        (0, HashMap::new())
    }

    /// `SelectSources` — surface the user consent prompt here. This is
    /// the step that decides whether the screencast goes ahead.
    async fn select_sources(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        _session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!("ScreenCast.SelectSources: app={app_id}");
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = FileChooserBackend::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let cap_template = Capability::Screencast {
            scope: GrantScope::Once,
        };
        let outcome = flow::resolve(
            &self.ctx,
            caller,
            CapabilityKind::Screencast,
            cap_template,
        )
        .await;
        let code = if outcome.is_allowed() { 0 } else { 1 };
        (code, HashMap::new())
    }

    /// `Start` — final commit. Mirrors `SelectSources`'s consent (the
    /// frontend won't call `Start` if `SelectSources` denied, but
    /// `xdg-desktop-portal` semantics expect us to re-check the grant
    /// just in case the user revoked between calls). Returns an empty
    /// `streams` array on Allow — the actual Pipewire wiring lands in
    /// the screencast follow-up unit.
    async fn start(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        _session_handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!("ScreenCast.Start: app={app_id}");
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = FileChooserBackend::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let cap_template = Capability::Screencast {
            scope: GrantScope::Once,
        };
        let outcome = flow::resolve(
            &self.ctx,
            caller,
            CapabilityKind::Screencast,
            cap_template,
        )
        .await;
        outcome_to_start_response(outcome)
    }
}

/// Translate the outcome into Start's (response, results) shape.
///
/// The `streams` field in `results` is `a(ua{sv})` — an array of
/// `(node_id, properties)`. v1 returns an empty array on Allow so the
/// frontend's strict-parser doesn't reject the response; the real
/// Pipewire node lookup lands in §5.3's screencast unit.
fn outcome_to_start_response(outcome: PromptOutcome) -> (u32, HashMap<String, OwnedValue>) {
    let mut results: HashMap<String, OwnedValue> = HashMap::new();
    if !outcome.is_allowed() {
        return (1, results);
    }
    // Empty `streams` array — keep the type signature correct.
    let streams: Vec<(u32, HashMap<String, OwnedValue>)> = vec![];
    let v = Value::new(streams);
    if let Ok(o) = OwnedValue::try_from(v) {
        results.insert("streams".to_string(), o);
    }
    (0, results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_allow_returns_response_zero_with_streams_key() {
        let (code, results) = outcome_to_start_response(PromptOutcome::AllowOnce);
        assert_eq!(code, 0);
        assert!(results.contains_key("streams"));
    }

    #[test]
    fn start_deny_returns_response_one_no_results() {
        let (code, results) = outcome_to_start_response(PromptOutcome::Deny);
        assert_eq!(code, 1);
        assert!(results.is_empty());
    }
}
