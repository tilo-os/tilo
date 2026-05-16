//! `org.freedesktop.impl.portal.Access` backend — also serves as the
//! Camera consent surface.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.Access.xml`.
//! Plan: Sandbox Phase 4.3, lines 335-353 (the plan lists
//! "FileChooser+ScreenCast+Camera"; the host-side portal layout has
//! Camera reusing `Access.AccessDialog` rather than owning a separate
//! `impl.portal.Camera` interface).
//!
//! When `xdg-desktop-portal-camera` receives an `AccessCamera()` call,
//! it turns around and calls `org.freedesktop.impl.portal.Access.AccessDialog`
//! on the configured backend with `subtitle="Allow access to your camera?"`
//! (or similar localised text). We classify the request as a Camera
//! capability based on the title/subtitle/options dict — see
//! [`classify`].
//!
//! Methods:
//! - `AccessDialog(handle, app_id, parent_window, title, subtitle, body, options) → (response, results)`

use std::collections::HashMap;

use zbus::interface;
use zbus::object_server::SignalContext;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::shell::state::{Capability, GrantScope};

use super::{
    flow, file_chooser::FileChooserBackend, CapabilityKind, PortalContext, PromptOutcome,
};

pub struct AccessBackend {
    ctx: PortalContext,
}

impl AccessBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self { ctx }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Access")]
impl AccessBackend {
    /// `AccessDialog` — generic consent prompt the host portal uses for
    /// Camera, Background, and similar capability gates.
    async fn access_dialog(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        title: String,
        subtitle: String,
        body: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "Access.AccessDialog: app={} title={:?} body={:?}",
            app_id,
            title,
            body
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = FileChooserBackend::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let (kind, cap_template) = classify(&title, &subtitle, &body, &options);
        let outcome = flow::resolve(&self.ctx, caller, kind, cap_template).await;
        outcome_to_access_response(outcome)
    }
}

/// Pick a [`CapabilityKind`] + [`Capability`] template from the request
/// metadata. Heuristic: the host portal's `Camera.AccessCamera()` path
/// passes a fixed `body` (in English: "{app_name} wants access to your
/// camera"); we match on the substring "camera" to keep the heuristic
/// stable across locales the portal hasn't translated yet.
///
/// Falls back to `Camera` because that's the only consumer the v1 plan
/// targets — Microphone goes through Pipewire's own ACL (plan §5.3
/// step 1); RemoteDesktop / Inhibit / Wallpaper aren't on the v1
/// backend's surface. A more elaborate matcher belongs in the §5.3
/// follow-up unit if/when we add the Microphone + Background routes.
pub(super) fn classify(
    title: &str,
    subtitle: &str,
    body: &str,
    _options: &HashMap<String, OwnedValue>,
) -> (CapabilityKind, Capability) {
    let blob = format!(
        "{} {} {}",
        title.to_lowercase(),
        subtitle.to_lowercase(),
        body.to_lowercase()
    );
    if blob.contains("microphone") || blob.contains("audio") {
        return (
            CapabilityKind::Microphone,
            Capability::Microphone {
                scope: GrantScope::Once,
            },
        );
    }
    // Default = Camera. Matches the only consumer in v1.
    (
        CapabilityKind::Camera,
        Capability::Camera {
            scope: GrantScope::Once,
        },
    )
}

fn outcome_to_access_response(outcome: PromptOutcome) -> (u32, HashMap<String, OwnedValue>) {
    let results: HashMap<String, OwnedValue> = HashMap::new();
    let code = if outcome.is_allowed() { 0 } else { 1 };
    (code, results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_camera_default() {
        let opts = HashMap::new();
        let (kind, cap) = classify("Camera Access", "Allow camera?", "wants access", &opts);
        assert_eq!(kind, CapabilityKind::Camera);
        assert!(matches!(cap, Capability::Camera { .. }));
    }

    #[test]
    fn classify_microphone_when_audio_in_body() {
        let opts = HashMap::new();
        let (kind, cap) = classify("Allow", "Microphone Access", "use audio input", &opts);
        assert_eq!(kind, CapabilityKind::Microphone);
        assert!(matches!(cap, Capability::Microphone { .. }));
    }

    #[test]
    fn classify_falls_back_to_camera_on_unknown_text() {
        let opts = HashMap::new();
        // Plan §4.3 only models Camera + Mic via Access; anything else
        // we treat as Camera so the prompt + grant model still mediate.
        let (kind, _cap) = classify("Permission", "", "weird thing", &opts);
        assert_eq!(kind, CapabilityKind::Camera);
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
