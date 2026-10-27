//! Room members' pointers over a mount: where this person's pointer is,
//! said relative to what it is over, and where everyone else's lands in
//! this page's own layout. Both go through
//! [`tilo_relay_protocol::live::PointerLayout`], the mapping native shells
//! use too, so a pointer over a window or a strip's empty desktop lands on
//! the same spot of it whatever size each member's window is or wherever
//! they scrolled.

use tilo_relay_protocol::live::{LivePointer, LiveRecord, PointerLayout, ShownStrip};
use ui::permissions::UserId;
use wasm_bindgen::prelude::*;

use super::{Mount, error, with_mount_quietly};

/// A pointer as the page's live layer carries it: `{window: {app, record},
/// at: [x, y]}` over a window, `{beside: {app, record}, edge: [fx, fy], at:
/// [dx, dy]}` over the empty desktop beside the nearest window, or `{strip:
/// "<id>", at: [x, y]}` on a desk showing none (the id a decimal string: it
/// can pass 2^53).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub(crate) enum JsPointer {
    Window {
        window: JsWindow,
        at: [f32; 2],
    },
    Beside {
        beside: JsWindow,
        edge: [f32; 2],
        at: [f32; 2],
    },
    Strip {
        strip: String,
        at: [f32; 2],
    },
}

/// The record a window shows, as the live layer names it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct JsWindow {
    app: String,
    record: String,
}

impl JsPointer {
    pub(crate) fn into_live(self) -> Result<LivePointer, String> {
        Ok(match self {
            Self::Window { window, at } => LivePointer::Window {
                record: LiveRecord {
                    app: window.app,
                    record: window.record,
                },
                at,
            },
            Self::Beside { beside, edge, at } => LivePointer::Beside {
                record: LiveRecord {
                    app: beside.app,
                    record: beside.record,
                },
                edge,
                at,
            },
            Self::Strip { strip, at } => LivePointer::Strip {
                strip: strip.parse().map_err(|_| "a strip id is a decimal u64")?,
                at,
            },
        })
    }

    pub(crate) fn from_live(pointer: LivePointer) -> Self {
        match pointer {
            LivePointer::Window { record, at } => Self::Window {
                window: JsWindow {
                    app: record.app,
                    record: record.record,
                },
                at,
            },
            LivePointer::Beside { record, edge, at } => Self::Beside {
                beside: JsWindow {
                    app: record.app,
                    record: record.record,
                },
                edge,
                at,
            },
            LivePointer::Strip { strip, at } => Self::Strip {
                strip: strip.to_string(),
                at,
            },
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "surface coordinates are a few thousand logical pixels"
)]
fn rect(rect: ui::kurbo::Rect) -> [f32; 4] {
    [
        rect.x0 as f32,
        rect.y0 as f32,
        rect.x1 as f32,
        rect.y1 as f32,
    ]
}

/// What this mount shows that a pointer can be over: the desktop's shown
/// windows by record and its shown strip, or a single application's
/// record over the whole surface.
fn layout(m: &Mount) -> PointerLayout {
    if let Some(desktop) = &m.desktop {
        let shown = desktop.handle.shown_tabs();
        let windows = desktop.handle.read(|tree| {
            shown
                .into_iter()
                .filter_map(|(tab, area)| {
                    let (app, record) = desktop.shared.live_record_of(tree, tab)?;
                    Some((LiveRecord { app, record }, rect(area)))
                })
                .collect()
        });
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a strip's pan is a few thousand logical pixels"
        )]
        let strip = desktop
            .handle
            .shown_strip()
            .map(|(id, area, pan)| ShownStrip {
                id: id.0,
                area: rect(area),
                scroll_x: pan as f32,
            });
        return PointerLayout { windows, strip };
    }
    let (width, height) = m.viewport.applied().logical_size();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a surface is a few thousand logical pixels"
    )]
    let surface = [0.0, 0.0, width as f32, height as f32];
    let windows = m
        .app
        .map(str::to_owned)
        .zip(m.checkout.as_ref())
        .map(|(app, checkout)| {
            let record = if checkout.id.is_package_primary() {
                String::new()
            } else {
                checkout.id.as_str().to_owned()
            };
            (LiveRecord { app, record }, surface)
        })
        .into_iter()
        .collect();
    PointerLayout {
        windows,
        strip: None,
    }
}

/// This person's pointer at `(x, y)` on the surface (CSS pixels), as the
/// room's live layer carries it ([`JsPointer`] JSON): relative to the
/// window it is over, else to the shown strip. `None` over nothing either
/// names, which publishes no pointer.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn pointer_anchor(id: u64, x: f32, y: f32) -> Result<Option<String>, JsValue> {
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
        layout(m)
            .anchor([x, y])
            .map(|pointer| serde_json::to_string(&JsPointer::from_live(pointer)).map_err(error))
            .transpose()
    })
}

#[derive(Clone, serde::Deserialize)]
struct Chat {
    text: String,
    #[serde(default)]
    typing: bool,
    opacity: f32,
}

#[derive(Clone, serde::Deserialize)]
struct Reaction {
    emoji: String,
    rise: f64,
    opacity: f32,
}

/// One member's pointer as the page hands it over.
#[derive(Clone, serde::Deserialize)]
pub(super) struct RemotePointer {
    publisher: String,
    pointer: JsPointer,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    agent: bool,
    #[serde(default)]
    activity: Option<String>,
    #[serde(default)]
    chat: Option<Chat>,
    #[serde(default)]
    reaction: Option<Reaction>,
}

impl Mount {
    /// Place the members' pointers in the layout this mount shows now,
    /// leaving out any over something it does not show; asks for a frame
    /// when what is drawn changed. Runs again after every frame, so a
    /// pointer follows its window or strip as it moves here.
    pub(super) fn place_remote_pointers(&mut self) {
        if self.remote_pointers.is_empty() && self.frame.placed_pointers().is_empty() {
            return;
        }
        let layout = layout(self);
        let presence = self.frame.presence();
        let cursors = self
            .remote_pointers
            .iter()
            .filter_map(|p| {
                let pointer = p.pointer.clone().into_live().ok()?;
                let [x, y] = layout.place(&pointer)?;
                Some(presence.placed_cursor(ui::presence::PlacedPointer {
                    user: UserId(p.publisher.clone()),
                    at: (f64::from(x), f64::from(y)),
                    name: p.name.clone(),
                    agent: p.agent,
                    activity: p.activity.clone(),
                    chat: p.chat.clone().map(|c| ui::presence::ChatBubble {
                        text: c.text,
                        typing: c.typing,
                        opacity: c.opacity.clamp(0.0, 1.0),
                    }),
                    reaction: p.reaction.clone().map(|r| ui::presence::ReactionFloat {
                        emoji: r.emoji,
                        rise: r.rise,
                        opacity: r.opacity.clamp(0.0, 1.0),
                    }),
                }))
            })
            .collect();
        if self.frame.set_placed_pointers(cursors) {
            self.pending = true;
            self.window.request_redraw();
        }
    }
}

/// The room's members' pointers over this mount: `[{publisher, pointer,
/// name?, agent?, activity?, chat?: {text, typing, opacity}, reaction?:
/// {emoji, rise, opacity}}]`, each `pointer` relative to what it is over
/// ([`JsPointer`]), `name` and `agent` as the member's own presence gives
/// them. Each replaces the last set: whoever is left out is no longer
/// shown. The frame draws each where it lands in this page's layout (none
/// over a window or strip this page does not show), in the colour and
/// under the name presence knows the member by.
///
/// # Errors
///
/// Rejects malformed JSON or a closed mount.
#[wasm_bindgen]
pub fn remote_pointers(id: u64, pointers: &str) -> Result<(), JsValue> {
    let pointers: Vec<RemotePointer> = serde_json::from_str(pointers).map_err(error)?;
    // Pointers change what is drawn, never what the semantics tree shows.
    with_mount_quietly(id, |m| {
        m.remote_pointers = pointers;
        m.place_remote_pointers();
        Ok(())
    })
}

/// The members' pointers the frame draws, as [`remote_pointers`] last
/// placed them: `[{label, agent, x, y, color}]`, `color` as `#rrggbb`.
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

impl Mount {
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
