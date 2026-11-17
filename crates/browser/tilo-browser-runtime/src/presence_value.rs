//! What this page says of itself in one workspace: its face and screen, the
//! window the person works in and their caret there, where its pointer is,
//! what it is saying beside it, its wave and who it follows. The page's live
//! layer sends it whole with each change, and the session's client keeps what
//! changed and says it under the session's schedule.
//!
//! A window is named by its app and the 16-hex short digest of its document,
//! the name every member's [`ShownFocus`] and the layout's windows carry, so a
//! receiver matches it against what it holds. Its tests run on the host too.

use collab_types::presence::{
    ShellChat, ShellPresence, ShellReaction, ShellSelection, ShellWindow, ShownFocus, ShownLayout,
    ShownWave,
};
use serde::{Deserialize, Serialize};

/// `{kind: 'person' | 'agent', label, color: '#rrggbb', profile_ms}`: `profile_ms` when the
/// person last changed their profile, as this page knows it (`0` for unknown).
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct JsMember {
    pub(crate) kind: String,
    pub(crate) label: String,
    pub(crate) color: String,
    #[serde(default)]
    pub(crate) profile_ms: u64,
}

/// Where this page's pointer is: over the tile of a tab of the workspace, in
/// that tile's logical pixels — the way every device's pointer travels. The
/// tab's id is a decimal string: it can pass 2^53.
#[derive(Clone, Serialize, Deserialize)]
struct JsPointer {
    tile: JsTile,
    at: [f32; 2],
}

#[derive(Clone, Serialize, Deserialize)]
struct JsTile {
    tab: String,
}

/// What this page says of itself in one workspace, as the live layer hands it
/// to the board. See the module docs.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JsPresence {
    /// This view's sequence, as the shell wire carries it: receivers order
    /// views by it, and one at or below the last they took is a repeat.
    #[serde(default)]
    seq: u32,
    #[serde(default)]
    sent_at_ms: u64,
    /// The window the person works in: its app and the short digest of its
    /// document, as every member's focus names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) focus: Option<ShownFocus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pointer: Option<JsPointer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selection: Option<ShellSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) member: Option<JsMember>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    place: Option<String>,
    /// Cursor chat, beside the pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chat: Option<ShellChat>,
    /// The latest reaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reaction: Option<ShellReaction>,
    /// The latest wave, `target` the waved-at member's publisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) wave: Option<ShownWave>,
    /// Where the windows are: `{size, windows: [{app, document, rect, focused}]}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layout: Option<ShownLayout>,
    /// Whose view this member follows: their publisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) following: Option<String>,
}

/// This page's shell as its value names it: who it is, its screen, the
/// window the person works in and their caret there. `None` while the value
/// names nobody, which says nothing of a shell.
pub(crate) fn shell_from_js(
    value: &JsPresence,
) -> Result<Option<Box<ShellPresence>>, &'static str> {
    let Some(member) = &value.member else {
        return Ok(None);
    };
    if !matches!(member.kind.as_str(), "person" | "agent") {
        return Err("a member is a person or an agent");
    }
    let colour = member.color.clone();
    let hex = colour
        .strip_prefix('#')
        .ok_or("a member's colour is #rrggbb")?;
    if hex.len() != 6 || hex::decode(hex).is_err() {
        return Err("a member's colour is #rrggbb");
    }
    let (size, mut windows) = match &value.layout {
        Some(layout) => (
            [u32::from(layout.size[0]), u32::from(layout.size[1])],
            layout
                .windows
                .iter()
                .map(|window| {
                    let document = <[u8; 8]>::try_from(
                        hex::decode(&window.document)
                            .map_err(|_| "a window's document is a 16-hex digest")?,
                    )
                    .map_err(|_| "a window's document is a 16-hex digest")?;
                    Ok(ShellWindow {
                        app: window.app.clone(),
                        document,
                        rect: window.rect,
                        focused: window.focused,
                    })
                })
                .collect::<Result<Vec<_>, &'static str>>()?,
        ),
        None => ([0, 0], Vec::new()),
    };
    // The window the person works in is the one their caret is in, whatever
    // the layout said: the value names it on its own.
    if let Some(focus) = &value.focus {
        let document = <[u8; 8]>::try_from(
            hex::decode(&focus.document)
                .map_err(|_| "a focused window's document is a 16-hex digest")?,
        )
        .map_err(|_| "a focused window's document is a 16-hex digest")?;
        let mut found = false;
        for window in &mut windows {
            window.focused = window.app == focus.app && window.document == document;
            found |= window.focused;
        }
        if !found {
            windows.push(ShellWindow {
                app: focus.app.clone(),
                document,
                rect: [0, 0, 0, 0],
                focused: true,
            });
        }
    }
    Ok(Some(Box::new(ShellPresence {
        seq: value.seq,
        sent_at_ms: value.sent_at_ms,
        label: member.label.clone(),
        agent: member.kind == "agent",
        colour,
        // What a person is doing travels as their status (`set_status`), never as their shell's.
        activity: None,
        place: value.place.clone(),
        size,
        windows,
        selection: value.selection.clone(),
        chat: value.chat.clone(),
        reaction: value.reaction.clone(),
    })))
}

/// Where this page's pointer is, on the tile it names. `None` while it names none.
pub(crate) fn pointer_of(
    value: &JsPresence,
) -> Result<Option<tilo_live::presence::Point>, &'static str> {
    value
        .pointer
        .as_ref()
        .map(|pointer| {
            Ok(tilo_live::presence::Point {
                tab: pointer
                    .tile
                    .tab
                    .parse()
                    .map_err(|_| "a pointer's tab is a decimal u64")?,
                x: pointer.at[0],
                y: pointer.at[1],
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page's own word is the value its live layer hands back, so it is
    /// the value the board reads: the window the person works in is named
    /// `{app, document}`, as every member's focus reads it.
    #[test]
    fn the_window_the_person_works_in_is_the_one_the_page_names() {
        let value: JsPresence = serde_json::from_str(
            r##"{"member": {"kind": "person", "label": "Ada", "color": "#ff0000"},
                "focus": {"app": "so.tilo.notes", "document": "0011223344556677"}}"##,
        )
        .expect("the page's own word parses");
        let shell = shell_from_js(&value)
            .expect("the page's own word is a shell")
            .expect("it names who it is");
        assert_eq!(shell.label, "Ada");
        assert_eq!(shell.windows.len(), 1);
        assert_eq!(shell.windows[0].app, "so.tilo.notes");
        assert_eq!(hex::encode(shell.windows[0].document), "0011223344556677");
        assert!(shell.windows[0].focused, "the window the person works in");
    }

    /// A layout already names every window and the focused one; the focus is
    /// the caret's word on it, and both name the same window.
    #[test]
    fn the_focus_marks_the_layout_window_it_names() {
        let value: JsPresence = serde_json::from_str(
            r##"{"member": {"kind": "person", "label": "Ada", "color": "#ff0000"},
                "layout": {"size": [1000, 700], "windows": [
                    {"app": "so.tilo.chat", "document": "8899aabbccddeeff", "rect": [0, 0, 5000, 10000], "focused": false},
                    {"app": "so.tilo.notes", "document": "0011223344556677", "rect": [5000, 0, 10000, 10000], "focused": false}]},
                "focus": {"app": "so.tilo.notes", "document": "0011223344556677"}}"##,
        )
        .expect("the page's own word parses");
        let shell = shell_from_js(&value)
            .expect("the page's own word is a shell")
            .expect("it names who it is");
        let focused: Vec<&str> = shell
            .windows
            .iter()
            .filter(|window| window.focused)
            .map(|window| window.app.as_str())
            .collect();
        assert_eq!(focused, ["so.tilo.notes"]);
    }

    /// Where the page's pointer is, as the board says it.
    #[test]
    fn the_pointer_is_on_the_tile_it_names() {
        let value: JsPresence =
            serde_json::from_str(r#"{"pointer": {"tile": {"tab": "42"}, "at": [12.5, 30.0]}}"#)
                .expect("the page's own word parses");
        let point = pointer_of(&value)
            .expect("a pointer's tab is a decimal u64")
            .expect("the value names a pointer");
        assert_eq!(point.tab, 42, "the tile's own tab");
        // Both numbers are exact in an f32, so their bits say where it is.
        assert_eq!(
            (point.x.to_bits(), point.y.to_bits()),
            (12.5_f32.to_bits(), 30.0_f32.to_bits())
        );
    }
}
