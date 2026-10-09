//! LIVE_V1, desktop side: what the local desktop publishes (focused
//! record and its window rectangle, the focused editor's caret/selection as
//! compact CRDT-keyed anchors, window drags) and the projection of remote
//! carets/selections onto the desktop's collaborator presence, which Writer,
//! Notes and the other presence-aware editors already draw natively.
//!
//! Remote pointers and window-drag ghosts are drawn by the browser overlay
//! (`web/live-overlay.mjs`) on a separate compositor layer, so they never
//! invalidate the document's layout or re-render the WebGPU scene.
//!
//! Anchors: the wire carries two opaque byte strings of at most 64 bytes.
//! `anchor = [1, uvarint(start), postcard(PresenceKey)]` names the editor's
//! stable presence scope (Writer: the CRDT block id; block editors: the
//! block) plus a byte offset; `head = [2, uvarint(end)]` reuses the anchor's
//! scope. A caret has `start == end`. A scope that does not fit is not
//! published (the pointer and focus still are).
use std::collections::BTreeMap;

use serde::Deserialize;
use ui::collaboration::PresencePayload;
use ui::permissions::UserId;
use ui::presence::{PresenceCtx, PresenceKey, SelectionRange};
use vello::kurbo::{Point, Rect};

/// Frozen wire bound for one anchor (`tilo_relay_protocol::live::MAX_OBJECT_ID_BYTES`).
pub(crate) const ANCHOR_MAX_BYTES: usize = 64;
const TAG_SCOPED: u8 = 1;
const TAG_SAME_SCOPE: u8 = 2;
/// `[3, uvarint(start), scope_digest(key)[16]]`: used only when the scoped
/// form does not fit the 64-byte bound (e.g. 130-byte Writer block ids). The
/// receiver files it under `PresenceKey::from_scope_digest`, which editors
/// resolve against their own keys (`PresenceKey::digest_form`).
const TAG_DIGEST: u8 = 3;
/// Offsets past this are not text positions any editor lays out.
const MAX_OFFSET: u64 = 1 << 32;
/// Live record id of a record-less (package-primary) document, as the host
/// forwards it (`live_service::PACKAGE_PRIMARY_RECORD`).
pub(crate) const PACKAGE_PRIMARY_RECORD: &str = "@tilo:package-primary";

fn push_uvarint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
fn read_uvarint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0u64;
    for (index, byte) in bytes.iter().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return (value <= MAX_OFFSET).then(|| (value, &bytes[index + 1..]));
        }
    }
    None
}

/// `(anchor, head)` for a caret or selection payload, or `None` when the
/// payload is not a text position or its scope exceeds the wire bound.
pub(crate) fn encode_selection(
    key: &PresenceKey,
    payload: &PresencePayload,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let (start, end) = match payload {
        PresencePayload::Caret { pos } => (*pos, *pos),
        PresencePayload::Selection { start, end } => (*start, *end),
        _ => return None,
    };
    if matches!(key, PresenceKey::View { .. } | PresenceKey::Pointer { .. }) {
        return None;
    }
    let scope = postcard::to_allocvec(key).ok()?;
    let mut anchor = vec![TAG_SCOPED];
    push_uvarint(&mut anchor, u64::try_from(start).ok()?);
    anchor.extend_from_slice(&scope);
    let mut head = vec![TAG_SAME_SCOPE];
    push_uvarint(&mut head, u64::try_from(end).ok()?);
    if anchor.len() > ANCHOR_MAX_BYTES {
        anchor = vec![TAG_DIGEST];
        push_uvarint(&mut anchor, u64::try_from(start).ok()?);
        anchor.extend_from_slice(&key.scope_digest()?);
    }
    (anchor.len() <= ANCHOR_MAX_BYTES).then_some((anchor, head))
}

/// Inverse of [`encode_selection`]: `(scope, start, end)` with the raw
/// published order preserved (callers order it for drawing).
pub(crate) fn decode_selection(anchor: &[u8], head: &[u8]) -> Option<(PresenceKey, usize, usize)> {
    if anchor.len() > ANCHOR_MAX_BYTES || head.len() > ANCHOR_MAX_BYTES {
        return None;
    }
    let (&tag, rest) = anchor.split_first()?;
    let (start, scope) = read_uvarint(rest)?;
    let key: PresenceKey = match tag {
        TAG_SCOPED => {
            let key: PresenceKey = postcard::from_bytes(scope).ok()?;
            if postcard::to_allocvec(&key).ok()? != scope {
                return None;
            }
            key
        }
        TAG_DIGEST => PresenceKey::from_scope_digest(scope.try_into().ok()?),
        _ => return None,
    };
    let end = match head.split_first()? {
        (&TAG_SAME_SCOPE, rest) => {
            let (end, tail) = read_uvarint(rest)?;
            if !tail.is_empty() {
                return None;
            }
            end
        }
        (&TAG_SCOPED | &TAG_DIGEST, _) => {
            let (other, end, _) = decode_selection(head, &[TAG_SAME_SCOPE, 0])?;
            if other != key {
                return None;
            }
            u64::try_from(end).ok()?
        }
        _ => return None,
    };
    Some((
        key,
        usize::try_from(start).ok()?,
        usize::try_from(end).ok()?,
    ))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}
pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    hex::decode(text).ok()
}

/// Live record for a durable `(app, record)`, as the host forwards it.
pub(crate) fn live_record(app: &str, record: &str) -> Option<(String, String)> {
    let record = if record.is_empty() {
        PACKAGE_PRIMARY_RECORD
    } else {
        record
    };
    (!app.is_empty() && app.len() <= 128 && record.len() <= 128)
        .then(|| (app.to_owned(), record.to_owned()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveDragKind {
    /// Whole-group reorder drag (the shared window-order change).
    Move,
    /// Width change through a resize handle or the group edge (per device).
    Resize,
}
impl LiveDragKind {
    const fn tag(self) -> u8 {
        match self {
            Self::Move => 1,
            Self::Resize => 2,
        }
    }
    const fn name(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Resize => "resize",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LiveGroup {
    pub id: u64,
    pub rect: Rect,
    pub tab: u64,
    /// Primary vertical scroll offset of the active tab's content (CSS px
    /// from the top), when the app exposes one: pointers over it are
    /// published in content space so they align with the content on
    /// receivers scrolled differently.
    pub scroll: Option<f64>,
    /// The active tab shows a legacy window: its app id and content bounds,
    /// which the overlay fills with the session's video of that window.
    pub legacy: Option<(String, Rect)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LiveDragState {
    pub kind: LiveDragKind,
    pub group: u64,
    /// Pointer displacement since the press (moves); zero for resizes.
    pub offset: (f64, f64),
}

/// Window geometry of the mounted desktop, captured after every draw and
/// input dispatch (a handful of rectangles; no document reads).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LiveLayout {
    pub groups: Vec<LiveGroup>,
    pub focused_group: Option<u64>,
    pub focused_tab: Option<u64>,
    pub drag: Option<LiveDragState>,
    /// A modal desktop activity covers the windows: no focus is published.
    pub covered: bool,
    /// Carets and selections of the focused window's editors
    /// ([`ui::presence::capability::subtree_presence`]).
    pub window_presence: Vec<(PresenceKey, PresencePayload)>,
}

/// The selection a mount publishes: the keyboard-focused widget's when it
/// is one of the focused window's (or mount's) editors, else that window's
/// first editor caret. Keyboard focus can stay behind in another window, or
/// an action can move a caret without focus.
pub(crate) fn published_selection(
    focused: Option<(PresenceKey, PresencePayload)>,
    window: &[(PresenceKey, PresencePayload)],
) -> Option<(PresenceKey, PresencePayload)> {
    match focused {
        Some(focused) if window.is_empty() || window.iter().any(|(key, _)| *key == focused.0) => {
            Some(focused)
        }
        _ => window.first().cloned(),
    }
}
impl LiveLayout {
    pub(crate) fn drag_from_row(
        row_drag: Option<(u64, Point, Point)>,
        resize: Option<u64>,
    ) -> Option<LiveDragState> {
        if let Some((group, start, current)) = row_drag {
            return Some(LiveDragState {
                kind: LiveDragKind::Move,
                group,
                offset: (current.x - start.x, current.y - start.y),
            });
        }
        resize.map(|group| LiveDragState {
            kind: LiveDragKind::Resize,
            group,
            offset: (0.0, 0.0),
        })
    }
}

fn rect_json(rect: Rect) -> serde_json::Value {
    let round = |v: f64| (v * 100.0).round() / 100.0;
    serde_json::json!([
        round(rect.x0),
        round(rect.y0),
        round(rect.width()),
        round(rect.height())
    ])
}
fn scroll_json(scroll: Option<f64>) -> serde_json::Value {
    scroll
        .filter(|offset| offset.is_finite() && *offset >= 0.0)
        .map_or(serde_json::Value::Null, |offset| {
            ((offset * 100.0).round() / 100.0).into()
        })
}
fn group_id(id: u64) -> String {
    hex(&id.to_be_bytes())
}
fn record_json(record: Option<&(String, String)>) -> serde_json::Value {
    record
        .and_then(|(app, record)| live_record(app, record))
        .map_or(
            serde_json::Value::Null,
            |(app, record)| serde_json::json!({"app": app, "record": record}),
        )
}

/// Local presence snapshot for the overlay: focus record and its window
/// rectangle (CSS pixels relative to the canvas), the focused editor's
/// anchors, the active window drag and every laid-out window group.
pub(crate) fn snapshot_json(
    layout: &LiveLayout,
    tab_records: &BTreeMap<u64, (String, String)>,
    selection: Option<(PresenceKey, PresencePayload)>,
) -> serde_json::Value {
    let focused = (!layout.covered)
        .then_some(layout.focused_group)
        .flatten()
        .and_then(|id| layout.groups.iter().find(|group| group.id == id));
    let focus_tab = layout.focused_tab.or(focused.map(|group| group.tab));
    let focus = if layout.covered {
        serde_json::Value::Null
    } else {
        record_json(focus_tab.and_then(|tab| tab_records.get(&tab)))
    };
    let selection = (!focus.is_null())
        .then_some(selection)
        .flatten()
        .and_then(|(key, payload)| encode_selection(&key, &payload))
        .map_or(
            serde_json::Value::Null,
            |(anchor, head)| serde_json::json!({"anchor": hex(&anchor), "head": hex(&head)}),
        );
    let drag = layout
        .drag
        .as_ref()
        .and_then(|drag| {
            let group = layout.groups.iter().find(|group| group.id == drag.group)?;
            let mut object = vec![drag.kind.tag()];
            object.extend_from_slice(&drag.group.to_be_bytes());
            Some(serde_json::json!({
                "kind": drag.kind.name(),
                "object": hex(&object),
                "group": group_id(drag.group),
                "offset": [drag.offset.0, drag.offset.1],
                "rect": rect_json(group.rect),
            }))
        })
        .unwrap_or(serde_json::Value::Null);
    let groups = layout
        .groups
        .iter()
        .map(|group| {
            serde_json::json!({
                "id": group_id(group.id),
                "rect": rect_json(group.rect),
                "scroll": scroll_json(group.scroll),
                "record": record_json(tab_records.get(&group.tab)),
                "legacy": group.legacy.as_ref().map_or(serde_json::Value::Null, |(app, content)| {
                    serde_json::json!({"app": app, "tab": group_id(group.tab), "content": rect_json(*content)})
                }),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "covered": layout.covered,
        "focus": focus,
        "focusRect": focused.map_or(serde_json::Value::Null, |group| rect_json(group.rect)),
        "focusScroll": focused.map_or(serde_json::Value::Null, |group| scroll_json(group.scroll)),
        "focusGroup": focused.map_or(serde_json::Value::Null, |group| group_id(group.id).into()),
        "selection": selection,
        "drag": drag,
        "groups": groups,
    })
}

/// Labels hosts put on certificates they issue for the owner's own devices.
/// They name a machine, never the
/// person, so they are not shown as the owner's name.
const MACHINE_LABELS: &[&str] = &[
    "Browser viewer",
    "Native workspace peer",
    "Local native relay",
    "Local Tilo session authority",
    "browser",
    "owner",
];

/// The owner's display name from its governed device label, unless that
/// label is a host-issued machine label.
pub(crate) fn owner_display_name(label: Option<&str>) -> Option<String> {
    let label = label?.trim();
    (!label.is_empty() && label.len() <= 128 && !MACHINE_LABELS.contains(&label))
        .then(|| label.to_owned())
}

/// One live publisher identity: a member device, its person and name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveMember {
    pub device: [u8; 32],
    pub user: [u8; 32],
    pub name: Option<String>,
    pub owner: bool,
}

/// `[{device, user, name, owner}]` for the overlay. The governed collaborator
/// projection does not include the structural owner (it has no admission
/// receipt), so the owner's certified devices are added after it: without
/// them the owner's cursor would be labelled "Guest xxxx" for every member.
pub(crate) fn live_member_rows(
    collaborators: Vec<LiveMember>,
    owner_devices: Vec<LiveMember>,
) -> Vec<serde_json::Value> {
    let mut seen = BTreeMap::new();
    for member in collaborators.into_iter().chain(owner_devices) {
        seen.entry(member.device).or_insert(member);
    }
    seen.into_values()
        .map(|member| {
            serde_json::json!({
                "device": hex(&member.device),
                "user": hex(&member.user),
                "name": member.name,
                "owner": member.owner,
            })
        })
        .collect()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteSelectionInput {
    pub anchor: String,
    pub head: String,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteInput {
    pub publisher: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub selection: Option<RemoteSelectionInput>,
    /// Text the member typed that has not reached this document yet
    /// (`OpsFrame::Preview`), shown beside its caret until the change lands.
    #[serde(default)]
    pub typing: Option<String>,
}

/// Characters of in-flight typing shown in a caret label.
const TYPING_PREVIEW_CHARS: usize = 40;

/// Caret label: the member's name, plus the tail of its in-flight typing.
fn caret_label(name: Option<&str>, typing: Option<&str>) -> Option<String> {
    let typing = typing
        .map(|text| text.replace(['\n', '\r', '\t'], " "))
        .filter(|text| !text.trim().is_empty());
    match (name, typing) {
        (name, Some(text)) => {
            let count = text.chars().count();
            let tail: String = text
                .chars()
                .skip(count.saturating_sub(TYPING_PREVIEW_CHARS))
                .collect();
            let ellipsis = if count > TYPING_PREVIEW_CHARS {
                "\u{2026}"
            } else {
                ""
            };
            Some(format!(
                "{} \u{201c}{ellipsis}{tail}\u{201d}",
                name.unwrap_or("Guest")
            ))
        }
        (Some(name), None) => Some(name.to_owned()),
        (None, None) => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteRow {
    name: Option<String>,
    color: Option<String>,
    target: Option<(PresenceKey, usize, usize)>,
}

/// Remote carets/selections currently projected onto the desktop presence.
/// Only this projection's users are touched; native presence is untouched.
#[derive(Debug, Default)]
pub(crate) struct RemoteProjection {
    rows: BTreeMap<String, RemoteRow>,
}
impl RemoteProjection {
    /// Replace the projected remote text positions. Returns whether the
    /// presence changed (so the caller schedules exactly one redraw).
    pub(crate) fn apply(&mut self, presence: &mut PresenceCtx, inputs: Vec<RemoteInput>) -> bool {
        let mut next = BTreeMap::new();
        for input in inputs {
            if input.publisher.is_empty() || input.publisher.len() > 128 {
                continue;
            }
            let target = input.selection.as_ref().and_then(|selection| {
                decode_selection(&unhex(&selection.anchor)?, &unhex(&selection.head)?)
            });
            let name = input.name.filter(|name| !name.is_empty());
            next.insert(
                input.publisher,
                RemoteRow {
                    name: caret_label(name.as_deref(), input.typing.as_deref()),
                    color: input.color,
                    target,
                },
            );
        }
        if next == self.rows {
            return false;
        }
        for user in self.rows.keys() {
            let user = UserId(user.clone());
            for list in presence.carets.values_mut() {
                list.retain(|(u, _)| u != &user);
            }
            for list in presence.selections.values_mut() {
                list.retain(|(u, _)| u != &user);
            }
        }
        presence.carets.retain(|_, list| !list.is_empty());
        presence.selections.retain(|_, list| !list.is_empty());
        for (publisher, row) in &next {
            let user = UserId(publisher.clone());
            if let Some(color) = &row.color {
                presence.set_color_override(user.clone(), color);
            }
            if let Some(name) = &row.name {
                presence.set_display_name(user.clone(), name.clone());
            }
            if let Some((key, start, end)) = &row.target {
                let (low, high) = if start <= end {
                    (*start, *end)
                } else {
                    (*end, *start)
                };
                if low < high {
                    presence.set_selection(
                        key,
                        user.clone(),
                        SelectionRange {
                            start: low,
                            end: high,
                        },
                    );
                }
                presence.set_caret(key, user, *end);
            }
        }
        self.rows = next;
        true
    }

    #[cfg(test)]
    pub(crate) fn users(&self) -> Vec<&str> {
        self.rows.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::presence::PathSegment;

    fn writer_block(id: &str) -> PresenceKey {
        PresenceKey::Widget {
            path: vec![
                PathSegment::Field("wp_block".into()),
                PathSegment::Field(id.into()),
            ],
        }
    }

    #[test]
    fn caret_and_selection_round_trip_through_bounded_anchors() {
        let key = writer_block("2@5f3a9c01d2e4b6a8");
        let (anchor, head) =
            encode_selection(&key, &PresencePayload::Selection { start: 3, end: 300 }).unwrap();
        assert!(anchor.len() <= ANCHOR_MAX_BYTES && head.len() <= ANCHOR_MAX_BYTES);
        assert_eq!(
            decode_selection(&anchor, &head),
            Some((key.clone(), 3, 300))
        );
        let (anchor, head) = encode_selection(&key, &PresencePayload::Caret { pos: 17 }).unwrap();
        assert_eq!(decode_selection(&anchor, &head), Some((key, 17, 17)));
    }

    #[test]
    fn the_focused_window_caret_wins_over_keyboard_focus_left_elsewhere() {
        let writer = writer_block("2@5f3a9c01d2e4b6a8");
        let elsewhere = writer_block("9@0000000000000001");
        let window = vec![(writer.clone(), PresencePayload::Caret { pos: 40 })];
        let stray = Some((elsewhere, PresencePayload::Caret { pos: 0 }));
        assert_eq!(published_selection(stray, &window), window.first().cloned());
        let own = Some((writer, PresencePayload::Caret { pos: 41 }));
        assert_eq!(published_selection(own.clone(), &window), own);
        assert_eq!(published_selection(own.clone(), &[]), own);
        assert_eq!(published_selection(None, &[]), None);
    }

    #[test]
    fn single_record_snapshot_names_its_record_and_caret() {
        let key = writer_block("2@5f3a9c01d2e4b6a8");
        let layout = LiveLayout {
            focused_tab: Some(1),
            ..LiveLayout::default()
        };
        let records = BTreeMap::from([(1, ("so.tilo.writer".to_string(), String::new()))]);
        let snapshot = snapshot_json(
            &layout,
            &records,
            Some((key.clone(), PresencePayload::Caret { pos: 4 })),
        );
        assert_eq!(
            snapshot["focus"],
            serde_json::json!({"app": "so.tilo.writer", "record": PACKAGE_PRIMARY_RECORD})
        );
        assert!(snapshot["focusRect"].is_null());
        let anchor = unhex(snapshot["selection"]["anchor"].as_str().unwrap()).unwrap();
        let head = unhex(snapshot["selection"]["head"].as_str().unwrap()).unwrap();
        assert_eq!(decode_selection(&anchor, &head), Some((key, 4, 4)));
        let invalid_records = BTreeMap::from([(1, (String::new(), "r".to_string()))]);
        assert!(snapshot_json(&layout, &invalid_records, None)["focus"].is_null());
    }

    /// Notes names blocks by their stable UUID; both its caret key and its
    /// cross-block selection key fit the live anchor bound.
    #[test]
    fn notes_block_id_scopes_fit_the_live_anchor() {
        use ui::widget::block_editor::{block_presence_key, document_selection_presence_key};
        let id = "0b7a4f5e-2f0a-4c7e-9d0c-6f1f3f0e8a11";
        let caret = block_presence_key(id);
        let (anchor, head) =
            encode_selection(&caret, &PresencePayload::Caret { pos: 70_000 }).unwrap();
        assert_eq!(
            decode_selection(&anchor, &head),
            Some((caret, 70_000, 70_000))
        );
        let range = document_selection_presence_key(id, 300);
        let (anchor, head) = encode_selection(
            &range,
            &PresencePayload::Selection {
                start: 16_000,
                end: 2,
            },
        )
        .unwrap();
        assert!(anchor.len() <= ANCHOR_MAX_BYTES);
        assert_eq!(decode_selection(&anchor, &head), Some((range, 16_000, 2)));
    }

    #[test]
    fn long_writer_block_ids_travel_as_a_scope_digest() {
        // Worker-minted block ids: 128-hex actor + counter (130 bytes).
        let long = writer_block(&format!("{}.7", "ab".repeat(64)));
        let (anchor, head) =
            encode_selection(&long, &PresencePayload::Selection { start: 3, end: 300 }).unwrap();
        assert_eq!(anchor[0], TAG_DIGEST);
        assert!(anchor.len() <= ANCHOR_MAX_BYTES);
        let (key, start, end) = decode_selection(&anchor, &head).unwrap();
        assert_eq!((start, end), (3, 300));
        assert_eq!(Some(key), long.digest_form());
        // Short ids keep the exact scoped form.
        let (anchor, _) =
            encode_selection(&writer_block("b1"), &PresencePayload::Caret { pos: 1 }).unwrap();
        assert_eq!(anchor[0], TAG_SCOPED);
        // Shared vector with ui-core / the host: scope_digest(Widget{[wp_block, b1]}).
        assert_eq!(
            hex(&writer_block("b1").scope_digest().unwrap()),
            "9c59c4ded04a69ccf5b2ac58797c3b77"
        );
        // A truncated digest is refused.
        assert!(decode_selection(&[TAG_DIGEST, 1, 0, 0], &[TAG_SAME_SCOPE, 1]).is_none());
    }

    #[test]
    fn oversized_or_non_text_scopes_are_not_published() {
        let key = writer_block("b");
        assert!(encode_selection(&key, &PresencePayload::Arrived).is_none());
        let view = PresenceKey::View { data: vec![1, 2] };
        assert!(encode_selection(&view, &PresencePayload::Caret { pos: 1 }).is_none());
    }

    #[test]
    fn malformed_anchors_are_rejected() {
        let key = writer_block("b1");
        let (anchor, head) = encode_selection(&key, &PresencePayload::Caret { pos: 4 }).unwrap();
        assert!(decode_selection(&anchor[..anchor.len() - 1], &head).is_none());
        assert!(decode_selection(&head, &anchor).is_none());
        let mut trailing = head.clone();
        trailing.push(0);
        assert!(decode_selection(&anchor, &trailing).is_none());
        assert!(
            decode_selection(&[TAG_SCOPED, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f], &head).is_none()
        );
        // A head carrying its own, different scope is refused.
        let (other, _) =
            encode_selection(&writer_block("b2"), &PresencePayload::Caret { pos: 9 }).unwrap();
        assert!(decode_selection(&anchor, &other).is_none());
        let (same, _) = encode_selection(&key, &PresencePayload::Caret { pos: 9 }).unwrap();
        assert_eq!(decode_selection(&anchor, &same), Some((key, 4, 9)));
    }

    #[test]
    fn hex_round_trips_and_rejects_odd_input() {
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
        assert_eq!(unhex("00ab10"), Some(vec![0, 0xab, 0x10]));
        assert!(unhex("abc").is_none());
        assert!(unhex("zz").is_none());
    }

    fn layout() -> (LiveLayout, BTreeMap<u64, (String, String)>) {
        let layout = LiveLayout {
            groups: vec![
                LiveGroup {
                    id: 3,
                    rect: Rect::new(10.0, 40.0, 610.0, 700.0),
                    tab: 30,
                    scroll: Some(120.5),
                    legacy: None,
                },
                LiveGroup {
                    id: 4,
                    rect: Rect::new(620.0, 40.0, 1000.0, 700.0),
                    tab: 40,
                    scroll: None,
                    legacy: None,
                },
            ],
            focused_group: Some(4),
            focused_tab: None,
            drag: Some(LiveDragState {
                kind: LiveDragKind::Move,
                group: 3,
                offset: (25.0, -4.0),
            }),
            covered: false,
            window_presence: Vec::new(),
        };
        let records = BTreeMap::from([
            (
                30,
                ("so.tilo.word-processor".to_string(), "uuid:abc".to_string()),
            ),
            (40, ("so.tilo.chat".to_string(), String::new())),
        ]);
        (layout, records)
    }

    #[test]
    fn snapshot_maps_focus_to_live_record_window_rect_and_drag_object() {
        let (layout, records) = layout();
        let caret = Some((writer_block("b"), PresencePayload::Caret { pos: 2 }));
        let value = snapshot_json(&layout, &records, caret);
        assert_eq!(
            value["focus"],
            serde_json::json!({"app": "so.tilo.chat", "record": PACKAGE_PRIMARY_RECORD})
        );
        assert_eq!(
            value["focusRect"],
            serde_json::json!([620.0, 40.0, 380.0, 660.0])
        );
        assert_eq!(value["focusGroup"], "0000000000000004");
        assert!(
            value["selection"]["anchor"]
                .as_str()
                .unwrap()
                .starts_with("0102")
        );
        assert_eq!(value["drag"]["kind"], "move");
        assert_eq!(value["drag"]["object"], "010000000000000003");
        assert_eq!(value["drag"]["offset"], serde_json::json!([25.0, -4.0]));
        assert_eq!(value["groups"][0]["record"]["record"], "uuid:abc");
        assert_eq!(value["groups"][0]["scroll"], 120.5);
        assert!(value["groups"][1]["scroll"].is_null() && value["focusScroll"].is_null());
        assert_eq!(value["groups"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn snapshot_reports_the_focused_window_scroll_offset() {
        let (mut layout, records) = layout();
        layout.groups[1].scroll = Some(333.333);
        let value = snapshot_json(&layout, &records, None);
        assert_eq!(value["focusScroll"], 333.33);
        layout.groups[1].scroll = Some(f64::NAN);
        assert!(snapshot_json(&layout, &records, None)["focusScroll"].is_null());
    }

    #[test]
    fn snapshot_names_legacy_window_tabs_and_their_content_bounds() {
        let (mut layout, records) = layout();
        layout.groups[1].legacy = Some((
            "org.chromium.Chromium".into(),
            Rect::new(620.0, 76.0, 1000.0, 700.0),
        ));
        let value = snapshot_json(&layout, &records, None);
        assert!(value["groups"][0]["legacy"].is_null());
        assert_eq!(
            value["groups"][1]["legacy"],
            serde_json::json!({
                "app": "org.chromium.Chromium",
                "tab": "0000000000000028",
                "content": [620.0, 76.0, 380.0, 624.0],
            })
        );
        assert_eq!(value["covered"], false);
    }

    #[test]
    fn covered_desktop_publishes_no_focus_or_selection() {
        let (mut layout, records) = layout();
        layout.covered = true;
        let caret = Some((writer_block("b"), PresencePayload::Caret { pos: 2 }));
        let value = snapshot_json(&layout, &records, caret);
        assert!(
            value["focus"].is_null()
                && value["selection"].is_null()
                && value["focusRect"].is_null()
        );
    }

    #[test]
    fn drag_state_prefers_reorder_and_reports_resize_without_offset() {
        let moving = LiveLayout::drag_from_row(
            Some((7, Point::new(10.0, 10.0), Point::new(40.0, 5.0))),
            Some(2),
        );
        assert_eq!(
            moving,
            Some(LiveDragState {
                kind: LiveDragKind::Move,
                group: 7,
                offset: (30.0, -5.0)
            })
        );
        let resizing = LiveLayout::drag_from_row(None, Some(2));
        assert_eq!(
            resizing,
            Some(LiveDragState {
                kind: LiveDragKind::Resize,
                group: 2,
                offset: (0.0, 0.0)
            })
        );
        assert_eq!(LiveLayout::drag_from_row(None, None), None);
    }

    fn input(publisher: &str, selection: Option<(Vec<u8>, Vec<u8>)>) -> RemoteInput {
        RemoteInput {
            publisher: publisher.into(),
            name: Some("Ada".into()),
            color: Some("#ec4899".into()),
            selection: selection.map(|(a, h)| RemoteSelectionInput {
                anchor: hex(&a),
                head: hex(&h),
            }),
            typing: None,
        }
    }

    #[test]
    fn in_flight_typing_is_shown_in_the_caret_label_until_it_lands() {
        let mut presence = PresenceCtx::empty();
        let mut projection = RemoteProjection::default();
        let caret =
            encode_selection(&writer_block("a"), &PresencePayload::Caret { pos: 1 }).unwrap();
        let mut row = input("aa01", Some(caret));
        row.typing = Some("hel".into());
        assert!(projection.apply(&mut presence, vec![row.clone()]));
        let user = UserId("aa01".into());
        assert_eq!(presence.display_name(&user), "Ada \u{201c}hel\u{201d}");
        row.typing = Some("x".repeat(50) + "\nend");
        assert!(projection.apply(&mut presence, vec![row.clone()]));
        let label = presence.display_name(&user);
        assert!(
            label.starts_with("Ada \u{201c}\u{2026}") && label.ends_with("x end\u{201d}"),
            "{label}"
        );
        // The durable change landed: the preview is dropped.
        row.typing = None;
        assert!(projection.apply(&mut presence, vec![row]));
        assert_eq!(presence.display_name(&user), "Ada");
    }

    #[test]
    fn remote_projection_places_moves_and_clears_carets_and_selections() {
        let mut presence = PresenceCtx::empty();
        let mut projection = RemoteProjection::default();
        let a = writer_block("a");
        let b = writer_block("b");
        let sel = encode_selection(&a, &PresencePayload::Selection { start: 2, end: 6 }).unwrap();
        assert!(projection.apply(&mut presence, vec![input("aa01", Some(sel.clone()))]));
        let user = UserId("aa01".into());
        assert_eq!(
            presence.selection_for(&a, &user),
            Some(SelectionRange { start: 2, end: 6 })
        );
        assert_eq!(presence.caret_for(&a, &user), Some(6));
        assert_eq!(presence.display_name(&user), "Ada");
        assert_eq!(presence.color_for(&user), (0xec, 0x48, 0x99));
        // Unchanged input is not a presence change (no redraw scheduled).
        assert!(!projection.apply(&mut presence, vec![input("aa01", Some(sel))]));
        // A caret moving to another block leaves nothing behind.
        let caret = encode_selection(&b, &PresencePayload::Caret { pos: 1 }).unwrap();
        assert!(projection.apply(&mut presence, vec![input("aa01", Some(caret))]));
        assert_eq!(presence.selection_for(&a, &user), None);
        assert_eq!(presence.caret_for(&a, &user), None);
        assert_eq!(presence.caret_for(&b, &user), Some(1));
        // The member leaving clears its caret.
        assert!(projection.apply(&mut presence, vec![]));
        assert_eq!(presence.caret_for(&b, &user), None);
        assert!(projection.users().is_empty());
    }

    #[test]
    fn remote_projection_leaves_native_presence_alone() {
        let mut presence = PresenceCtx::empty();
        let a = writer_block("a");
        presence.set_caret(&a, UserId("native".into()), 3);
        let mut projection = RemoteProjection::default();
        let caret = encode_selection(&a, &PresencePayload::Caret { pos: 1 }).unwrap();
        projection.apply(&mut presence, vec![input("aa01", Some(caret))]);
        projection.apply(&mut presence, vec![]);
        assert_eq!(presence.caret_for(&a, &UserId("native".into())), Some(3));
    }

    /// `web/live-overlay.mjs` colours DOM cursors with a port of
    /// `user_color`; these vectors are asserted on both sides so a remote
    /// cursor and the same member's native caret always share a colour.
    #[test]
    fn member_colors_match_the_overlay_port() {
        let palette = ui::presence::PEER_PALETTE;
        for (id, index) in [
            ("dave", 3usize),
            ("eve", 1),
            ("a1b2c3d4", 1),
            (&"7f00aa55".repeat(8), 9),
        ] {
            assert_eq!(ui::presence::user_color(id), palette[index], "{id}");
        }
    }

    #[test]
    fn owner_devices_are_labelled_with_the_owner_name_not_a_guest_tag() {
        let owner = [7u8; 32];
        let guest = LiveMember {
            device: [1; 32],
            user: [2; 32],
            name: Some("Ada".into()),
            owner: false,
        };
        let rows = live_member_rows(
            vec![guest.clone()],
            vec![
                LiveMember {
                    device: [9; 32],
                    user: owner,
                    name: owner_display_name(Some("bob")),
                    owner: true,
                },
                LiveMember {
                    device: [8; 32],
                    user: owner,
                    name: owner_display_name(Some("Browser viewer")),
                    owner: true,
                },
                // A device already projected as a collaborator keeps that row.
                LiveMember {
                    device: [1; 32],
                    user: owner,
                    name: None,
                    owner: true,
                },
            ],
        );
        assert_eq!(rows.len(), 3);
        let by_device = |d: u8| {
            rows.iter()
                .find(|row| row["device"] == hex(&[d; 32]))
                .unwrap()
                .clone()
        };
        assert_eq!(by_device(1)["name"], "Ada");
        assert_eq!(by_device(1)["owner"], false);
        assert_eq!(by_device(9)["name"], "bob");
        assert_eq!(by_device(9)["owner"], true);
        assert_eq!(by_device(9)["user"], hex(&owner));
        // Host-issued machine label: no name; the overlay shows "Owner".
        assert!(by_device(8)["name"].is_null());
        assert_eq!(by_device(8)["owner"], true);
        assert_eq!(owner_display_name(Some("  ")), None);
        assert_eq!(owner_display_name(None), None);
    }

    #[test]
    fn live_record_maps_package_primary_and_bounds() {
        assert_eq!(
            live_record("so.tilo.chat", ""),
            Some(("so.tilo.chat".into(), PACKAGE_PRIMARY_RECORD.into()))
        );
        assert_eq!(live_record("", "x"), None);
        assert_eq!(live_record("a", &"r".repeat(129)), None);
    }
}
