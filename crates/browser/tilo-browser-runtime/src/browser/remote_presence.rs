//! Other people here, as the page's live layer reports them: their carets
//! and selections, their names, and the windows they are in. All of it
//! arrives many times a frame while people type and move; the next frame
//! lays it out once.

use ui::permissions::UserId;
use wasm_bindgen::prelude::*;

use super::{Mount, with_mount};

impl Mount {
    /// Who is here changed: the next frame puts them on the windows they
    /// are in. Presence arrives many times a frame while people type and
    /// move, so it is left to that frame, never laid out on arrival.
    pub(super) fn apply_here(&mut self) {
        self.here_stale = true;
        self.pending = true;
        self.window.request_redraw();
    }

    /// Put who is here on the windows they are in, as the frame begins.
    /// Remote carets clear a member's registrations as they move, so this
    /// runs in each frame after one arrived. The frame lays the change out.
    pub(super) fn place_here(&mut self) {
        if !std::mem::take(&mut self.here_stale) {
            return;
        }
        let Some(desktop) = &self.desktop else {
            return;
        };
        let tree = desktop.handle.read(Clone::clone);
        let people = crate::shell::here_people(&desktop.here, &desktop.shared, &tree);
        let mut here = desktop.here.borrow_mut();
        self.frame.update_presence(|presence| {
            here.show_waves(presence);
            here.register_windows(&people, presence);
            true
        });
    }
}

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
        m.frame.update_presence(|presence| {
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
            true
        });
        m.apply_here();
        Ok(())
    })
}

/// A member left the room: forget its carets and name.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn remote_presence_gone(id: u64, publisher: &str) -> Result<(), JsValue> {
    with_mount(id, |m| {
        m.frame.update_presence(|presence| {
            presence.unregister_peer(publisher);
            true
        });
        m.apply_here();
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
        m.frame.update_presence(|presence| {
            for person in people {
                let name = person.name.trim();
                if !person.id.is_empty() && !name.is_empty() {
                    presence.set_display_name(UserId(person.id), name);
                }
            }
            true
        });
        m.apply_here();
        Ok(())
    })
}
