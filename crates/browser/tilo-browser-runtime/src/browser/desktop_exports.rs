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

/// Who holds the input on the desk, and the field an input method is helping
/// (`{owner, holder, ime}` JSON). `owner` is the keyboard owner path of the
/// semantics tree the page holds, which the page matches its own accessible
/// elements against; `holder` is who that is — `"system-search"` while the
/// desk's search is up, else the application of the focused window, `null`
/// while no window has it; `ime` is the field an input method is helping,
/// `null` while none is. `None` for a single-application mount, whose page
/// holds its own input.
///
/// The page reads it live, not from a status: routing a press on the page's own
/// chrome and handing focus back after the desk activates something both need
/// who holds the keyboard now. The owner path and the field are the last
/// presented frame's, as the status reports them — the same tree the page
/// holds, and a tree a keystroke retired between frames names no keyboard
/// owner, so reading a fresher one would move the page's focus on every key.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn desktop_input_ownership(id: u64) -> Result<Option<String>, JsValue> {
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
        let owner = m.semantics.borrow().published_keyboard_owner();
        Ok(m.desktop.as_ref().map(|desktop| {
            serde_json::json!({
                "owner": owner,
                "holder": desktop.handle.input_owner(),
                "ime": m.presented_ime.as_ref().map(super::ime_json),
            })
            .to_string()
        }))
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
    // The page's welcome verbs are the cards it draws: the ones whose apps this page carries.
    actions.extend(
        tilo_desktop::launchpad::LaunchpadAction::ALL
            .into_iter()
            .filter(|card| card.offered(|app| crate::desktop::known_application(app).is_some()))
            .map(|card| card.host_action(tilo_desktop::launchpad::WorkspaceScope::default())),
    );
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

/// Close desktop search wherever it stands, and nothing when none is open: the
/// page dismissing the search that holds the keyboard after a press on its own
/// chrome, which the desk's hit test never reaches.
///
/// # Errors
///
/// Rejects a closed or single-application mount.
#[wasm_bindgen]
pub fn desktop_search_close(id: u64) -> Result<(), JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop search unavailable"))?;
        desktop.handle.close_search();
        desktop.shared.wake();
        m.pending = true;
        m.window.request_redraw();
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
            desktop.persistence.notifications.post(
                tilo_session_core::notice::Notice::asks(&offer.action, offer.title, offer.body)
                    .shown(),
            );
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
