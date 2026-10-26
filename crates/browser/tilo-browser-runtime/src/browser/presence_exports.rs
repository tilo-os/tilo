//! Who is here, as the page's live layer and door tell it: members' carets
//! and selections, the room's people and their names, what each says they
//! are doing, and who watches. Each finds its mount and hands the call to
//! its frame or desktop.

use ui::permissions::UserId;
use wasm_bindgen::prelude::*;

use super::{error, with_mount, with_mount_quietly};

/// A room member's presence from the page's live layer: `publisher` (the
/// member's device, hex) with its participant `label` and `kind` (`person`
/// or `agent`), and its caret or selection as the live anchors (hex) it
/// published, or none when it has no selection. Editors draw the caret,
/// labelled, in the next frame. Its colour is the room's to settle
/// ([`tilo_desktop::here::HereState::colors`]), never the one it publishes,
/// so two people here at once are never drawn alike.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives optional JavaScript strings only as owned values"
)]
pub fn remote_presence(
    id: u64,
    publisher: &str,
    label: &str,
    kind: &str,
    anchor: Option<String>,
    head: Option<String>,
) -> Result<(), JsValue> {
    use ui::collaboration::PresencePayload;
    let range = anchor.zip(head).and_then(|(anchor, head)| {
        let anchor = tilo_relay_protocol::live::LiveAnchor {
            cursor: hex::decode(anchor).ok()?,
        };
        let head = tilo_relay_protocol::live::LiveAnchor {
            cursor: hex::decode(head).ok()?,
        };
        tilo_browser_peer::transport::read_presence_anchors(&anchor, &head)
    });
    with_mount(id, |m| {
        let user = UserId(publisher.to_owned());
        let presence = m.frame.presence_mut();
        presence.set_participant_kind(&user, kind == "agent");
        if !label.is_empty() {
            presence.set_display_name(user, label);
        }
        match range {
            Some((key, start, end)) => {
                let payload = if start == end {
                    PresencePayload::Caret { pos: start }
                } else {
                    PresencePayload::Selection { start, end }
                };
                presence.apply_remote_presence(publisher.to_owned(), key, payload);
            }
            None => presence.unregister_peer(publisher),
        }
        m.apply_here();
        m.frame.relayout();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Who is in the room's live layer now: `{own: {publisher, label, user},
/// members: [{publisher, kind, label, focus, activity, place}] | null}`,
/// `members` null while the layer is not live. The bar, the People popover
/// and the windows show them, each in the colour the room settles for them;
/// nothing is written to the room.
///
/// # Errors
///
/// Rejects malformed JSON or a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_set_people(id: u64, people: &str) -> Result<(), JsValue> {
    #[derive(serde::Deserialize)]
    struct People {
        own: Option<tilo_desktop::here::LiveMember>,
        members: Option<Vec<tilo_desktop::here::LiveMember>>,
    }
    let people: People = serde_json::from_str(people).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        if desktop
            .here
            .borrow_mut()
            .set_members(people.own, people.members)
        {
            desktop.announce_arrivals();
            // The bar reads the strips' counts in its update pass, which a
            // bare redraw skips: without the wake it would paint them only
            // on its next other change, such as the clock's minute.
            desktop.shared.wake();
            m.apply_here();
        }
        Ok(())
    })
}

/// Who the desktop shows as here, as its People list does: `[{id, label,
/// agent}]`, one row per person however many devices they are here from,
/// never this page's own person. `None` for a single application.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn desktop_here_people(id: u64) -> Result<Option<String>, JsValue> {
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
        m.desktop
            .as_ref()
            .map(|d| {
                let tree = d.handle.read(Clone::clone);
                let people: Vec<serde_json::Value> = crate::shell::here_people(&d.here, &d.shared, &tree)
                    .into_iter()
                    .map(|person| serde_json::json!({"id": person.id, "label": person.label, "agent": person.agent}))
                    .collect();
                serde_json::to_string(&people).map_err(error)
            })
            .transpose()
    })
}

/// The host's answer to `/here` (`peer.here`): each person's line.
///
/// # Errors
///
/// Rejects malformed JSON or a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_set_here(id: u64, lines: &str) -> Result<(), JsValue> {
    let lines: Vec<tilo_desktop::here::HostLine> = serde_json::from_str(lines).map_err(error)?;
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        if desktop
            .here
            .borrow_mut()
            .set_lines(lines, crate::shell::now_ms())
        {
            // A line can name someone whose presence arrived nameless.
            desktop.announce_arrivals();
            m.apply_here();
        }
        Ok(())
    })
}

/// How many spectators follow the room without taking part.
///
/// # Errors
///
/// Rejects a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_set_watching(id: u64, watching: u32) -> Result<(), JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        if desktop.here.borrow_mut().set_watching(watching) {
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(())
    })
}

/// What this page's person is doing, for the page to post to `/here`:
/// `{window: hex | null, share, actions: [{id, action, resource, app,
/// at_ms}]}`. `window` is the focused window's tab id; `actions` are their own steps in the active workspace's records since
/// `since_ms` (titles only), and empty unless they share an automatic
/// status (`share`).
///
/// # Errors
///
/// Rejects a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_here_tick(id: u64, since_ms: f64) -> Result<String, JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let tree = desktop.handle.read(Clone::clone);
        let focused = tree.active_workspace().and_then(|workspace| {
            // A person who has not moved yet is shown the first strip, as
            // the composition draws it.
            let strip = tree
                .active_strip
                .get(&workspace.id)
                .and_then(|id| workspace.strips.iter().find(|s| s.id == *id))
                .or_else(|| workspace.strips.first())?;
            let group = tree
                .active_group_for(strip.id)
                .and_then(|g| strip.window_groups.iter().find(|w| w.id == g))
                .or_else(|| strip.window_groups.first())?;
            group.tabs.iter().find(|tab| tab.id == group.active_tab)
        });
        let window = focused.map(|tab| format!("{:x}", tab.id));
        let share = desktop.here.borrow().sharing() == ui::StatusSharing::Automatic;
        let actions = if share {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a page clock in milliseconds; negative clamps to zero"
            )]
            let since = since_ms.max(0.0) as u64;
            crate::history::own_recent_actions(&desktop.shared, &tree, &desktop.author, since)
        } else {
            Vec::new()
        };
        serde_json::to_string(&serde_json::json!({
            "window": window,
            "share": share,
            "actions": actions,
        }))
        .map_err(error)
    })
}

/// The colour presence draws the identity `id` in when nothing settled
/// another one for it, as `#rrggbb`: [`ui::presence::user_color`], the one
/// the native desktop uses, so a page never computes a colour of its own.
#[wasm_bindgen(js_name = presenceColor)]
#[must_use]
pub fn presence_color(id: &str) -> String {
    let (r, g, b) = ui::presence::user_color(id);
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// A member left the room: forget its carets and name.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn remote_presence_gone(id: u64, publisher: &str) -> Result<(), JsValue> {
    with_mount(id, |m| {
        m.frame.presence_mut().unregister_peer(publisher);
        m.apply_here();
        m.frame.relayout();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}

/// Name the people of the mount's room, `[{id, name}]`, `id` each person's
/// key as their actions record it (what `cx.me()` and `ActionCx::author`
/// answer). Live presence names devices; a page that knows who belongs to
/// the room (its door admitted them) names the people, so an app shows
/// `cx.people().name(author)` as the person's name rather than their key.
///
/// # Errors
///
/// Rejects malformed JSON or a closed mount.
#[wasm_bindgen]
pub fn set_people(id: u64, people: &str) -> Result<(), JsValue> {
    #[derive(serde::Deserialize)]
    struct Person {
        id: String,
        name: String,
    }
    let people: Vec<Person> =
        serde_json::from_str(people).map_err(|error| JsValue::from_str(&error.to_string()))?;
    with_mount(id, |m| {
        let presence = m.frame.presence_mut();
        for person in people {
            let name = person.name.trim();
            if !person.id.is_empty() && !name.is_empty() {
                presence.set_display_name(UserId(person.id), name);
            }
        }
        m.apply_here();
        m.frame.relayout();
        m.pending = true;
        m.window.request_redraw();
        Ok(())
    })
}
