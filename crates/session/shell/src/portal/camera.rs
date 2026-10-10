//! `org.freedesktop.impl.portal.Access` backend — also serves as the
//! Camera consent surface.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.Access.xml`.
//! Camera reuses `Access.AccessDialog` rather than owning a separate
//! `impl.portal.Camera` interface. This backend is therefore a camera consent
//! surface. It never guesses a security capability from translated display
//! text; microphone access has its own PipeWire/portal path.

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "linux")]
use zbus::names::OwnedUniqueName;
#[cfg(target_os = "linux")]
use zbus::object_server::DispatchResult2;
#[cfg(target_os = "linux")]
use zbus::zvariant::OwnedObjectPath;
use zbus::zvariant::OwnedValue;
#[cfg(target_os = "linux")]
use zbus::{Connection, message::Header, message::Message};

use crate::shell::state::{Capability, GRANT_ONCE};

#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
use super::{CapabilityKind, PromptOutcome};
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call, flow, interface_dispatch, lockdown};

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

/// The spec arguments of one `AccessDialog` call that policy reads. The
/// subtitle and options are accepted on the wire and ignored.
#[cfg(target_os = "linux")]
struct AccessDialogRequest {
    handle: OwnedObjectPath,
    app_id: String,
    parent_window: String,
    title: String,
    body: String,
}

/// `AccessDialog`'s wire arguments: `handle o`, `app_id s`, `parent_window s`,
/// `title s`, `subtitle s`, `body s`, `options a{sv}`.
#[cfg(target_os = "linux")]
type AccessDialogArgs = (
    OwnedObjectPath,
    String,
    String,
    String,
    String,
    String,
    HashMap<String, OwnedValue>,
);

#[cfg(target_os = "linux")]
impl From<AccessDialogArgs> for AccessDialogRequest {
    fn from(
        (handle, app_id, parent_window, title, _subtitle, body, _options): AccessDialogArgs,
    ) -> Self {
        Self {
            handle,
            app_id,
            parent_window,
            title,
            body,
        }
    }
}

/// The interface's introspection, byte-identical to what zbus's
/// `#[interface]` macro wrote for the former macro-dispatched backend.
#[cfg(target_os = "linux")]
const ACCESS_INTROSPECTION: &str = r#"<interface name="org.freedesktop.impl.portal.Access">
  <!--
   `AccessDialog` — generic consent prompt the host portal uses for Camera, Background, and similar capability gates.
   -->
  <method name="AccessDialog">
    <arg name="handle" type="o" direction="in"/>
    <arg name="app_id" type="s" direction="in"/>
    <arg name="parent_window" type="s" direction="in"/>
    <arg name="title" type="s" direction="in"/>
    <arg name="_subtitle" type="s" direction="in"/>
    <arg name="body" type="s" direction="in"/>
    <arg name="_options" type="a{sv}" direction="in"/>
    <arg type="u" direction="out"/>
    <arg type="a{sv}" direction="out"/>
  </method>
</interface>
"#;

#[cfg(target_os = "linux")]
interface_dispatch::method_interface!(
    AccessBackend,
    "org.freedesktop.impl.portal.Access",
    ACCESS_INTROSPECTION
);

#[cfg(target_os = "linux")]
impl AccessBackend {
    fn dispatch_call<'call>(
        &'call self,
        connection: &'call Connection,
        message: &'call Message,
        name: &str,
    ) -> DispatchResult2<'call> {
        match name {
            "AccessDialog" => interface_dispatch::dispatch(
                connection,
                message,
                move |hdr, args: AccessDialogArgs| async move {
                    Ok(self.access_dialog(&hdr, connection, args.into()).await)
                },
            ),
            _ => DispatchResult2::NotFound,
        }
    }

    /// `AccessDialog` — generic consent prompt the host portal uses for Camera, Background, and similar capability gates.
    async fn access_dialog(
        &self,
        hdr: &Header<'_>,
        connection: &Connection,
        request: AccessDialogRequest,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let AccessDialogRequest {
            handle,
            app_id,
            parent_window,
            title,
            body,
        } = request;
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
        let request = handle.as_str().to_owned();
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
    fn camera_access_is_always_the_camera_capability_and_maps_responses() {
        // Phase `access_is_always_camera`.
        {
            let (kind, cap) = camera_capability();
            assert_eq!(kind, CapabilityKind::Camera);
            assert!(matches!(cap, Capability::Camera { .. }));
        }
        // (The former `translated_or_hostile_ui_text_cannot_change_the_capability` made exactly
        // these assertions: `camera_capability` takes no UI text, so nothing can change it.)
        // Phase `access_allow_returns_zero`.
        {
            let (code, results) = outcome_to_access_response(PromptOutcome::AllowOnce);
            assert_eq!(code, 0);
            assert!(results.is_empty());
        }
        // Phase `access_deny_returns_one`.
        {
            let (code, _) = outcome_to_access_response(PromptOutcome::Deny);
            assert_eq!(code, 1);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn access_dialog_decodes_exactly_the_advertised_arguments() {
        use zbus::zvariant::Type;
        let advertised =
            interface_dispatch::advertised_in_signature(ACCESS_INTROSPECTION, "AccessDialog");
        assert_eq!(
            format!("({advertised})"),
            <AccessDialogArgs as Type>::SIGNATURE.to_string()
        );
    }
}
