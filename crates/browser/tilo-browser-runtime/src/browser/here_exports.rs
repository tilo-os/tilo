//! The desktop's calls about who is here: the people of its live layer and
//! their photos, what the host and each person say they are doing, who
//! watches, and the colour a person is drawn in. Each finds its mount and
//! hands the call to the desktop's here state.

use wasm_bindgen::prelude::*;

use super::{error, with_mount, with_mount_quietly};

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

/// The picture `user` (root key, hex) chose for their profile, decoded by
/// the page into `width` × `height` straight-alpha RGBA pixels; empty
/// pixels take it away. Every avatar of theirs wears it from the next frame.
///
/// # Errors
///
/// Rejects pixels that do not fill the size, a picture larger than a
/// profile's, or a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_set_person_photo(
    id: u64,
    user: &str,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> Result<(), JsValue> {
    /// The largest side a profile picture is drawn from.
    const MAX_SIDE: u32 = 256;
    let image = if rgba.is_empty() {
        None
    } else {
        let expected = usize::try_from(width.saturating_mul(height).saturating_mul(4)).ok();
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(error("a profile picture is at most 256 pixels square"));
        }
        if expected != Some(rgba.len()) {
            return Err(error("the pixels do not fill the picture"));
        }
        Some(ui::peniko::ImageData {
            data: ui::peniko::Blob::new(std::sync::Arc::new(rgba)),
            format: ui::peniko::ImageFormat::Rgba8,
            alpha_type: ui::peniko::ImageAlphaType::Alpha,
            width,
            height,
        })
    };
    ui::people_photos::set(user, image);
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        desktop.here.borrow_mut().photos_changed();
        m.apply_here();
        Ok(())
    })
}

/// The accounts elsewhere `user`'s (root key, hex) profile names, as JSON
/// `[{service, handle}]`: their card says "Linked to @handle on Discord".
///
/// # Errors
///
/// Rejects malformed JSON or a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_set_person_links(id: u64, user: &str, linked: &str) -> Result<(), JsValue> {
    let linked: Vec<tilo_app::linked_accounts::ProfileLink> =
        serde_json::from_str(linked).map_err(error)?;
    // Only what a card says: the pictures are the profile's own.
    let linked = linked
        .into_iter()
        .take(4)
        .map(|link| tilo_app::linked_accounts::ProfileLink {
            picture: None,
            ..link
        })
        .collect();
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        if desktop.here.borrow_mut().set_linked(user, linked) {
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(())
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

/// What the person's profile says they are doing (`status`, empty for
/// nothing) and how they share it (`automatic`, `manual` or `off`; anything
/// else leaves it as it is): what they set on another of their devices.
///
/// # Errors
///
/// Rejects a mount without a desktop.
#[wasm_bindgen]
pub fn desktop_set_own_status(id: u64, status: &str, sharing: &str) -> Result<(), JsValue> {
    with_mount(id, |m| {
        let desktop = m
            .desktop
            .as_ref()
            .ok_or_else(|| error("Desktop unavailable"))?;
        let mut here = desktop.here.borrow_mut();
        let before = here.revision();
        here.set_own_words(Some(status.to_owned()));
        if let Some(sharing) = crate::shell::sharing_of(sharing) {
            here.set_sharing(sharing);
        }
        if here.revision() != before {
            m.pending = true;
            m.window.request_redraw();
        }
        Ok(())
    })
}

/// What this page's person is doing, for the page to post to `/here`:
/// `{window: hex | null, share, actions: [{id, action, resource, app,
/// at_ms}], status}`: the focused window's tab id, their own steps in the active workspace since
/// `since_ms` (titles only; none unless they share an automatic status) and their own words for
/// their live presence (`null` while they said nothing or share nothing).
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
            "status": desktop.here.borrow().shared_words(),
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
