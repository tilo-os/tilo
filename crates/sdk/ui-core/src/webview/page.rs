//! Typed requests against a live page's content.
//!
//! Browser chrome turns its page actions into a [`PageRequest`]; the engine
//! answers with exactly one [`WebViewEvent::PageResult`](super::WebViewEvent)
//! carrying the same request id. Elements are addressed by the refs a prior
//! [`PageRequest::Outline`] printed. A ref names one element of one document,
//! so a ref from a page the tab has since left is refused rather than
//! resolved against whatever now sits at the same position.

use super::KeyModifiers;

/// Upper bound on an outline as it crosses the engine boundary.
pub const MAX_OUTLINE_BYTES: usize = 256 * 1024;
/// Upper bound on structured reply data (`PageReply::data`), serialised.
pub const MAX_DATA_BYTES: usize = 256 * 1024;
/// Bounds of a settle wait, milliseconds.
pub const MIN_SETTLE_QUIET_MS: u32 = 50;
pub const MAX_SETTLE_MS: u32 = 20_000;
/// Largest viewport coordinate a point may name, CSS pixels.
const MAX_COORDINATE: f64 = 100_000.0;
/// Upper bound on the points of one pointer path, and the steps of one
/// drag, so a gesture is always a bounded amount of work for the engine.
pub const MAX_PATH_POINTS: usize = 1024;
pub const MAX_DRAG_STEPS: usize = 64;
/// Upper bound on the text one fill may insert.
pub const MAX_FILL_BYTES: usize = 64 * 1024;
/// Upper bound on the options one select may choose.
pub const MAX_SELECT_VALUES: usize = 64;
/// Upper bound on one option value or label.
pub const MAX_SELECT_VALUE_BYTES: usize = 1024;
/// Upper bound on the files one fill of a file input may choose.
pub const MAX_FILE_PATHS: usize = 16;
/// Upper bound on one chosen file's path.
pub const MAX_FILE_PATH_BYTES: usize = 4096;
/// Upper bound on the steps of one pointer gesture.
pub const MAX_POINTER_STEPS: usize = 256;
/// Upper bound on the button names a gesture may name.
const POINTER_BUTTONS: &[&str] = &["left", "right"];
const MAX_REF_BYTES: usize = 40;
const MAX_MESSAGE_BYTES: usize = 1024;
const MAX_CODE_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum PageRequest {
    /// A compact accessibility outline of the document. Interactive elements
    /// carry the refs every other request addresses.
    Outline,
    /// A trusted pointer click at the element's visible centre.
    Click { element: String },
    /// A trusted pointer move to the element's visible centre, or to the
    /// viewport point (`x`, `y`) when no element is named: moving the pointer
    /// alone, with no button, so the page's hover styles apply.
    Hover {
        element: Option<String>,
        point: Option<(f64, f64)>,
    },
    /// A trusted press, move and release on the element's visible centre, or
    /// on the viewport point when no element is named: the drag and drop
    /// gesture of a person holding the pointer down and moving it.
    Drag {
        from: Option<String>,
        from_point: Option<(f64, f64)>,
        to: Option<String>,
        to_point: Option<(f64, f64)>,
        steps: u8,
    },
    /// A trusted contact path: press at the first point, move through the
    /// rest and release at the last, for drawing on canvases and other
    /// surfaces that read pointer contact. Points are viewport CSS pixels
    /// when the element is named and are measured from its padding box,
    /// otherwise from the viewport's top left.
    Path {
        element: Option<String>,
        points: Vec<(f64, f64)>,
    },
    /// Replace an editable element's contents with `text`. A password or
    /// card field is refused unless `secret` says the caller means it, and
    /// then the text is a secret the caller keeps out of logs.
    Fill {
        element: String,
        text: String,
        secret: bool,
    },
    /// Choose `<select>` options by value or visible label.
    Select {
        element: String,
        values: Vec<String>,
    },
    /// Choose the files of an `<input type=file>`, by absolute path.
    ///
    /// The caller has already checked each path is a regular file it may
    /// read; the engine hands those files to the page as a person's own
    /// picker would, dispatching `input` and `change`. A path that names no
    /// readable regular file, or one the element's `accept` refuses, is
    /// refused rather than silently dropped.
    FillFiles { element: String, paths: Vec<String> },
    /// A trusted key press, after focusing `element` when one is named.
    Press {
        element: Option<String>,
        key: PageKey,
    },
    /// Submit the element's form through the page's own validation. A form
    /// that sends its data to another site than the page's is refused unless
    /// `cross_origin` says the caller means it.
    Submit { element: String, cross_origin: bool },
    /// What changed in the outline since this tab's last read or changes:
    /// lines added (`+`) and removed (`-`), or the whole outline when the
    /// document is new.
    Changes,
    /// The element a click at viewport point (`x`, `y`) in CSS pixels would
    /// hit, and the nearest interactive element holding it.
    At { x: f64, y: f64 },
    /// Wait until the document has stopped changing for `quiet_ms` and has
    /// loaded, or `timeout_ms` passed.
    Settle { quiet_ms: u32, timeout_ms: u32 },
    /// Console messages and uncaught errors after sequence number `since`.
    Console { since: u64 },
    /// Network requests after sequence number `since`: method, URL, type,
    /// status and failure, never headers, bodies or cookies.
    Network { since: u64 },
    /// Show the document's article alone in a reading layout (`enabled`), or
    /// put the page back as it was. A page without an article is refused.
    Reader { enabled: bool },
    /// Move the pointer over the page as a hand does: `steps` in order, a
    /// press and release of `button` where the page asks for one. A step
    /// naming `element` is placed in that element's box, every other step is
    /// a viewport position in CSS pixels. All of it is real input to the
    /// page, so hover, drag and drop and drawing see what a person's hand
    /// would have done, and the reply reports what changed.
    Pointer {
        steps: Vec<PointerStep>,
        button: PointerButton,
    },
}

/// One step of a [`PageRequest::Pointer`] gesture.
#[derive(Debug, Clone, PartialEq)]
pub struct PointerStep {
    pub kind: PointerStepKind,
    /// Where the step happens, relative to `element`'s box when it names one.
    pub at: PointerPoint,
    /// The element the step is placed against. A gesture that keeps naming
    /// the same element keeps stepping inside the same box.
    pub element: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerStepKind {
    Move,
    Down,
    Up,
}

impl PointerStepKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Down => "down",
            Self::Up => "up",
        }
    }
}

/// Where a step happens: inside an element's box or at a viewport point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerPoint {
    InElement { x: f64, y: f64 },
    Viewport { x: f64, y: f64 },
}

impl PointerPoint {
    pub const fn coordinates(self) -> (f64, f64) {
        match self {
            Self::InElement { x, y } | Self::Viewport { x, y } => (x, y),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Right,
}

impl PointerButton {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    /// The button a gesture names, left when the name is not one we know.
    pub const fn parse(name: &str) -> Self {
        match name.as_bytes() {
            b"right" => Self::Right,
            _ => Self::Left,
        }
    }
}

impl PageRequest {
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Outline => "outline",
            Self::Click { .. } => "click",
            Self::Hover { .. } => "hover",
            Self::Drag { .. } => "drag",
            Self::Path { .. } => "path",
            Self::Fill { .. } => "fill",
            Self::Select { .. } => "select",
            Self::FillFiles { .. } => "fill_files",
            Self::Press { .. } => "press",
            Self::Submit { .. } => "submit",
            Self::Changes => "changes",
            Self::At { .. } => "at",
            Self::Settle { .. } => "settle",
            Self::Console { .. } => "console",
            Self::Network { .. } => "network",
            Self::Reader { .. } => "reader",
            Self::Pointer { .. } => "pointer",
        }
    }

    /// What the request does, in the words an answer that a dialog it opened
    /// cut short repeats: `Clicked element d1e5`, `Submitted form d1e7`.
    pub fn described(&self) -> String {
        match self {
            Self::Outline => "Read the page".to_string(),
            Self::Click { element } => format!("Clicked element {element}"),
            Self::Hover {
                element: Some(element),
                ..
            } => format!("Moved the pointer over element {element}"),
            Self::Hover { element: None, .. } => "Moved the pointer".to_string(),
            Self::Drag { .. } => "Dragged on the page".to_string(),
            Self::Path { .. } => "Drew on the page".to_string(),
            Self::Fill { element, .. } => format!("Filled element {element}"),
            Self::Select { element, values } => {
                format!("Chose {} on element {element}", values.join(", "))
            }
            Self::FillFiles { element, paths } => {
                format!("Chose {} file(s) for element {element}", paths.len())
            }
            Self::Press { element: None, key } => format!("Pressed {}", key.chord()),
            Self::Press {
                element: Some(element),
                key,
            } => format!("Pressed {} on element {element}", key.chord()),
            Self::Submit { element, .. } => format!("Submitted form {element}"),
            Self::Changes => "Read what changed".to_string(),
            Self::At { x, y } => format!("Read the element at {x}, {y}"),
            Self::Settle {
                quiet_ms,
                timeout_ms,
            } => format!("Waited for the page to settle ({quiet_ms} ms, up to {timeout_ms} ms)"),
            Self::Console { .. } => "Read the console".to_string(),
            Self::Network { .. } => "Read the network".to_string(),
            Self::Reader { enabled: true } => "Showed the article alone".to_string(),
            Self::Reader { enabled: false } => "Put the page back as it was".to_string(),
            Self::Pointer { steps, .. } => format!("Moved the pointer ({} steps)", steps.len()),
        }
    }

    pub fn element(&self) -> Option<&str> {
        match self {
            Self::Outline
            | Self::Press { element: None, .. }
            | Self::Changes
            | Self::At { .. }
            | Self::Settle { .. }
            | Self::Console { .. }
            | Self::Network { .. }
            | Self::Reader { .. }
            | Self::Pointer { .. }
            | Self::Hover { element: None, .. }
            | Self::Drag {
                from: None,
                to: None,
                ..
            }
            | Self::Path { element: None, .. } => None,
            Self::Click { element }
            | Self::Fill { element, .. }
            | Self::Select { element, .. }
            | Self::FillFiles { element, .. }
            | Self::Press {
                element: Some(element),
                ..
            }
            | Self::Submit { element, .. }
            | Self::Hover {
                element: Some(element),
                ..
            }
            | Self::Drag {
                from: Some(element),
                ..
            }
            | Self::Drag {
                from: None,
                to: Some(element),
                ..
            }
            | Self::Path {
                element: Some(element),
                ..
            } => Some(element),
        }
    }

    /// Whether every field is within the bounds an engine accepts.
    #[allow(
        clippy::too_many_lines,
        reason = "one match names every request's bounds, so none can be forgotten"
    )]
    pub fn valid(&self) -> bool {
        let refs: Vec<&str> = match self {
            Self::Pointer { steps, .. } => {
                steps.iter().filter_map(|s| s.element.as_deref()).collect()
            }
            _ => self.element().into_iter().collect(),
        };
        if refs.iter().any(|element| !is_element_ref(element)) {
            return false;
        }
        match self {
            Self::Hover { element, point } => {
                matches!((element, point), (Some(_), None) | (None, Some(_)))
            }
            Self::Drag {
                from,
                from_point,
                to,
                to_point,
                steps,
            } => {
                let known = |element: &Option<String>, point: &Option<(f64, f64)>| {
                    matches!((element, point), (Some(_), None) | (None, Some(_)))
                };
                known(from, from_point)
                    && known(to, to_point)
                    && from_point
                        .iter()
                        .chain(to_point)
                        .all(|point| point_is_viewport(*point))
                    && (1..=MAX_DRAG_STEPS).contains(&usize::from(*steps))
            }
            Self::Path { element, points } => {
                let relative = element.is_some();
                (2..=MAX_PATH_POINTS).contains(&points.len())
                    && points.iter().all(|&point| {
                        if relative {
                            point.0.is_finite() && point.1.is_finite()
                        } else {
                            point_is_viewport(point)
                        }
                    })
            }
            Self::Fill { text, .. } => text.len() <= MAX_FILL_BYTES,
            Self::Select { values, .. } => {
                !values.is_empty()
                    && values.len() <= MAX_SELECT_VALUES
                    && values
                        .iter()
                        .all(|value| value.len() <= MAX_SELECT_VALUE_BYTES)
            }
            Self::FillFiles { paths, .. } => {
                !paths.is_empty()
                    && paths.len() <= MAX_FILE_PATHS
                    && paths
                        .iter()
                        .all(|path| path.len() <= MAX_FILE_PATH_BYTES && path.starts_with('/'))
            }
            Self::Press { key, .. } => PageKey::parse(&key.chord()).as_ref() == Some(key),
            Self::At { x, y } => [x, y]
                .iter()
                .all(|v| v.is_finite() && (0.0..=MAX_COORDINATE).contains(*v)),
            Self::Settle {
                quiet_ms,
                timeout_ms,
            } => {
                (MIN_SETTLE_QUIET_MS..=MAX_SETTLE_MS).contains(quiet_ms)
                    && (*quiet_ms..=MAX_SETTLE_MS).contains(timeout_ms)
            }
            Self::Pointer { steps, button } => {
                !steps.is_empty()
                    && steps.len() <= MAX_POINTER_STEPS
                    && POINTER_BUTTONS.contains(&button.as_str())
                    && steps.iter().all(|step| {
                        let (x, y) = step.at.coordinates();
                        [x, y]
                            .iter()
                            .all(|v| v.is_finite() && (0.0..=MAX_COORDINATE).contains(v))
                    })
            }
            Self::Outline
            | Self::Click { .. }
            | Self::Submit { .. }
            | Self::Changes
            | Self::Console { .. }
            | Self::Network { .. }
            | Self::Reader { .. } => true,
        }
    }
}

/// A finite point inside the viewport coordinate bounds.
fn point_is_viewport((x, y): (f64, f64)) -> bool {
    [x, y]
        .iter()
        .all(|v| v.is_finite() && (0.0..=MAX_COORDINATE).contains(v))
}

/// Element refs are `d<document>e<element>`: both parts are decimal, so a ref
/// can never smuggle selector or script syntax to the engine.
pub fn is_element_ref(value: &str) -> bool {
    let Some((document, element)) = value
        .strip_prefix('d')
        .and_then(|rest| rest.split_once('e'))
    else {
        return false;
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    value.len() <= MAX_REF_BYTES && digits(document) && digits(element)
}

/// One key plus modifiers, written as a chord such as `Enter`, `Shift+Tab`
/// or `Control+a`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageKey {
    /// A named key (`Enter`, `ArrowDown`, …) or a single character.
    pub key: String,
    pub modifiers: KeyModifiers,
}

/// Keys the engines map to a physical key. `Space` is spelled out because a
/// literal space is invisible in a chord.
const NAMED_KEYS: &[&str] = &[
    "Enter",
    "Tab",
    "Escape",
    "Backspace",
    "Delete",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Home",
    "End",
    "PageUp",
    "PageDown",
];

impl PageKey {
    pub fn parse(chord: &str) -> Option<Self> {
        let mut parts: Vec<&str> = chord.split('+').collect();
        // `Control++` names the plus key itself.
        if chord.ends_with("++") {
            parts.truncate(parts.len().saturating_sub(2));
            parts.push("+");
        }
        let key = parts.pop()?;
        let mut modifiers = KeyModifiers::default();
        for part in parts {
            let flag = match part {
                "Control" | "Ctrl" => &mut modifiers.ctrl,
                "Shift" => &mut modifiers.shift,
                "Alt" => &mut modifiers.alt,
                "Meta" => &mut modifiers.meta,
                _ => return None,
            };
            if std::mem::replace(flag, true) {
                return None;
            }
        }
        let key = match key {
            "Space" => " ",
            named if NAMED_KEYS.contains(&named) => named,
            character if character.chars().count() == 1 => character,
            _ => return None,
        };
        Some(Self {
            key: key.to_string(),
            modifiers,
        })
    }

    /// The canonical chord, which [`Self::parse`] reads back unchanged.
    pub fn chord(&self) -> String {
        let mut chord = String::new();
        for (held, name) in [
            (self.modifiers.ctrl, "Control+"),
            (self.modifiers.shift, "Shift+"),
            (self.modifiers.alt, "Alt+"),
            (self.modifiers.meta, "Meta+"),
        ] {
            if held {
                chord.push_str(name);
            }
        }
        chord.push_str(if self.key == " " { "Space" } else { &self.key });
        chord
    }
}

/// What a completed page request observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageReply {
    /// The main document's URL when the request finished.
    pub url: String,
    pub message: String,
    /// Present for [`PageRequest::Outline`], and the line diff for
    /// [`PageRequest::Changes`].
    pub outline: Option<String>,
    /// Structured findings: the element at a point, how a settle went, log
    /// entries. Always page content, so never trusted as instructions.
    pub data: Option<serde_json::Value>,
    /// Whether the request led the page away from its document.
    pub navigation: PageNavigation,
    /// The navigation it started had not finished loading when the engine
    /// answered.
    pub loading: bool,
}

/// Whether a page request led the page to another document, as far as the
/// engine can tell when it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageNavigation {
    /// The request ran none of the page's code, so it cannot have.
    None,
    /// The page began a main-frame navigation while it handled the request.
    Started,
    /// It had not by the time it handled the request, and may still: a
    /// navigation the page starts later arrives as its own document request in
    /// the network log.
    Possible,
}

impl PageNavigation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Started => "started",
            Self::Possible => "possible",
        }
    }
}

/// Why a page request did nothing. `code` is stable, such as
/// `stale_element`, `obscured` or `unavailable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageFailure {
    pub code: String,
    pub message: String,
}

impl PageFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// The answer of an engine without page automation.
    pub fn unavailable() -> Self {
        Self::new(
            "unavailable",
            "This browser engine cannot read or operate page content.",
        )
    }
}

pub(crate) fn encode_request(request_id: u32, request: &PageRequest) -> serde_json::Value {
    let mut command = serde_json::json!({
        "op": "page",
        "request_id": request_id,
        "action": request.action(),
    });
    if let Some(element) = request.element() {
        command["element"] = element.into();
    }
    if let PageRequest::Drag {
        to: Some(target), ..
    } = request
    {
        command["to"] = target.as_str().into();
    }
    match request {
        PageRequest::Fill { text, secret, .. } => {
            command["text"] = text.as_str().into();
            command["secret"] = (*secret).into();
        }
        PageRequest::Select { values, .. } => command["values"] = values.clone().into(),
        PageRequest::FillFiles { paths, .. } => command["paths"] = paths.clone().into(),
        PageRequest::Press { key, .. } => {
            command["key"] = key.key.as_str().into();
            command["ctrl"] = key.modifiers.ctrl.into();
            command["shift"] = key.modifiers.shift.into();
            command["alt"] = key.modifiers.alt.into();
            command["meta"] = key.modifiers.meta.into();
        }
        PageRequest::Submit { cross_origin, .. } => {
            command["cross_origin"] = (*cross_origin).into()
        }
        PageRequest::At { x, y } => {
            command["x"] = (*x).into();
            command["y"] = (*y).into();
        }
        PageRequest::Settle {
            quiet_ms,
            timeout_ms,
        } => {
            command["quiet_ms"] = (*quiet_ms).into();
            command["timeout_ms"] = (*timeout_ms).into();
        }
        PageRequest::Console { since } | PageRequest::Network { since } => {
            command["since"] = (*since).into();
        }
        PageRequest::Reader { enabled } => command["enabled"] = (*enabled).into(),
        PageRequest::Pointer { steps, button } => {
            command["button"] = button.as_str().into();
            command["steps"] = steps
                .iter()
                .map(|step| {
                    let (x, y) = step.at.coordinates();
                    let mut encoded = serde_json::json!({
                        "kind": step.kind.as_str(),
                        "x": x,
                        "y": y,
                    });
                    if let Some(element) = &step.element {
                        encoded["element"] = element.as_str().into();
                    }
                    encoded
                })
                .collect();
        }
        PageRequest::Hover { element, point } => {
            // An unnamed element hovers a viewport point instead.
            if element.is_none()
                && let Some((x, y)) = point
            {
                command["action"] = "at".into();
                command["hover"] = true.into();
                command["x"] = (*x).into();
                command["y"] = (*y).into();
            }
        }
        PageRequest::Drag {
            from_point,
            to_point,
            steps,
            ..
        } => {
            // A named element travels as its ref under `element`; the
            // destination element under `to`; unnamed ends as points.
            if let Some((x, y)) = from_point {
                command["x"] = (*x).into();
                command["y"] = (*y).into();
            }
            if let Some((x, y)) = to_point {
                command["to_x"] = (*x).into();
                command["to_y"] = (*y).into();
            }
            command["steps"] = (*steps).into();
        }
        PageRequest::Path { points, .. } => {
            command["points"] = serde_json::Value::Array(
                points
                    .iter()
                    .map(|&(x, y)| serde_json::json!([x, y]))
                    .collect(),
            );
        }
        PageRequest::Outline | PageRequest::Click { .. } | PageRequest::Changes => {}
    }
    command
}

/// Decode a `page_result` event, rejecting anything outside the bounds above.
pub(crate) fn decode_result(
    value: &serde_json::Value,
) -> Option<(u32, Result<PageReply, PageFailure>)> {
    let request_id = u32::try_from(value["request_id"].as_u64()?)
        .ok()
        .filter(|id| *id > 0 && i32::try_from(*id).is_ok())?;
    let bounded = |key: &str, limit: usize| {
        value[key]
            .as_str()
            .filter(|text| text.len() <= limit)
            .map(str::to_string)
    };
    let message = bounded("message", MAX_MESSAGE_BYTES)?;
    let result = if value["ok"].as_bool()? {
        let outline = match &value["outline"] {
            serde_json::Value::Null => None,
            _ => Some(bounded("outline", MAX_OUTLINE_BYTES)?),
        };
        let navigated = value["navigated"].as_bool()?;
        // An engine from before `navigation` only knew whether one began soon after.
        let navigation = match value["navigation"].as_str() {
            Some("none") => PageNavigation::None,
            Some("started") => PageNavigation::Started,
            Some("possible") => PageNavigation::Possible,
            Some(_) => return None,
            None if navigated => PageNavigation::Started,
            None => PageNavigation::Possible,
        };
        if navigated != (navigation == PageNavigation::Started) {
            return None;
        }
        let data = match &value["data"] {
            serde_json::Value::Null => None,
            data @ serde_json::Value::Object(_) => {
                if runtime_collab::wire_json_len(data) > MAX_DATA_BYTES {
                    return None;
                }
                Some(data.clone())
            }
            _ => return None,
        };
        Ok(PageReply {
            url: bounded("url", 8192)?,
            message,
            outline,
            data,
            navigation,
            loading: navigated && value["loading"].as_bool()?,
        })
    } else {
        Err(PageFailure {
            code: bounded("code", MAX_CODE_BYTES).filter(|code| !code.is_empty())?,
            message,
        })
    };
    Some((request_id, result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_says_what_it_did_in_the_words_its_answer_repeats() {
        let element = || "d1e5".to_string();
        let described = |request: PageRequest| request.described();
        assert_eq!(
            described(PageRequest::Click { element: element() }),
            "Clicked element d1e5"
        );
        assert_eq!(
            described(PageRequest::Fill {
                element: element(),
                text: "Maria".into(),
                secret: false,
            }),
            "Filled element d1e5"
        );
        assert_eq!(
            described(PageRequest::Submit {
                element: element(),
                cross_origin: false,
            }),
            "Submitted form d1e5"
        );
        assert_eq!(
            described(PageRequest::Select {
                element: element(),
                values: vec!["a".into(), "b".into()],
            }),
            "Chose a, b on element d1e5"
        );
        let enter = PageKey::parse("Enter").unwrap();
        assert_eq!(
            described(PageRequest::Press {
                element: None,
                key: enter.clone(),
            }),
            "Pressed Enter"
        );
        assert_eq!(
            described(PageRequest::Press {
                element: Some(element()),
                key: enter,
            }),
            "Pressed Enter on element d1e5"
        );
    }

    #[test]
    fn requests_are_bounded_and_round_trip_to_the_wire() {
        // phase: element_refs_accept_only_document_and_element_numbers
        {
            assert!(is_element_ref("d1e2"));
            assert!(is_element_ref("d12e345"));
            for bad in [
                "",
                "e2",
                "d1",
                "d1e",
                "de2",
                "d1e2x",
                "d1e2;alert(1)",
                "D1E2",
            ] {
                assert!(!is_element_ref(bad), "{bad}");
            }
            assert!(!is_element_ref(&format!("d1e{}", "9".repeat(40))));
        }
        // phase: key_chords_parse_to_canonical_form
        {
            let key = PageKey::parse("Ctrl+Shift+a").unwrap();
            assert_eq!(key.key, "a");
            assert!(key.modifiers.ctrl && key.modifiers.shift && !key.modifiers.alt);
            assert_eq!(key.chord(), "Control+Shift+a");
            assert_eq!(PageKey::parse(&key.chord()), Some(key));
            assert_eq!(PageKey::parse("Space").unwrap().key, " ");
            assert_eq!(PageKey::parse("Control++").unwrap().key, "+");
            for bad in ["", "Enterr", "Hyper+a", "Shift+Shift+a", "ab", "Control+"] {
                assert!(PageKey::parse(bad).is_none(), "{bad}");
            }
        }
        // phase: invalid_requests_are_refused_before_the_engine
        {
            let element = "d1e2".to_string();
            assert!(PageRequest::Outline.valid());
            assert!(
                PageRequest::Fill {
                    element: element.clone(),
                    text: "hello".into(),
                    secret: false,
                }
                .valid()
            );
            assert!(
                !PageRequest::Fill {
                    element: element.clone(),
                    text: "x".repeat(MAX_FILL_BYTES + 1),
                    secret: false,
                }
                .valid()
            );
            assert!(
                !PageRequest::Select {
                    element: element.clone(),
                    values: Vec::new()
                }
                .valid()
            );
            assert!(
                !PageRequest::Click {
                    element: "#submit".into()
                }
                .valid()
            );
            // A chosen file is an absolute path, so a relative one never reaches
            // the engine to resolve against the browser's own working folder.
            assert!(
                PageRequest::FillFiles {
                    element: element.clone(),
                    paths: vec!["/var/home/you/scan.pdf".into()],
                }
                .valid()
            );
            for paths in [
                Vec::new(),
                vec!["scan.pdf".into()],
                vec!["./scan.pdf".into()],
                vec![format!("/{}", "x".repeat(MAX_FILE_PATH_BYTES))],
                (0..=MAX_FILE_PATHS)
                    .map(|n| format!("/var/home/you/{n}.pdf"))
                    .collect(),
            ] {
                assert!(
                    !PageRequest::FillFiles {
                        element: element.clone(),
                        paths,
                    }
                    .valid()
                );
            }
        }
        // phase: pointer_gestures_are_bounded_and_round_trip_to_the_wire
        {
            let inside = |kind, element: Option<&str>, x, y| PointerStep {
                kind,
                at: PointerPoint::InElement { x, y },
                element: element.map(str::to_owned),
            };
            let gesture = |steps: Vec<PointerStep>, button| PageRequest::Pointer { steps, button };
            let drag = gesture(
                vec![
                    inside(PointerStepKind::Down, Some("d1e2"), 4.0, 4.0),
                    inside(PointerStepKind::Move, Some("d1e3"), 4.0, 4.0),
                    PointerStep {
                        kind: PointerStepKind::Up,
                        at: PointerPoint::Viewport { x: 40.0, y: 90.0 },
                        element: None,
                    },
                ],
                PointerButton::Left,
            );
            assert!(drag.valid());
            assert_eq!(drag.element(), None, "a gesture addresses many elements");
            assert_eq!(
                encode_request(13, &drag),
                serde_json::json!({"op":"page","request_id":13,"action":"pointer","button":"left",
                    "steps":[
                        {"kind":"down","x":4.0,"y":4.0,"element":"d1e2"},
                        {"kind":"move","x":4.0,"y":4.0,"element":"d1e3"},
                        {"kind":"up","x":40.0,"y":90.0}]})
            );
            // A step naming no element is a viewport point, and it is
            // written without the key rather than with a null.
            assert!(
                encode_request(14, &drag)["steps"][2]
                    .get("element")
                    .is_none()
            );
            let right = gesture(
                vec![PointerStep {
                    kind: PointerStepKind::Down,
                    at: PointerPoint::Viewport { x: 1.0, y: 2.0 },
                    element: None,
                }],
                PointerButton::Right,
            );
            assert_eq!(encode_request(15, &right)["button"], "right");
            for bad in [
                // A gesture is a movement of the hand, never empty.
                gesture(Vec::new(), PointerButton::Left),
                // A step the engine cannot place.
                gesture(
                    vec![PointerStep {
                        kind: PointerStepKind::Down,
                        at: PointerPoint::InElement {
                            x: f64::INFINITY,
                            y: 0.0,
                        },
                        element: None,
                    }],
                    PointerButton::Left,
                ),
                gesture(
                    vec![PointerStep {
                        kind: PointerStepKind::Down,
                        at: PointerPoint::Viewport {
                            x: 0.0,
                            y: MAX_COORDINATE + 1.0,
                        },
                        element: None,
                    }],
                    PointerButton::Left,
                ),
                // A step that names something other than an element ref.
                gesture(
                    vec![inside(PointerStepKind::Move, Some("#menu"), 0.0, 0.0)],
                    PointerButton::Left,
                ),
                // More steps than any gesture needs.
                gesture(
                    (0..=MAX_POINTER_STEPS)
                        .map(|_| inside(PointerStepKind::Move, None, 0.0, 0.0))
                        .collect(),
                    PointerButton::Left,
                ),
            ] {
                assert!(!bad.valid(), "{bad:?}");
            }
            assert_eq!(PointerButton::parse("right"), PointerButton::Right);
            assert_eq!(
                PointerButton::parse("middle"),
                PointerButton::Left,
                "an unknown button is the one a hand uses"
            );
        }
        // phase: wire_encoding_names_every_field
        {
            let command = encode_request(
                7,
                &PageRequest::Press {
                    element: Some("d1e2".into()),
                    key: PageKey::parse("Shift+Enter").unwrap(),
                },
            );
            assert_eq!(
                command,
                serde_json::json!({"op":"page","request_id":7,"action":"press","element":"d1e2",
                    "key":"Enter","ctrl":false,"shift":true,"alt":false,"meta":false})
            );
            let command = encode_request(
                8,
                &PageRequest::Select {
                    element: "d1e3".into(),
                    values: vec!["NL".into()],
                },
            );
            assert_eq!(command["values"], serde_json::json!(["NL"]));
        }
        // phase: agent_requests_name_their_arguments_and_bounds
        {
            let at = encode_request(9, &PageRequest::At { x: 12.5, y: 40.0 });
            assert_eq!(
                at,
                serde_json::json!({"op":"page","request_id":9,"action":"at","x":12.5,"y":40.0})
            );
            let settle = PageRequest::Settle {
                quiet_ms: 500,
                timeout_ms: 10_000,
            };
            assert!(settle.valid());
            assert_eq!(encode_request(10, &settle)["timeout_ms"], 10_000);
            assert_eq!(
                encode_request(11, &PageRequest::Network { since: 42 })["since"],
                42
            );
            assert_eq!(
                encode_request(
                    12,
                    &PageRequest::Submit {
                        element: "d1e2".into(),
                        cross_origin: true,
                    }
                )["cross_origin"],
                true
            );
            for bad in [
                PageRequest::At { x: -1.0, y: 0.0 },
                PageRequest::At {
                    x: f64::NAN,
                    y: 0.0,
                },
                PageRequest::Settle {
                    quiet_ms: 10,
                    timeout_ms: 1000,
                },
                PageRequest::Settle {
                    quiet_ms: 500,
                    timeout_ms: 100,
                },
                PageRequest::Settle {
                    quiet_ms: 500,
                    timeout_ms: MAX_SETTLE_MS + 1,
                },
            ] {
                assert!(!bad.valid(), "{bad:?}");
            }
            assert!(PageRequest::Changes.element().is_none());
        }
        // phase: pointer_gestures_are_bounded_and_name_their_two_ends
        {
            let hover = PageRequest::Hover {
                element: Some("d1e4".into()),
                point: None,
            };
            assert!(hover.valid());
            assert_eq!(hover.element(), Some("d1e4"));
            assert_eq!(
                encode_request(20, &hover),
                serde_json::json!({"op":"page","request_id":20,"action":"hover","element":"d1e4"})
            );
            // A point without an element hovers there instead of clicks.
            let at = PageRequest::Hover {
                element: None,
                point: Some((30.0, 44.5)),
            };
            assert!(at.valid());
            assert_eq!(
                encode_request(21, &at),
                serde_json::json!({"op":"page","request_id":21,"action":"at","hover":true,
                    "x":30.0,"y":44.5})
            );
            for bad in [
                PageRequest::Hover {
                    element: None,
                    point: None,
                },
                PageRequest::Hover {
                    element: Some("#menu".into()),
                    point: None,
                },
                PageRequest::Hover {
                    element: Some("d1e4".into()),
                    point: Some((1.0, 1.0)),
                },
            ] {
                assert!(!bad.valid(), "{bad:?}");
            }
            let drag = PageRequest::Drag {
                from: Some("d1e4".into()),
                from_point: None,
                to: Some("d1e6".into()),
                to_point: None,
                steps: 8,
            };
            assert!(drag.valid());
            assert_eq!(
                encode_request(22, &drag),
                serde_json::json!({"op":"page","request_id":22,"action":"drag","element":"d1e4",
                    "to":"d1e6","steps":8})
            );
            let by_point = PageRequest::Drag {
                from: None,
                from_point: Some((10.0, 20.0)),
                to: None,
                to_point: Some((70.0, 80.0)),
                steps: 4,
            };
            assert!(by_point.valid());
            assert_eq!(
                encode_request(23, &by_point),
                serde_json::json!({"op":"page","request_id":23,"action":"drag","x":10.0,"y":20.0,
                    "to_x":70.0,"to_y":80.0,"steps":4})
            );
            for bad in [
                PageRequest::Drag {
                    from: None,
                    from_point: None,
                    to: Some("d1e6".into()),
                    to_point: None,
                    steps: 4,
                },
                PageRequest::Drag {
                    from: Some("d1e4".into()),
                    from_point: None,
                    to: None,
                    to_point: None,
                    steps: 4,
                },
                PageRequest::Drag {
                    from: Some("d1e4".into()),
                    from_point: None,
                    to: Some("d1e6".into()),
                    to_point: None,
                    steps: 0,
                },
                PageRequest::Drag {
                    from: Some("d1e4".into()),
                    from_point: None,
                    to: Some("d1e6".into()),
                    to_point: None,
                    steps: 65,
                },
                PageRequest::Drag {
                    from: None,
                    from_point: Some((f64::INFINITY, 0.0)),
                    to: Some("d1e6".into()),
                    to_point: None,
                    steps: 4,
                },
            ] {
                assert!(!bad.valid(), "{bad:?}");
            }
            let path = PageRequest::Path {
                element: Some("d1e9".into()),
                points: vec![(1.0, 2.0), (3.5, 4.5), (60.0, 70.0)],
            };
            assert!(path.valid());
            let wire = encode_request(24, &path);
            assert_eq!(
                wire["points"],
                serde_json::json!([[1.0, 2.0], [3.5, 4.5], [60.0, 70.0]])
            );
            assert_eq!(wire["element"], "d1e9");
            let free = PageRequest::Path {
                element: None,
                points: vec![(1.0, 2.0), (3.0, 4.0)],
            };
            assert!(free.valid());
            assert!(encode_request(25, &free).get("element").is_none());
            for bad in [
                PageRequest::Path {
                    element: Some("d1e9".into()),
                    points: vec![(1.0, 2.0)],
                },
                PageRequest::Path {
                    element: Some("d1e9".into()),
                    points: vec![(1.0, 2.0); MAX_PATH_POINTS + 1],
                },
                PageRequest::Path {
                    element: None,
                    points: vec![(-1.0, 2.0), (3.0, 4.0)],
                },
                PageRequest::Path {
                    element: Some("d1e9".into()),
                    points: vec![(f64::NAN, 2.0), (3.0, 4.0)],
                },
            ] {
                assert!(!bad.valid(), "{bad:?}");
            }
        }
        // phase: findings_decode_as_data_within_their_bound
        {
            let reply = |data: serde_json::Value| {
                serde_json::json!({"request_id":3,"ok":true,"message":"2 requests","url":"https://a.test/",
                    "navigated":false,"loading":false,"outline":null,"data":data})
            };
            let (_, result) = decode_result(&reply(
                serde_json::json!({"entries":[{"url":"https://a.test/x"}]}),
            ))
            .expect("decodes");
            assert_eq!(
                result.expect("succeeded").data,
                Some(serde_json::json!({"entries":[{"url":"https://a.test/x"}]}))
            );
            assert!(decode_result(&reply(serde_json::json!("not an object"))).is_none());
            let huge = "x".repeat(MAX_DATA_BYTES);
            assert!(decode_result(&reply(serde_json::json!({"text": huge}))).is_none());
        }
        // phase: results_decode_within_bounds
        {
            let ok = serde_json::json!({"event":"page_result","request_id":3,"ok":true,
                "url":"https://a.test/","message":"Read","outline":"- button \"Go\" [ref=d1e1]","navigated":false,"loading":false});
            let (id, reply) = decode_result(&ok).unwrap();
            assert_eq!(id, 3);
            let reply = reply.unwrap();
            assert_eq!(reply.outline.as_deref(), Some("- button \"Go\" [ref=d1e1]"));
            assert_eq!(reply.navigation, PageNavigation::Possible);
            let read = serde_json::json!({"event":"page_result","request_id":3,"ok":true,
                "url":"https://a.test/","message":"Read","outline":null,"navigation":"none","navigated":false,"loading":false});
            assert_eq!(
                decode_result(&read).unwrap().1.unwrap().navigation,
                PageNavigation::None
            );
            let contradicts = serde_json::json!({"event":"page_result","request_id":3,"ok":true,
                "url":"https://a.test/","message":"Read","outline":null,"navigation":"started","navigated":false,"loading":false});
            assert!(decode_result(&contradicts).is_none());
            let failed = serde_json::json!({"event":"page_result","request_id":4,"ok":false,
                "code":"stale_element","message":"Read the page again"});
            assert_eq!(
                decode_result(&failed).unwrap().1,
                Err(PageFailure::new("stale_element", "Read the page again"))
            );
            let oversized = serde_json::json!({"event":"page_result","request_id":5,"ok":true,
                "url":"https://a.test/","message":"Read","outline":"x".repeat(MAX_OUTLINE_BYTES + 1),"navigated":false,"loading":false});
            assert!(decode_result(&oversized).is_none());
            let anonymous = serde_json::json!({"event":"page_result","request_id":0,"ok":false,
                "code":"busy","message":""});
            assert!(decode_result(&anonymous).is_none());
        }
    }
}
