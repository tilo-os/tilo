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
/// Largest viewport coordinate an `At` names, CSS pixels.
const MAX_COORDINATE: f64 = 100_000.0;
/// Upper bound on the text one fill may insert.
pub const MAX_FILL_BYTES: usize = 64 * 1024;
/// Upper bound on the options one select may choose.
pub const MAX_SELECT_VALUES: usize = 64;
/// Upper bound on one option value or label.
pub const MAX_SELECT_VALUE_BYTES: usize = 1024;
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
}

impl PageRequest {
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Outline => "outline",
            Self::Click { .. } => "click",
            Self::Fill { .. } => "fill",
            Self::Select { .. } => "select",
            Self::Press { .. } => "press",
            Self::Submit { .. } => "submit",
            Self::Changes => "changes",
            Self::At { .. } => "at",
            Self::Settle { .. } => "settle",
            Self::Console { .. } => "console",
            Self::Network { .. } => "network",
            Self::Reader { .. } => "reader",
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
            | Self::Reader { .. } => None,
            Self::Click { element }
            | Self::Fill { element, .. }
            | Self::Select { element, .. }
            | Self::Press {
                element: Some(element),
                ..
            }
            | Self::Submit { element, .. } => Some(element),
        }
    }

    /// Whether every field is within the bounds an engine accepts.
    pub fn valid(&self) -> bool {
        if self
            .element()
            .is_some_and(|element| !is_element_ref(element))
        {
            return false;
        }
        match self {
            Self::Fill { text, .. } => text.len() <= MAX_FILL_BYTES,
            Self::Select { values, .. } => {
                !values.is_empty()
                    && values.len() <= MAX_SELECT_VALUES
                    && values
                        .iter()
                        .all(|value| value.len() <= MAX_SELECT_VALUE_BYTES)
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
    /// The request started a main-frame navigation.
    pub navigated: bool,
    /// That navigation had not finished loading when the engine answered.
    pub loading: bool,
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
    match request {
        PageRequest::Fill { text, secret, .. } => {
            command["text"] = text.as_str().into();
            command["secret"] = (*secret).into();
        }
        PageRequest::Select { values, .. } => command["values"] = values.clone().into(),
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
            navigated,
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
            assert_eq!(
                reply.unwrap().outline.as_deref(),
                Some("- button \"Go\" [ref=d1e1]")
            );
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
