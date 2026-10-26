//! Picking parts of a window drawn by another process: what the desktop
//! asks the app that draws a window while a person points at what is wrong
//! ([`crate::floating_surface::FloatingSurfaceHostMessage::Pick`]), and
//! what the app answers from its own retained tree with [`answer`]
//! ([`crate::floating_surface::FloatingSurfaceClientMessage::PickAnswered`]).
//!
//! Everything is in the window's logical coordinates, rectangles as
//! `[x0, y0, x1, y1]`. The desktop names the app by the launcher's id, never
//! by anything in an answer.

use serde::{Deserialize, Serialize};
use vello::kurbo::{Point, Rect};

use crate::widget::Widget;

/// Most parts one answer names: the chain under a point is never this deep
/// in a person's interface, and a dragged rectangle asks for fewer.
pub const MAX_PARTS: usize = 64;
/// Most parts one capture is asked for.
pub const MAX_CAPTURES: usize = 16;
/// Most recent actions a capture carries.
pub const MAX_RECENT: usize = crate::pick::RECENT_INVOCATIONS;

/// What the desktop asks a window while a person picks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PickQuery {
    /// The nameable parts under `point`, outermost first
    /// ([`crate::pick::targets_at`]).
    At { point: [f64; 2] },
    /// The outermost nameable parts wholly inside `area`, at most `limit`
    /// ([`crate::pick::targets_within`]).
    Within { area: [f64; 4], limit: u32 },
    /// The texts of the picked parts and where a picture of each must be
    /// masked ([`crate::pick::capture`], [`crate::pick::sensitive_areas`]),
    /// with what the person last did in the app.
    Capture { parts: Vec<PartToCapture> },
}

/// One picked part, by the path and bounds an earlier answer gave.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartToCapture {
    pub path: String,
    pub rect: [f64; 4],
}

/// A window's answer to a [`PickQuery`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PickAnswer {
    /// For [`PickQuery::At`] and [`PickQuery::Within`].
    Targets { parts: Vec<PickedPart> },
    /// For [`PickQuery::Capture`]: a part the tree no longer has is left
    /// out.
    Captured {
        parts: Vec<CapturedPart>,
        recent: Vec<RecentAction>,
    },
}

/// A part a person can pick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PickedPart {
    /// The widget path, `#i/j` addressing a synthetic child.
    pub path: String,
    /// Its role as assistive technology names it (`Button`, `ListItem`).
    pub role: String,
    /// What kind of thing it is in the app's own words ("Message").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Its name, when that is the app's own copy ("Send message").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<String>,
    pub rect: [f64; 4],
}

/// A picked part's texts, read again from the tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CapturedPart {
    pub path: String,
    /// `None` for a widget that names no semantics of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What kind of thing it is in the app's own words ("Message").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<CapturedText>,
    pub texts: Vec<CapturedText>,
    /// Where a picture of the part must be painted over.
    pub masked: Vec<[f64; 4]>,
}

/// One text under a picked part.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CapturedText {
    pub text: String,
    /// The app's own fixed copy, rather than content.
    pub interface: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<[f64; 4]>,
}

/// Something the person last did through a control of the app: names
/// only, never arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentAction {
    /// Unix milliseconds.
    pub at: i64,
    pub resource: String,
    pub action: String,
    /// The action's label as the app words it ("Post a message").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub done: bool,
}

/// `rect` as the wire carries it.
#[must_use]
pub fn rect_array(rect: Rect) -> [f64; 4] {
    [rect.x0, rect.y0, rect.x1, rect.y1]
}

/// A rectangle from the wire: finite, with an area.
#[must_use]
pub fn rect_of(coords: [f64; 4]) -> Option<Rect> {
    crate::floating_surface::protocol_rect(coords).ok()
}

fn part(target: crate::pick::PickTarget) -> PickedPart {
    PickedPart {
        role: target.role_name(),
        kind: target.kind.clone(),
        copy: target.name.clone().filter(|_| target.interface),
        path: target.path,
        rect: rect_array(target.rect),
    }
}

fn text(text: crate::pick::PickedText) -> CapturedText {
    CapturedText {
        text: text.text,
        interface: text.interface,
        rect: text.rect.map(rect_array),
    }
}

/// Answer `query` from the tree under `root`.
#[must_use]
pub fn answer(root: &dyn Widget, query: &PickQuery) -> PickAnswer {
    match query {
        PickQuery::At { point } => {
            let point = Point::new(point[0], point[1]);
            let parts = if point.x.is_finite() && point.y.is_finite() {
                crate::pick::targets_at(root, point)
                    .into_iter()
                    .take(MAX_PARTS)
                    .map(part)
                    .collect()
            } else {
                Vec::new()
            };
            PickAnswer::Targets { parts }
        }
        PickQuery::Within { area, limit } => {
            let limit = usize::try_from(*limit).unwrap_or(MAX_PARTS).min(MAX_PARTS);
            let parts = rect_of(*area).map_or_else(Vec::new, |area| {
                crate::pick::targets_within(root, area, limit)
                    .into_iter()
                    .take(limit)
                    .map(part)
                    .collect()
            });
            PickAnswer::Targets { parts }
        }
        PickQuery::Capture { parts } => {
            let parts = parts
                .iter()
                .take(MAX_CAPTURES)
                .filter_map(|asked| {
                    let whole = rect_of(asked.rect)?;
                    let captured = crate::pick::capture(root, &asked.path)?;
                    Some(CapturedPart {
                        path: asked.path.clone(),
                        role: captured.role.map(crate::pick::role_name),
                        kind: captured.kind,
                        label: captured.label.map(text),
                        texts: captured.texts.into_iter().map(text).collect(),
                        masked: crate::pick::sensitive_areas(root, &asked.path, whole)
                            .into_iter()
                            .map(rect_array)
                            .collect(),
                    })
                })
                .collect();
            let recent = crate::pick::recent_invocations()
                .into_iter()
                .rev()
                .take(MAX_RECENT)
                .rev()
                .map(|invocation| RecentAction {
                    at: invocation.at,
                    resource: invocation.resource,
                    action: invocation.action,
                    label: invocation.label,
                    done: invocation.done,
                })
                .collect();
            PickAnswer::Captured { parts, recent }
        }
    }
}

#[cfg(test)]
#[path = "pick_wire_tests.rs"]
mod tests;
