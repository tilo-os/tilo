//! Who is here, drawn where they are: the workspace's presence board, read by
//! the mount that draws the members' pointers, their names and carets, and
//! the desktop's roster. The board merges every member's word
//! ([`crate::peer`]); the mount only places what it holds in the tiles it
//! shows, and animates what arrived: a cursor-chat bubble, a reaction.
//!
//! Placement runs at the start of every frame and after each drawn one, so a
//! pointer follows a window the person moved here. Frames are asked for only
//! while a pointer may still be moving, a bubble or reaction is animating, or
//! a silent member's pointer is about to leave the screen.

use tilo_desktop::here::{Focus, LiveMember, MemberLayout, MemberWindow, Wave};
use ui::permissions::UserId;
use ui::presence::PresenceKey;
use wasm_bindgen::prelude::*;

use crate::peer_presence::{BoardView, JsSelection, Present};

use super::{Mount, error, with_mount_quietly};

/// Keep drawing this long after the newest pointer was heard, so it eases to
/// rest instead of stopping mid-move.
const SETTLE_MS: f64 = 250.0;
/// A pointer whose publisher said nothing for this long is no longer shown: a
/// member here restates itself every keepalive, so a longer silence is a page
/// that left before the workspace noticed.
const POINTER_STALE_MS: f64 = tilo_live::presence::KEEPALIVE_MS + 1000.0;

/// A member of the board as the desktop's roster reads them.
fn member_of(present: &Present) -> LiveMember {
    LiveMember {
        publisher: present.publisher.clone(),
        kind: if present.agent { "agent" } else { "person" }.into(),
        label: present.label.clone(),
        device: present.device.clone(),
        focus: present.focus.as_ref().map(|focus| Focus {
            app: focus.app.clone(),
            record: focus.record.clone(),
        }),
        activity: present.activity.clone(),
        place: present.place.clone(),
        user: present.user.clone(),
        wave: present.wave.as_ref().map(|wave| Wave {
            seq: wave.seq,
            target: wave.target.clone(),
        }),
        layout: present.layout.as_ref().map(|layout| MemberLayout {
            size: layout.size,
            windows: layout
                .windows
                .iter()
                .map(|window| MemberWindow {
                    app: window.app.clone(),
                    record: window.record.clone(),
                    rect: window.rect,
                    focused: window.focused,
                })
                .collect(),
        }),
        following: present.following.clone(),
        availability: Some(present.availability.clone()),
        call: present.call.clone(),
    }
}

/// This page's own word, as the desktop's roster reads it (it names the
/// person's own device apart from anyone else's).
fn own_member(view: &BoardView) -> LiveMember {
    let own = &view.own;
    let shell = own.shell.as_deref();
    LiveMember {
        publisher: view.publisher.clone(),
        kind: if shell.is_some_and(|shell| shell.agent) {
            "agent"
        } else {
            "person"
        }
        .into(),
        label: own.label.clone(),
        device: own.device.clone(),
        focus: shell
            .and_then(|shell| shell.windows.iter().find(|window| window.focused))
            .map(|window| Focus {
                app: window.app.clone(),
                record: hex::encode(window.record),
            }),
        activity: shell.and_then(|shell| shell.activity.clone()),
        place: shell.and_then(|shell| shell.place.clone()),
        user: own.user.clone(),
        wave: own.wave.as_ref().map(|wave| Wave {
            seq: wave.seq,
            target: wave.target.clone(),
        }),
        layout: shell.map(|shell| MemberLayout {
            size: [
                u16::try_from(shell.size[0]).unwrap_or(u16::MAX),
                u16::try_from(shell.size[1]).unwrap_or(u16::MAX),
            ],
            windows: shell
                .windows
                .iter()
                .map(|window| MemberWindow {
                    app: window.app.clone(),
                    record: hex::encode(window.record),
                    rect: window.rect,
                    focused: window.focused,
                })
                .collect(),
        }),
        following: own.following.clone(),
        availability: Some(own.availability.id().to_owned()),
        call: own.call.as_ref().map(|call| call.call.encode()),
    }
}

impl Mount {
    /// Who is here changed: the next frame puts them on the windows they are
    /// in. Presence arrives many times a frame while people type and move,
    /// so it is left to that frame, never laid out on arrival.
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

    /// The tiles this mount shows, where the workspace's pointers land.
    fn tiles(&self) -> Vec<tilo_live::presence::Tile> {
        self.desktop.as_ref().map_or_else(Vec::new, |desktop| {
            desktop
                .handle
                .shown_tabs()
                .into_iter()
                .map(|(tab, area)| {
                    tilo_live::presence::Tile::new(
                        tab,
                        area.x0,
                        area.y0,
                        area.width(),
                        area.height(),
                    )
                })
                .collect()
        })
    }

    /// Who is here, placed: the members' pointers over the tiles this mount
    /// shows and everything drawn beside them, the desktop's roster, and the
    /// names, kinds and carets their presence carries. Runs at the start of
    /// every frame and after each drawn one, so a pointer follows a window
    /// that moved here, and asks for the frames its animation needs.
    pub(super) fn place_presence(&mut self) {
        let Some(peer) = self.peer else {
            return;
        };
        let workspace = self.workspace.clone();
        let tiles = self.tiles();
        let now = Self::now_ms();
        let Some(view) = crate::peer_presence::view(peer, &workspace, &tiles, now) else {
            return;
        };
        let changed = view.revision != self.presence.revision;
        if changed {
            self.presence.revision = view.revision;
            self.here_from(&view);
            self.names_from(&view);
        }
        let mut settling = false;
        let mut leaving = f64::INFINITY;
        let mut cursors = Vec::new();
        let presence = self.frame.presence();
        for (publisher, _tab, x, y, heard) in &view.placed {
            let age = now - heard;
            let publisher = hex::encode(publisher);
            if age > POINTER_STALE_MS {
                continue;
            }
            leaving = leaving.min(heard + POINTER_STALE_MS);
            self.presence.motion_until = self.presence.motion_until.max(heard + SETTLE_MS);
            let Some(member) = view
                .members
                .iter()
                .find(|member| member.publisher == publisher)
            else {
                continue;
            };
            let chat = self
                .presence
                .chat(
                    &publisher,
                    member
                        .chat
                        .as_ref()
                        .map(|chat| (chat.text.as_str(), chat.sent)),
                    now,
                )
                .map(|(text, typing, opacity)| ui::presence::ChatBubble {
                    text,
                    typing,
                    opacity,
                });
            let reaction = self
                .presence
                .reaction(
                    &publisher,
                    member
                        .reaction
                        .as_ref()
                        .map(|reaction| (reaction.seq, reaction.emoji.as_str())),
                    now,
                )
                .map(|(emoji, rise, opacity)| ui::presence::ReactionFloat {
                    emoji,
                    rise,
                    opacity,
                });
            settling |= self.presence.animating(&publisher, now);
            cursors.push(presence.placed_cursor(ui::presence::PlacedPointer {
                user: UserId(publisher),
                at: (*x, *y),
                name: Some(member.label.clone()),
                agent: member.agent,
                activity: member.activity.clone(),
                chat,
                reaction,
            }));
        }
        if self.frame.set_placed_pointers(cursors) {
            self.pending = true;
            self.window.request_redraw();
        }
        // A pointer still easing or an animation running: the next frame
        // draws a change, so ask for it.
        if settling || self.presence.settling(now) {
            self.pending = true;
            self.window.request_redraw();
        }
        // A pointer whose member went silent leaves the screen at the end of
        // its keepalive window, and no arrival will come to notice it.
        if leaving.is_finite() {
            self.wake_at(self.id, leaving, now);
        }
    }

    /// Ask for a frame at `at_ms`, when a silent member's pointer has to
    /// leave the screen.
    fn wake_at(&mut self, id: u64, at_ms: f64, now: f64) {
        if self
            .presence
            .wake_at
            .is_some_and(|asked| (asked - at_ms).abs() < 1.0)
        {
            return;
        }
        let Ok(proxy) = super::proxy() else {
            return;
        };
        self.presence.wake_at = Some(at_ms);
        let delay = (at_ms - now).max(0.0).ceil();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a timer of a few seconds"
        )]
        let ms = delay as u32;
        wasm_bindgen_futures::spawn_local(async move {
            super::sleep_ms(ms).await;
            let _ = proxy.send_event(id);
        });
    }

    /// The board's members as the desktop's roster reads them.
    fn here_from(&mut self, view: &BoardView) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        let members: Vec<LiveMember> = view.members.iter().map(member_of).collect();
        let changed = {
            let mut here = desktop.here.borrow_mut();
            let members = here.set_members(Some(own_member(view)), Some(members));
            let note = here.set_unavailable(view.note.clone());
            members || note
        };
        if changed {
            desktop.announce_arrivals();
            // The bar reads the strips' counts in its update pass, which a
            // bare redraw skips: without the wake it would paint them only on
            // its next other change, such as the clock's minute.
            desktop.shared.wake();
            self.apply_here();
        }
    }

    /// The members' names, kinds and carets, as their presence says them:
    /// editors draw the caret, labelled, in the next frame. Its colour is the
    /// workspace's to settle, never the one the member publishes, so two
    /// people here at once are never drawn alike.
    fn names_from(&mut self, view: &BoardView) {
        let present: Vec<String> = view.members.iter().map(|m| m.publisher.clone()).collect();
        let gone: Vec<String> = self
            .presence
            .seen
            .iter()
            .filter(|publisher| !present.contains(publisher))
            .cloned()
            .collect();
        self.presence.seen = present;
        if view.members.is_empty() && gone.is_empty() {
            return;
        }
        self.frame.update_presence(|presence| {
            for publisher in &gone {
                presence.unregister_peer(publisher);
            }
            for member in &view.members {
                let user = UserId(member.publisher.clone());
                presence.set_participant_kind(&user, member.agent);
                if !member.label.is_empty() {
                    presence.set_display_name(user, member.label.clone());
                }
                match selection_anchors(member.selection.as_ref()) {
                    Some((key, start, end)) => {
                        use ui::collaboration::PresencePayload;
                        let payload = if start == end {
                            PresencePayload::Caret { pos: start }
                        } else {
                            PresencePayload::Selection { start, end }
                        };
                        presence.apply_remote_presence(member.publisher.clone(), key, payload);
                    }
                    None => presence.unregister_peer(&member.publisher),
                }
            }
            true
        });
    }
}

/// A member's published caret or selection, as the record it is in and its
/// anchors.
fn selection_anchors(selection: Option<&JsSelection>) -> Option<(PresenceKey, usize, usize)> {
    let selection = selection?;
    let anchor = tilo_relay_protocol::live::LiveAnchor {
        cursor: hex::decode(&selection.anchor).ok()?,
    };
    let head = tilo_relay_protocol::live::LiveAnchor {
        cursor: hex::decode(&selection.head).ok()?,
    };
    tilo_browser_peer::transport::read_presence_anchors(&anchor, &head)
}

impl Mount {
    /// Unix milliseconds, on the clock presence is heard on.
    fn now_ms() -> f64 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a millisecond clock far below 2^53"
        )]
        let now = ui::time::unix_now_ms() as f64;
        now
    }

    /// Show the pointer shape the surface asks for where the pointer is, as
    /// a native window does: the canvas's CSS cursor, set only on a change.
    pub(super) fn show_cursor(&mut self) {
        let cursor = self.frame.pointer_cursor();
        if cursor != self.cursor {
            self.cursor = cursor;
            self.window.set_cursor(cursor);
        }
    }
}

/// This person's pointer at `(x, y)` on the surface (CSS pixels), as the
/// workspace's live layer carries it: `{tile: {tab}, at}` over the tile of the
/// tab it is over, in that tile's logical pixels. `None` over no tab, which
/// publishes no pointer.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn pointer_tile(id: u64, x: f32, y: f32) -> Result<Option<String>, JsValue> {
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
        let point = (f64::from(x), f64::from(y));
        let Some((tab, at_x, at_y)) = tilo_live::presence::anchor(&m.tiles(), point) else {
            return Ok(None);
        };
        serde_json::to_string(&serde_json::json!({
            "tile": {"tab": tab.to_string()},
            "at": [at_x, at_y],
        }))
        .map(Some)
        .map_err(error)
    })
}

/// The members' pointers the frame draws: `[{label, agent, x, y, color}]`,
/// `color` as `#rrggbb`.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn remote_pointers_shown(id: u64) -> Result<String, JsValue> {
    with_mount_quietly(id, |m| {
        let shown: Vec<serde_json::Value> = m
            .frame
            .placed_pointers()
            .iter()
            .map(|p| {
                let [r, g, b, _] = p.color.to_rgba8().to_u8_array();
                serde_json::json!({
                    "label": p.label,
                    "agent": p.is_agent,
                    "x": p.pos.0,
                    "y": p.pos.1,
                    "color": format!("#{r:02x}{g:02x}{b:02x}"),
                })
            })
            .collect();
        serde_json::to_string(&shown).map_err(error)
    })
}

/// Name the people of the mount's workspace, `[{id, name}]`, `id` each
/// person's key as their actions record it (what `cx.me()` and
/// `ActionCx::author` answer). Live presence names devices; a page that knows
/// who belongs to the workspace (its door admitted them) names the people, so
/// an app shows `cx.people().name(author)` as the person's name rather than
/// their key.
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
    with_mount_quietly(id, |m| {
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
