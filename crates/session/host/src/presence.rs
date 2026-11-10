//! An agent's word in a workspace: the view its person's desktops draw it by.

use collab_types::presence::{MAX_ACTIVITY_BYTES, ShellPresence, ShellSelection, ShellWindow};
use tilo_relay_protocol::live::{LiveAnchor, LiveRecord};

/// The view `label` states, numbered `seq` and said at `sent_at_ms`: its face, what it is doing, and
/// the record it acts in with its caret or selection there. An agent has no screen, so the window it
/// names carries its face, in that window's rect.
#[must_use]
pub fn view(
    seq: u32,
    sent_at_ms: u64,
    label: &str,
    colour: [u8; 3],
    activity: Option<String>,
    focus: Option<&LiveRecord>,
    selection: Option<&(LiveAnchor, LiveAnchor)>,
) -> ShellPresence {
    ShellPresence {
        seq,
        sent_at_ms,
        label: label.to_owned(),
        agent: true,
        colour: format!("#{:02x}{:02x}{:02x}", colour[0], colour[1], colour[2]),
        activity,
        place: None,
        size: [0, 0],
        windows: focus.map(window).into_iter().collect(),
        selection: selection.map(|(anchor, head)| ShellSelection {
            anchor: hex::encode(&anchor.cursor),
            head: hex::encode(&head.cursor),
        }),
        chat: None,
        reaction: None,
    }
}

/// The window showing `focus`, by the record's short digest.
fn window(focus: &LiveRecord) -> ShellWindow {
    ShellWindow {
        app: focus.app.clone(),
        record: focus.short_digest(),
        rect: [0, 0, 10_000, 10_000],
        focused: true,
    }
}

/// What a member says it is doing, cut to what a presence frame carries, on a character edge.
#[must_use]
pub fn activity(activity: &str) -> String {
    let mut end = activity.len().min(MAX_ACTIVITY_BYTES);
    while !activity.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    activity.get(..end).unwrap_or_default().to_owned()
}
