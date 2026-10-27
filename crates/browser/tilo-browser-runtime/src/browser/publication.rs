//! What the page and the accessibility projection ask of one mount: its
//! input events, the semantics tree they read, and the actions its
//! application publishes to the palette, agents and assistive technology.
//! Each call finds its mount, does the one thing it names, and asks for the
//! frame that lays out what it changed.

use js_sys::Function;
use wasm_bindgen::prelude::*;
use winit::event::WindowEvent;

use super::{Mount, error, with_mount, with_mount_quietly};

/// Bounded exact publication request; never a presence or action invocation.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SemanticPresentationTarget {
    pub(super) app: String,
    pub(super) instance: u64,
    pub(super) record: String,
    pub(super) resource: String,
    pub(super) action: String,
}

#[wasm_bindgen]
pub fn ime(id: u64, kind: &str, text: &str) -> Result<(), JsValue> {
    let event = match kind {
        "start" => winit::event::Ime::Enabled,
        "preedit" => winit::event::Ime::Preedit(text.into(), Some((text.len(), text.len()))),
        "commit" => winit::event::Ime::Commit(text.into()),
        "end" => winit::event::Ime::Disabled,
        _ => return Err(error("invalid IME event")),
    };
    with_mount(id, |m| {
        let event = WindowEvent::Ime(event);
        m.frame.send_host_event(&event);
        m.publish_committed(&event);
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Perform an accessibility action from the semantics DOM projection.
///
/// # Errors
///
/// Rejects an unsupported action, a closed mount, or a refused action.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives an optional JavaScript string only as an owned value"
)]
pub fn semantic(
    id: u64,
    path: &str,
    target: Option<String>,
    action: &str,
    value: &str,
    anchor: usize,
    focus: usize,
) -> Result<bool, JsValue> {
    use ui::a11y::Action;
    let action = match action {
        "focus" => Action::Focus,
        "activate" => Action::Activate,
        "toggle" => Action::ToggleCheck,
        "value" => Action::SetValue(value.into()),
        "selection" => Action::SetSelection { anchor, focus },
        _ => return Err(error("unsupported semantic action")),
    };
    with_mount(id, |m| {
        crate::clipboard::during_input(&m.status, || {
            m.frame
                .perform_semantic_named(path, target.as_deref(), &action)
        })
        .map_err(error)?;
        m.pending = true;
        m.window.request_redraw();
        Ok(true)
    })
}

/// The actions the mount's application publishes, described as
/// `tilo-control actions list` describes them. `resource` narrows to one
/// resource and its children, including one the app resolves lazily. On a
/// desktop, the actions of one window's application: `window` (its tab id),
/// else the focused window's.
///
/// # Errors
///
/// Rejects a closed mount, an unknown or absent window, or an unavailable
/// application.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives an optional JavaScript string only as an owned value"
)]
pub fn component_actions(
    id: u64,
    resource: Option<String>,
    window: Option<String>,
) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let instance = m.action_window(window.as_deref())?;
        let resources = m
            .frame
            .bound_resources_of(instance, resource.as_deref())
            .map_err(|e| door_error(&e))?;
        serde_json::to_string(&tilo_actions::door::survey(&resources)).map_err(error)
    })
}

/// Publish component aliases through the canonical native publication namespace.
/// Existing `component_actions` retains its domain-address compatibility.
///
/// # Errors
/// Refuses closed mounts and unavailable component publications.
#[wasm_bindgen]
pub fn component_publications(id: u64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let resources = m
            .frame
            .published_bound_resources_of(None)
            .map_err(|e| door_error(&e))?;
        serde_json::to_string(&tilo_actions::door::survey(&resources)).map_err(error)
    })
}

/// Native catalogue requests, answered by the page's existing certified peer.
/// Shares the retained request collector with the local media lane.
/// # Errors
/// Refuses closed mounts or unencodable requests.
#[wasm_bindgen]
pub fn component_catalogue_requests(id: u64) -> Result<String, JsValue> {
    // The page asks every frame; taking the requests changes nothing the
    // tree shows (an answer arrives through a call of its own).
    with_mount_quietly(id, |m| {
        let requests = m.media.take_catalogue_requests(&mut m.frame);
        serde_json::to_string(&requests).map_err(error)
    })
}

/// Deliver one certified catalogue answer to this live native app mount.
/// The app fences the originating request, current resource and timeout.
/// # Errors
/// Refuses closed mounts, oversized or non-catalogue answers.
#[wasm_bindgen]
pub fn component_catalogue_response(id: u64, response: &str) -> Result<(), JsValue> {
    if response.len() > tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES {
        return Err(error("catalogue response exceeds its wire budget"));
    }
    let message: tilo_actions::remote::RemoteActionMessage =
        serde_json::from_str(response).map_err(error)?;
    if !matches!(
        message,
        tilo_actions::remote::RemoteActionMessage::CatalogueResponse { .. }
            | tilo_actions::remote::RemoteActionMessage::Receipt(_)
    ) {
        return Err(error("not a catalogue response or receipt"));
    }
    with_mount(id, |m| {
        // This only reports the originating command's actual system transfer.
        // The browser's own copy command still refuses absent/expired permission.
        crate::clipboard::during_input(&m.status, || {
            m.frame
                .handle_collaboration_event(&ui::collaboration::CollabEvent::RemoteAction {
                    message,
                });
        });
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Register/remove an exact read-only target for the next successful presentation.
/// Requests are local observations and do not establish authenticated admission.
///
/// # Errors
/// Refuses malformed/oversized targets, desktops and more than 32 queued targets.
#[wasm_bindgen]
pub fn request_component_locus(id: u64, target: &str, observe: bool) -> Result<(), JsValue> {
    if target.len() > 17_000 {
        return Err(error("semantic target exceeds size bound"));
    }
    let target: SemanticPresentationTarget = serde_json::from_str(target).map_err(error)?;
    if target.instance > 9_007_199_254_740_991
        || [
            &target.app,
            &target.record,
            &target.resource,
            &target.action,
        ]
        .into_iter()
        .any(|s| s.is_empty() || s.len() > 4096)
    {
        return Err(error("invalid semantic target"));
    }
    with_mount(id, |m| {
        if m.desktop.is_some() {
            return Err(error("semantic presentation requires a single-app mount"));
        }
        m.semantic_targets.retain(|t| t != &target);
        if observe {
            if m.semantic_targets.len() >= 32 {
                return Err(error("semantic target capacity exceeded"));
            }
            m.semantic_targets.push(target);
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(())
    })
}

/// Invoke one action the mount's application publishes, through the bound
/// door the palette, agents and assistive technology use: the address must
/// be published now, the mount's permissions must allow it, its arguments
/// must bind, and a confirmation policy requires `confirmed`. `window` is
/// as in [`component_actions`].
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives an optional JavaScript string only as an owned value"
)]
pub fn invoke_component_action(
    id: u64,
    resource: &str,
    action: &str,
    arguments: &str,
    confirmed: bool,
    window: Option<String>,
) -> Result<String, JsValue> {
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(arguments).map_err(error)?;
    let by = if confirmed {
        Invoker::Confirmed
    } else {
        Invoker::Door
    };
    invoke_as(id, resource, action, &arguments, window.as_deref(), by)
}

/// Who an action call from the page is.
#[derive(Clone, Copy)]
pub(super) enum Invoker {
    /// Whoever calls the bound door.
    Door,
    /// The door's caller, having confirmed this exact request.
    Confirmed,
    /// The person themself, by a control they pressed in the desktop's own
    /// chrome.
    Person,
}

pub(super) fn invoke_as(
    id: u64,
    resource: &str,
    action: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
    window: Option<&str>,
    by: Invoker,
) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let instance = m.action_window(window)?;
        let address = ui::bound_dispatch::BoundActionAddress {
            instance_id: instance
                .or_else(|| m.frame.component_instance_id())
                .ok_or_else(|| error("the mount hosts no application"))?,
            address: tilo_actions::ActionAddress::new(resource, action),
        };
        let started = match by {
            Invoker::Door => m.frame.invoke_bound_action(&address, arguments),
            Invoker::Confirmed => m.frame.invoke_bound_action_confirmed(&address, arguments),
            Invoker::Person => m.frame.invoke_bound_action_by_person(&address, arguments),
        }
        .map_err(|e| door_error(&e))?;
        // A following call sees the state this one left, not the last frame's.
        m.frame.relayout();
        m.pending = true;
        m.window.request_redraw();
        Ok(match started {
            ui::host_operation::ResourceOperationStart::Completed(output) => {
                serde_json::json!({"state": "completed", "output": output})
            }
            ui::host_operation::ResourceOperationStart::Accepted => {
                serde_json::json!({"state": "accepted"})
            }
        }
        .to_string())
    })
}

fn door_error(e: &tilo_actions::DoorError) -> JsValue {
    error(format!("{}: {e}", e.code()))
}

impl Mount {
    /// The application instance a page's action call addresses: `None` for
    /// a single-application mount, a desktop's `window` (tab id) or else its
    /// focused window.
    fn action_window(&self, window: Option<&str>) -> Result<Option<u64>, JsValue> {
        let Some(desktop) = &self.desktop else {
            return match window {
                None => Ok(None),
                Some(_) => Err(error("a single-application mount has no windows")),
            };
        };
        let tab = match window {
            Some(window) => window
                .parse::<u64>()
                .ok()
                .filter(|tab| tab.to_string() == window)
                .ok_or_else(|| error("invalid window identity"))?,
            None => desktop
                .handle
                .read(tilo_desktop_surface::navigation::active_route)
                .map(|route| route.tab)
                .ok_or_else(|| error("no window is focused"))?,
        };
        Ok(Some(tab))
    }
}

/// The next status carries the complete semantics tree, for example when
/// assistive technology focus enters the surface.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn request_semantics(id: u64) -> Result<(), JsValue> {
    with_mount(id, |m| {
        m.semantics.borrow_mut().request();
        m.publish(id);
        Ok(())
    })
}

/// What a presented frame changed, which decides how much of its status
/// has to be worked out again ([`Mount::publish_frame`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameChange {
    /// It laid nothing out: a reader sees what the last frame showed.
    Nothing,
    /// It only re-applied positions: laid-out content moved, and the
    /// tree's structure and focus are the last frame's.
    Moved,
    /// Anything else, or not known.
    Other,
}

pub(super) fn emit_text(f: &Function, text: &str) {
    let _ = f.call1(&JsValue::NULL, &JsValue::from_str(text));
}
