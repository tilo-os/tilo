//! `org.freedesktop.impl.portal.Access` backend — also serves as the
//! Camera consent surface.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.Access.xml`.
//! Camera reuses `Access.AccessDialog` rather than owning a separate
//! `impl.portal.Camera` interface: `xdg-desktop-portal-camera` turns an
//! `AccessCamera()` call into an `AccessDialog` call on the configured
//! backend with `subtitle="Allow access to your camera?"` (or similar);
//! the request is classified as a Camera capability from the title/subtitle/options dict — see [`classify`].

use std::collections::HashMap;

use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalContext;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::shell::state::{Capability, GRANT_ONCE};

use super::{CapabilityKind, PortalContext, PromptOutcome, file_chooser::FileChooserBackend, flow};

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
    /// `AccessDialog` — generic consent prompt the host portal uses for Camera, Background, and similar capability gates.
    #[allow(
        clippy::too_many_arguments,
        clippy::used_underscore_binding,
        reason = "argument list mirrors the fixed org.freedesktop.impl.portal.Access.AccessDialog D-Bus signature; zbus's #[interface] dispatch wraps every call in an async task that captures unused underscore-prefixed parameters, tripping used_underscore_binding on the desugared code"
    )]
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
/// metadata. Matches "microphone" / "audio" in the lowercased
/// title+subtitle+body to route Microphone requests — substring rather
/// than a fixed phrase, to keep working across locales the portal hasn't translated yet.
///
/// Falls back to `Camera` — the only consumer this backend's surface
/// targets, since Microphone goes through Pipewire's own ACL and `RemoteDesktop` / Inhibit / Wallpaper aren't handled here.
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
            Capability::Microphone { scope: GRANT_ONCE },
        );
    }
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
        // Anything that isn't microphone/audio is treated as Camera, so the prompt + grant model still mediates.
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
