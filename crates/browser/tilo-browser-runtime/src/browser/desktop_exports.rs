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
                serde_json::to_string(
                    &d.handle
                        .snapshot(&d.generation, crate::desktop::catalogue()),
                )
                .map_err(error)
            })
            .transpose()
    })
}

/// Offer what only the page can do for the person (`[{id, title,
/// subtitle?}]` JSON; in place of earlier ones): desktop search lists them
/// beside the desktop's commands, and choosing one reaches the page as a
/// host request `{action: id}`. The empty desk's welcome verbs are the
/// desktop's own and are added here, so a page and a session offer the same
/// rows.
///
/// # Errors
///
/// Rejects a closed or single-application mount or malformed JSON.
#[wasm_bindgen]
pub fn desktop_set_host_actions(id: u64, actions: &str) -> Result<(), JsValue> {
    let mut actions: Vec<tilo_desktop_surface::HostAction> =
        serde_json::from_str(actions).map_err(error)?;
    actions.extend(tilo_desktop::launchpad::LaunchpadAction::host_actions(
        tilo_desktop::launchpad::WorkspaceScope::default(),
    ));
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

/// The workspace's host answered the workspace action `request` the desktop asked for
/// (a `room-action` host request): `answer` is `{"ok": value}` or
/// `{"error": "why"}`.
///
/// # Errors
///
/// Rejects a closed or single-application mount, or an answer of neither
/// form.
#[wasm_bindgen]
pub fn desktop_record_action_answer(id: u64, request: u32, answer: &str) -> Result<(), JsValue> {
    let answer: serde_json::Value = serde_json::from_str(answer).map_err(error)?;
    let answer = match (answer.get("ok"), answer.get("error")) {
        (Some(value), None) => Ok(value.clone()),
        (None, Some(why)) => Err(why.as_str().unwrap_or("refused").to_owned()),
        _ => return Err(error("a record action answer is {ok} or {error}")),
    };
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        desktop.shared.record_actions.answer(request, answer);
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

/// Dispatch a verb (JSON: its effect id and arguments). Acceptance queues
/// the change for the next frame; the page observes it in later snapshots.
///
/// # Errors
///
/// Rejects a closed or single-application mount, a malformed verb, an
/// absent target, or a verb the desktop does not perform.
#[wasm_bindgen]
pub fn desktop_navigate(id: u64, command: &str) -> Result<(), JsValue> {
    use tilo_desktop_surface::navigation::{ChromeEffect, Verb};
    let verb = Verb::from_payload(command.as_bytes()).map_err(error)?;
    if let Verb::Chrome(ChromeEffect::OpenApp { app, .. }) = &verb
        && crate::desktop::known_application(app).is_none()
    {
        return Err(error(format!("{app} is not available in the browser")));
    }
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop navigation unavailable"))?;
        desktop.handle.dispatch(verb).map_err(error)?;
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
