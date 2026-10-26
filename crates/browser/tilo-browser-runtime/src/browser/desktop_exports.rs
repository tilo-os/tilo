//! The desktop's page-facing calls: its navigation, its windows and
//! those its peers open, its host actions and the app requests it
//! raises. Each finds its mount and hands the call to the desktop.

use wasm_bindgen::prelude::*;

use super::{error, with_mount, with_mount_quietly};

/// The desktop's `DesktopNavigationV1` snapshot as JSON; `None` for a
/// single-application mount.
///
/// # Errors
///
/// Rejects a closed mount or a failure to serialize the snapshot.
#[wasm_bindgen]
pub fn desktop_navigation_snapshot(id: u64) -> Result<Option<String>, JsValue> {
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
        m.desktop
            .as_ref()
            .map(|d| {
                serde_json::to_string(&d.handle.snapshot(&d.generation, d.shared.catalogue()))
                    .map_err(error)
            })
            .transpose()
    })
}

/// Where the desktop shows each member's legacy window right now: `[{key,
/// rect: [x0, y0, x1, y1], focused}]` for windows whose active tab shows one
/// in the shown strip, in the surface's CSS pixels, `focused` for the
/// desktop's focused window; empty while search covers the desktop. `None`
/// for a single application.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn desktop_peer_windows(id: u64) -> Result<Option<String>, JsValue> {
    with_mount(id, |m| {
        m.desktop
            .as_ref()
            .map(|d| {
                let windows: Vec<serde_json::Value> = if d.handle.search_open() {
                    Vec::new()
                } else {
                    let focused = d.handle.read(|tree| {
                        tilo_desktop_surface::navigation::active_route(tree).map(|route| route.tab)
                    });
                    d.handle
                        .shown_tabs()
                        .into_iter()
                        .filter_map(|(tab, rect)| {
                            Some(serde_json::json!({
                                "key": d.shared.peer_window_of(tab)?,
                                "rect": [rect.x0, rect.y0, rect.x1, rect.y1],
                                "focused": focused == Some(tab),
                            }))
                        })
                        .collect()
                };
                serde_json::to_string(&windows).map_err(error)
            })
            .transpose()
    })
}

/// Focus the desktop window showing the legacy window `key`: the person
/// clicked into it, which the page handles, not the desktop.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_focus_peer_window(id: u64, key: &str) -> Result<(), JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let Some(tab) = desktop.shared.peer_window_tab(key) else {
            return Ok(());
        };
        if desktop.handle.focus_tab(tab) {
            desktop.shared.wake();
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(())
    })
}

/// List the legacy applications the node beside the host runs (`[{id,
/// title}]` JSON, `id` what the node opens): desktop search and the
/// navigation catalogue offer them, and opening one is handed to the page
/// ([`desktop_take_node_app_requests`]).
///
/// # Errors
///
/// Rejects a closed or single-application mount or malformed JSON.
#[wasm_bindgen]
pub fn desktop_set_node_apps(id: u64, apps: &str) -> Result<(), JsValue> {
    let apps: Vec<crate::desktop::NodeApp> = serde_json::from_str(apps).map_err(error)?;
    with_mount(id, |m| {
        m.desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?
            .shared
            .set_node_apps(apps);
        Ok(())
    })
}

/// Offer what only the page can do for the person (`[{id, title,
/// subtitle?}]` JSON; in place of earlier ones): desktop search lists them
/// beside the desktop's commands, and choosing one reaches the page as a
/// host request `{action: id}`.
///
/// # Errors
///
/// Rejects a closed or single-application mount or malformed JSON.
#[wasm_bindgen]
pub fn desktop_set_host_actions(id: u64, actions: &str) -> Result<(), JsValue> {
    let actions: Vec<tilo_desktop_surface::HostAction> =
        serde_json::from_str(actions).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        // An ask for an action no longer offered was answered some other way.
        let mut withdrew = false;
        for gone in desktop.handle.set_host_actions(actions) {
            withdrew |= desktop
                .persistence
                .notifications
                .withdraw_host_action(&gone);
        }
        if withdrew {
            m.window.request_redraw();
        }
        Ok(())
    })
}

/// Show a banner offering a host action (`{action: {id, title}, title,
/// body}` JSON) until the person chooses or dismisses it; choosing it
/// reaches the page as that action.
///
/// # Errors
///
/// Rejects a closed or single-application mount or malformed JSON.
#[wasm_bindgen]
pub fn desktop_offer_host_action(id: u64, offer: &str) -> Result<(), JsValue> {
    #[derive(serde::Deserialize)]
    struct Offer {
        action: tilo_desktop_surface::HostAction,
        title: String,
        #[serde(default)]
        body: String,
    }
    let offer: Offer = serde_json::from_str(offer).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        if offer.action.is_personal() {
            // About the person themselves: asked from their avatar in the
            // bar, never as a banner over the windows.
            desktop
                .handle
                .ask_personally(offer.action, offer.title, offer.body);
        } else {
            desktop
                .persistence
                .notifications
                .post(offer.action.notice(offer.title, offer.body));
        }
        m.window.request_redraw();
        Ok(())
    })
}

/// Confirm in passing what the page just did out of sight ("Link copied"):
/// a short-lived pill over the desktop, never a notification.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_acknowledge(id: u64, text: &str) -> Result<(), JsValue> {
    with_mount(id, |m| {
        m.desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?
            .handle
            .acknowledge(text);
        m.window.request_redraw();
        Ok(())
    })
}

/// The room's host answered the room action `request` the desktop asked for
/// (a `room-action` host request): `answer` is `{"ok": value}` or
/// `{"error": "why"}`.
///
/// # Errors
///
/// Rejects a closed or single-application mount, or an answer of neither
/// form.
#[wasm_bindgen]
pub fn desktop_room_action_answer(id: u64, request: u32, answer: &str) -> Result<(), JsValue> {
    let answer: serde_json::Value = serde_json::from_str(answer).map_err(error)?;
    let answer = match (answer.get("ok"), answer.get("error")) {
        (Some(value), None) => Ok(value.clone()),
        (None, Some(why)) => Err(why.as_str().unwrap_or("refused").to_owned()),
        _ => return Err(error("a room action answer is {ok} or {error}")),
    };
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        desktop.shared.room_actions.answer(request, answer);
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// What came of a direct message the people overlay asked the page to send
/// by Chat's action (`HostRequest::Send`, by `request`): `error` says why
/// it did not go, none that Chat took it.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn desktop_message_answer(id: u64, request: u32, error: Option<String>) -> Result<(), JsValue> {
    crate::shell::people::answer(request, error.map_or(Ok(()), Err));
    with_mount(id, |m| {
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Run the person's own ask `request` of Chat (a `message` or `send` host
/// request) on `resource`, Chat's conversation with them in `window`: as
/// the person's own act, which Chat's private actions require. An id the
/// person never asked, or a resource that is not their conversation with
/// whom they asked, runs nothing.
///
/// # Errors
///
/// Rejects a closed mount, an unknown ask, and as `invoke_component_action`.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives an optional JavaScript string only as an owned value"
)]
pub fn desktop_run_own_ask(
    id: u64,
    request: u32,
    resource: &str,
    window: Option<String>,
) -> Result<String, JsValue> {
    let ask = crate::shell::people::take_own_ask(request)
        .ok_or_else(|| error("the person asked nothing by that id"))?;
    if !resource.ends_with(&format!("chat.person:{}", ask.to)) {
        return Err(error("that is not their conversation with this person"));
    }
    super::invoke_as(
        id,
        resource,
        ask.action,
        &ask.arguments,
        window.as_deref(),
        super::Invoker::Person,
    )
}

/// The node applications the person asked to open since the last call (a
/// JSON array of their ids): the page asks the node to open each, and its
/// window arrives as a legacy window.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_take_node_app_requests(id: u64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let requests = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?
            .shared
            .take_node_app_requests();
        serde_json::to_string(&requests).map_err(error)
    })
}

/// Hand a painted window (`tilo-paint` stream) one reassembled frame of its
/// stream: a whole page, patches, or an image it references. Returns whether
/// what the window shows changed; the desktop then draws it again.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_peer_window_paint(id: u64, key: &str, frame: &[u8]) -> Result<bool, JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let changed = desktop.shared.paint_page(key).push(frame);
        if changed {
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(changed)
    })
}

/// Input the person gave painted windows since the last call, as JSON
/// `[{key, events}]` with `legacy_window::InputEvent`s in page pixels, for
/// the page to send to each stream's host.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_take_peer_window_input(id: u64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let input: Vec<serde_json::Value> = desktop
            .shared
            .take_paint_input()
            .into_iter()
            .map(|(key, events)| serde_json::json!({"key": key, "events": events}))
            .collect();
        serde_json::to_string(&input).map_err(error)
    })
}

/// The legacy windows the person closed since the last call (a JSON array
/// of their keys): the page asks their sources to close them, and the
/// desktop does not open them again while they still stream.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_dismissed_peer_windows(id: u64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let keys = desktop
            .handle
            .read(|tree| desktop.shared.take_dismissed_peer_windows(tree));
        serde_json::to_string(&keys).map_err(error)
    })
}

/// Show exactly `windows` (`[{key, app, title}]` JSON), the legacy windows
/// other members stream, as desktop windows: a new key opens a window
/// beside the focused one, a key no longer listed closes its window. The
/// page draws each stream over its window's content
/// ([`desktop_peer_windows`]).
///
/// # Errors
///
/// Rejects a closed or single-application mount or malformed JSON.
#[wasm_bindgen]
pub fn desktop_set_peer_windows(id: u64, windows: &str) -> Result<(), JsValue> {
    let wanted: Vec<crate::desktop::PeerWindow> = serde_json::from_str(windows).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let (launch, close) = desktop
            .handle
            .read(|tree| desktop.shared.reconcile_peer_windows(tree, wanted));
        if launch.is_empty() && close.is_empty() {
            return Ok(());
        }
        for tab in close {
            let route = desktop.handle.read(|tree| {
                tree.workspaces.iter().find_map(|workspace| {
                    workspace.strips.iter().find_map(|strip| {
                        strip
                            .window_groups
                            .iter()
                            .find(|group| group.tabs.iter().any(|t| t.id == tab))
                            .map(|group| tilo_desktop_surface::navigation::Route {
                                workspace: workspace.id.0.to_string(),
                                strip: strip.id.0.to_string(),
                                group: group.id.to_string(),
                                tab: tab.to_string(),
                            })
                    })
                })
            });
            if let Some(route) = route {
                let command = tilo_desktop_surface::navigation::Command::Close {
                    generation: desktop.generation.clone(),
                    route,
                };
                // A window the person already closed is gone either way.
                let _ = desktop.handle.dispatch(&command, &desktop.generation);
            }
            desktop.shared.closed_peer_window(tab);
        }
        for app in launch {
            let command = tilo_desktop_surface::navigation::Command::Launch {
                generation: desktop.generation.clone(),
                app,
            };
            desktop
                .handle
                .dispatch(&command, &desktop.generation)
                .map_err(error)?;
        }
        desktop.shared.wake();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Changes whenever the desktop tree or what the desktop presents beside it
/// (pan, scroll, history, activity) does, so the page republishes
/// navigation only when something moved. `0` for a single application.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn desktop_navigation_revision(id: u64) -> Result<f64, JsValue> {
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a revision counter stays far below 2^53 and is only compared for change"
        )]
        Ok(m.desktop
            .as_ref()
            .map_or(0.0, |d| d.handle.navigation_revision() as f64))
    })
}

/// Dispatch a `DesktopNavigationCommand` (JSON). Acceptance queues the
/// change for the next frame; the page observes it in later snapshots.
///
/// # Errors
///
/// Rejects a closed or single-application mount, a malformed command, a
/// stale generation, an absent target, or a command the desktop does not
/// perform.
#[wasm_bindgen]
pub fn desktop_navigate(id: u64, command: &str) -> Result<(), JsValue> {
    let command: tilo_desktop_surface::navigation::Command =
        serde_json::from_str(command).map_err(error)?;
    if let tilo_desktop_surface::navigation::Command::Launch { app, .. } = &command
        && crate::desktop::known_application(app).is_none()
    {
        return Err(error(format!("{app} is not available in the browser")));
    }
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop navigation unavailable"))?;
        desktop
            .handle
            .dispatch(&command, &desktop.generation)
            .map_err(error)?;
        desktop.shared.wake();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Open desktop search, or close it when open: the page's search shortcut
/// and search control. The frame that follows shows the palette.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_search_toggle(id: u64) -> Result<(), JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop search unavailable"))?;
        desktop.handle.toggle_search();
        desktop.shared.wake();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Replace the desktop's tree with one changed elsewhere (`DesktopTree`
/// JSON, for example from a peer). Tabs still present keep their running
/// applications; the change is saved to the profile like a local one.
///
/// # Errors
///
/// Rejects a closed or single-application mount or a malformed tree.
#[wasm_bindgen]
pub fn desktop_apply_tree(id: u64, tree: &str) -> Result<(), JsValue> {
    let tree = crate::desktop::parse_tree(tree).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        desktop.handle.replace_tree(tree);
        desktop.shared.wake();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}
