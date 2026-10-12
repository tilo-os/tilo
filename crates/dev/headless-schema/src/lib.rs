//! Lightweight parser for the `--headless-drive` wire format.

#[derive(Clone, Debug, PartialEq)]
pub enum SemanticAction {
    Activate,
    Focus,
    ToggleCheck,
    Increment,
    Decrement,
    Expand,
    Collapse,
    ScrollIntoView,
    ShowContextMenu,
    SetValue(String),
    SetSelection { anchor: usize, focus: usize },
}

impl SemanticAction {
    /// The wire name a scenario uses for this action.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Focus => "focus",
            Self::ToggleCheck => "toggle_check",
            Self::Increment => "increment",
            Self::Decrement => "decrement",
            Self::Expand => "expand",
            Self::Collapse => "collapse",
            Self::ScrollIntoView => "scroll_into_view",
            Self::ShowContextMenu => "show_context_menu",
            Self::SetValue(_) => "set_value",
            Self::SetSelection { .. } => "set_selection",
        }
    }
}

/// One thing to do to the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Click the centre of the widget addressed by `name` — either its
    /// test id or the `Parent/Child[index]` path fallback.
    ClickNamed(String),
    /// Click at raw logical coordinates.
    ClickAt(f64, f64),
    /// Direct-touch tap on a named widget.
    TapNamed(String),
    /// Direct-touch tap at logical coordinates.
    TapAt(f64, f64),
    /// Drag one touch from start to end, exercising tap cancellation and
    /// pixel-precise scrolling.
    SwipeAt(f64, f64, f64, f64),
    /// Press and release one named key (`Enter`, `Tab`, `Escape`, `Space`,
    /// `Backspace`, arrows) or a single character.
    Key(String),
    /// Type a string, one press/release pair per character.
    Type(String),
    /// Hold the given modifiers for subsequent actions. Empty clears them.
    Modifiers(Vec<String>),
    /// Drive frames until a named retained widget appears, or fail after the
    /// bounded timeout. This lets saved scenarios synchronize with workers
    /// and asynchronous model delegates without timing sleeps.
    WaitForNamed { target: String, timeout_ms: u64 },
    /// Require semantic output or state at this exact point in a scenario.
    Expect(SemanticExpectation),
    /// Ask the named widget to perform an accessibility action, through the
    /// same path as a live accessibility request.
    Perform {
        target: String,
        semantic_target: Option<String>,
        action: SemanticAction,
    },
    /// Invoke one published action by address, through the same resolution,
    /// availability, confirmation and strict binding the live host applies.
    Invoke(Invocation),
}

/// Invoke one action the app publishes, by its `(resource, action)` address,
/// through the same door every other caller uses. No coordinates: the
/// scenario names what to do, not where to click.
#[derive(Clone, PartialEq)]
pub struct Invocation {
    pub resource: String,
    pub action: String,
    /// Typed arguments, bound strictly against the action's parameters.
    pub args: serde_json::Map<String, serde_json::Value>,
    /// Explicit confirmation for an action whose contract asks for one.
    pub confirm: bool,
    /// The exact phrase for an action whose contract asks for a typed one.
    pub confirmation_phrase: Option<String>,
    /// Expect the invocation to be refused with this door code
    /// (`unknown_action`, `invalid_arguments`, `action_unavailable`,
    /// `action_confirmation_required`, `action_failed`) instead of running.
    pub expect_error: Option<String>,
}

impl std::fmt::Debug for Invocation {
    /// Arguments can carry secrets (passphrases); a scenario's debug output
    /// names the address and the argument names only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invocation")
            .field("resource", &self.resource)
            .field("action", &self.action)
            .field("args", &self.args.keys().collect::<Vec<_>>())
            .field("confirm", &self.confirm)
            .field("confirmation_phrase", &self.confirmation_phrase.is_some())
            .field("expect_error", &self.expect_error)
            .finish()
    }
}

/// Semantic outcome required at this point in a saved scenario.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemanticExpectation {
    pub target: String,
    pub semantic_target: Option<String>,
    /// Optional bounded auto-wait. Absent expectations are immediate.
    pub timeout_ms: Option<u64>,
    pub role: Option<String>,
    pub name: Option<String>,
    pub name_contains: Option<String>,
    pub value: Option<String>,
    pub value_contains: Option<String>,
    pub focused: Option<bool>,
    pub selected: Option<bool>,
    pub checked: Option<bool>,
    pub expanded: Option<bool>,
    pub enabled: Option<bool>,
    pub busy: Option<bool>,
    pub invalid: Option<bool>,
}

impl SemanticExpectation {
    /// Start an assertion for one retained widget id or inspection path.
    #[must_use]
    pub fn named(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            ..Self::default()
        }
    }

    /// Address a synthetic collection descendant by its stable model identity.
    #[must_use]
    pub fn semantic_target(mut self, target: impl Into<String>) -> Self {
        self.semantic_target = Some(target.into());
        self
    }

    /// Drive live-equivalent frames until this assertion matches or the
    /// explicit bound expires. Without this, assertions are immediate.
    #[must_use]
    pub fn within(mut self, timeout: std::time::Duration) -> Self {
        self.timeout_ms = Some(u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX));
        self
    }

    #[must_use]
    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    #[must_use]
    pub fn name_contains(mut self, fragment: impl Into<String>) -> Self {
        self.name_contains = Some(fragment.into());
        self
    }

    #[must_use]
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    #[must_use]
    pub fn value_contains(mut self, fragment: impl Into<String>) -> Self {
        self.value_contains = Some(fragment.into());
        self
    }

    #[must_use]
    pub fn focused(mut self, expected: bool) -> Self {
        self.focused = Some(expected);
        self
    }

    #[must_use]
    pub fn selected(mut self, expected: bool) -> Self {
        self.selected = Some(expected);
        self
    }

    #[must_use]
    pub fn checked(mut self, expected: bool) -> Self {
        self.checked = Some(expected);
        self
    }

    #[must_use]
    pub fn expanded(mut self, expected: bool) -> Self {
        self.expanded = Some(expected);
        self
    }

    #[must_use]
    pub fn enabled(mut self, expected: bool) -> Self {
        self.enabled = Some(expected);
        self
    }

    #[must_use]
    pub fn busy(mut self, expected: bool) -> Self {
        self.busy = Some(expected);
        self
    }

    #[must_use]
    pub fn invalid(mut self, expected: bool) -> Self {
        self.invalid = Some(expected);
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spec {
    pub viewport: Option<(u32, u32)>,
    pub actions: Vec<Action>,
    pub audit: bool,
    pub audit_report: Option<String>,
    pub dump: Option<String>,
    pub targets: Option<String>,
    pub inspect: Option<String>,
    pub results: Option<String>,
}

/// Parse and strictly validate one complete headless-driver payload.
///
/// # Errors
///
/// Returns a concise path-specific error for malformed JSON or any unknown,
/// missing, or ill-typed field.
pub fn parse_spec(raw: &str) -> Result<Spec, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| format!("invalid JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "payload must be a JSON object".to_owned())?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "viewport"
                | "actions"
                | "audit"
                | "audit_report"
                | "dump"
                | "targets"
                | "inspect"
                | "results"
        ) {
            return Err(format!(
                "unknown field {key:?}; expected viewport, actions, audit, audit_report, dump, targets, inspect, or results"
            ));
        }
    }
    let viewport = object.get("viewport").map(parse_viewport).transpose()?;
    let actions = object
        .get("actions")
        .map(parse_actions_value)
        .transpose()?
        .unwrap_or_default();
    Ok(Spec {
        viewport,
        actions,
        audit: object
            .get("audit")
            .map(|value| value.as_bool().ok_or("audit must be true or false"))
            .transpose()?
            .unwrap_or(false),
        audit_report: optional_string(object, "audit_report")?,
        dump: optional_string(object, "dump")?,
        targets: optional_string(object, "targets")?,
        inspect: optional_string(object, "inspect")?,
        results: optional_string(object, "results")?,
    })
}

/// Parse the standalone action-array format written under `tests/scenarios/`.
///
/// # Errors
///
/// Returns a concise action-specific error for malformed JSON or an operation
/// that does not match the runtime driver's grammar.
pub fn parse_actions(raw: &str) -> Result<Vec<Action>, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| format!("invalid JSON: {error}"))?;
    parse_actions_value(&value)
}

fn optional_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    object
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{key} must be a string"))
        })
        .transpose()
}

fn parse_viewport(value: &serde_json::Value) -> Result<(u32, u32), String> {
    let viewport = value
        .as_array()
        .filter(|viewport| viewport.len() == 2)
        .ok_or("viewport must be [width, height]")?;
    let dimension = |index: usize, name: &str| {
        viewport
            .get(index)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| format!("viewport {name} must be a non-zero u32"))
    };
    Ok((dimension(0, "width")?, dimension(1, "height")?))
}

fn parse_actions_value(value: &serde_json::Value) -> Result<Vec<Action>, String> {
    value
        .as_array()
        .ok_or_else(|| "actions must contain a JSON array".to_owned())?
        .iter()
        .map(parse_action)
        .collect()
}

fn parse_action(value: &serde_json::Value) -> Result<Action, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("action must be an object, got {value}"))?;
    if object.len() != 1 {
        return Err("each action must contain exactly one operation".to_owned());
    }
    let (name, argument) = object
        .iter()
        .next()
        .ok_or_else(|| "action object is empty".to_owned())?;
    match name.as_str() {
        "click_named" => Ok(Action::ClickNamed(required_string(argument, name)?)),
        "click_at" => {
            let [x, y] = point(argument, name)?;
            Ok(Action::ClickAt(x, y))
        }
        "tap_named" => Ok(Action::TapNamed(required_string(argument, name)?)),
        "tap_at" => {
            let [x, y] = point(argument, name)?;
            Ok(Action::TapAt(x, y))
        }
        "swipe_at" => {
            let values = argument
                .as_array()
                .filter(|values| values.len() == 4)
                .ok_or("swipe_at takes [x0, y0, x1, y1]")?;
            let number = |index: usize, coordinate: &str| {
                values
                    .get(index)
                    .and_then(serde_json::Value::as_f64)
                    .ok_or_else(|| format!("swipe_at {coordinate}"))
            };
            Ok(Action::SwipeAt(
                number(0, "x0")?,
                number(1, "y0")?,
                number(2, "x1")?,
                number(3, "y1")?,
            ))
        }
        "key" => Ok(Action::Key(required_string(argument, name)?)),
        "type" => Ok(Action::Type(required_string(argument, name)?)),
        "modifiers" => Ok(Action::Modifiers(string_array(argument, name)?)),
        "wait_for_named" => parse_wait_for_named(argument),
        "expect" => parse_expectation(argument).map(Action::Expect),
        "perform" => parse_semantic_action(argument),
        "invoke" => parse_invocation(argument).map(Action::Invoke),
        other => Err(format!("unknown action {other:?}")),
    }
}

const INVOCATION_CODES: &[&str] = &[
    "unknown_action",
    "invalid_arguments",
    "action_unavailable",
    "action_confirmation_required",
    "action_failed",
];

fn parse_invocation(value: &serde_json::Value) -> Result<Invocation, String> {
    const FIELDS: &[&str] = &[
        "resource",
        "action",
        "args",
        "confirm",
        "confirmation_phrase",
        "expect_error",
    ];
    let object = value
        .as_object()
        .ok_or("invoke takes an object with resource and action")?;
    if let Some(unknown) = object.keys().find(|key| !FIELDS.contains(&key.as_str())) {
        return Err(format!("unknown field {unknown:?} for invoke"));
    }
    let non_empty = |name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("invoke.{name} must be a non-empty string"))
    };
    let args = match object.get("args") {
        None => serde_json::Map::new(),
        Some(serde_json::Value::Object(args)) => args.clone(),
        Some(_) => return Err("invoke.args must be an object".to_owned()),
    };
    let confirm = object
        .get("confirm")
        .map(|value| {
            value
                .as_bool()
                .ok_or("invoke.confirm must be true or false")
        })
        .transpose()?
        .unwrap_or(false);
    let confirmation_phrase = object
        .get("confirmation_phrase")
        .map(|value| {
            value
                .as_str()
                .filter(|phrase| !phrase.is_empty())
                .map(str::to_owned)
                .ok_or("invoke.confirmation_phrase must be a non-empty string")
        })
        .transpose()?;
    let expect_error = object
        .get("expect_error")
        .map(|value| {
            value
                .as_str()
                .filter(|code| INVOCATION_CODES.contains(code))
                .map(str::to_owned)
                .ok_or_else(|| {
                    format!(
                        "invoke.expect_error must be one of {}",
                        INVOCATION_CODES.join(", ")
                    )
                })
        })
        .transpose()?;
    Ok(Invocation {
        resource: non_empty("resource")?,
        action: non_empty("action")?,
        args,
        confirm,
        confirmation_phrase,
        expect_error,
    })
}

fn parse_expectation(value: &serde_json::Value) -> Result<SemanticExpectation, String> {
    const FIELDS: &[&str] = &[
        "target",
        "semantic_target",
        "timeout_ms",
        "role",
        "name",
        "name_contains",
        "value",
        "value_contains",
        "focused",
        "selected",
        "checked",
        "expanded",
        "enabled",
        "busy",
        "invalid",
    ];
    let object = value
        .as_object()
        .ok_or("expect takes an object with a target and semantic conditions")?;
    if let Some(unknown) = object.keys().find(|key| !FIELDS.contains(&key.as_str())) {
        return Err(format!("unknown field {unknown:?} for expect"));
    }
    let target = object
        .get("target")
        .and_then(serde_json::Value::as_str)
        .filter(|target| !target.is_empty())
        .ok_or("expect.target must be a non-empty string")?
        .to_owned();
    let non_empty_string = |name: &str| {
        object
            .get(name)
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| format!("expect.{name} must be a non-empty string"))
            })
            .transpose()
    };
    let optional_string = |name: &str| {
        object
            .get(name)
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("expect.{name} must be a string"))
            })
            .transpose()
    };
    let optional_bool = |name: &str| {
        object
            .get(name)
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| format!("expect.{name} must be true or false"))
            })
            .transpose()
    };
    let role = non_empty_string("role")?;
    if let Some(role) = role.as_deref()
        && !EXPECTED_ROLES.contains(&role)
    {
        return Err(format!(
            "unknown expect.role {role:?}; use a toolkit snake-case semantic role"
        ));
    }
    Ok(SemanticExpectation {
        target,
        semantic_target: non_empty_string("semantic_target")?,
        timeout_ms: optional_timeout_ms(object, "expect")?,
        role,
        name: optional_string("name")?,
        name_contains: non_empty_string("name_contains")?,
        value: optional_string("value")?,
        value_contains: non_empty_string("value_contains")?,
        focused: optional_bool("focused")?,
        selected: optional_bool("selected")?,
        checked: optional_bool("checked")?,
        expanded: optional_bool("expanded")?,
        enabled: optional_bool("enabled")?,
        busy: optional_bool("busy")?,
        invalid: optional_bool("invalid")?,
    })
}

const MAX_TIMEOUT_MS: u64 = 60_000;

fn optional_timeout_ms(
    object: &serde_json::Map<String, serde_json::Value>,
    operation: &str,
) -> Result<Option<u64>, String> {
    object
        .get("timeout_ms")
        .map(|value| {
            value
                .as_u64()
                .filter(|timeout| (1..=MAX_TIMEOUT_MS).contains(timeout))
                .ok_or_else(|| {
                    format!(
                        "{operation}.timeout_ms must be an integer from 1 through {MAX_TIMEOUT_MS}"
                    )
                })
        })
        .transpose()
}

const EXPECTED_ROLES: &[&str] = &[
    "window",
    "dialog",
    "group",
    "generic",
    "toolbar",
    "tab_bar",
    "tab",
    "tab_panel",
    "list",
    "list_item",
    "table",
    "row",
    "cell",
    "tree",
    "tree_item",
    "menu",
    "menu_item",
    "button",
    "toggle_button",
    "checkbox",
    "radio_button",
    "switch",
    "slider",
    "spin_button",
    "combo_box",
    "text_input",
    "multiline_text_input",
    "link",
    "progress_bar",
    "spinner",
    "tooltip",
    "image",
    "label",
    "static_text",
    "heading",
    "paragraph",
    "document",
    "canvas",
    "unknown",
];

fn parse_wait_for_named(value: &serde_json::Value) -> Result<Action, String> {
    let object = value
        .as_object()
        .ok_or("wait_for_named takes an object with target and timeout_ms")?;
    if let Some(unknown) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "target" | "timeout_ms"))
    {
        return Err(format!("unknown field {unknown:?} for wait_for_named"));
    }
    let target = object
        .get("target")
        .and_then(serde_json::Value::as_str)
        .filter(|target| !target.is_empty())
        .ok_or("wait_for_named.target must be a non-empty string")?
        .to_owned();
    let timeout_ms = optional_timeout_ms(object, "wait_for_named")?
        .ok_or("wait_for_named.timeout_ms is required")?;
    Ok(Action::WaitForNamed { target, timeout_ms })
}

fn required_string(value: &serde_json::Value, name: &str) -> Result<String, String> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{name} takes a string"))
}

fn point(value: &serde_json::Value, name: &str) -> Result<[f64; 2], String> {
    let values = value
        .as_array()
        .filter(|values| values.len() == 2)
        .ok_or_else(|| format!("{name} takes [x, y]"))?;
    Ok([
        values
            .first()
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| format!("{name} x"))?,
        values
            .get(1)
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| format!("{name} y"))?,
    ])
}

fn string_array(value: &serde_json::Value, name: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{name} takes a list"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} entries must be strings"))
        })
        .collect()
}

fn parse_semantic_action(value: &serde_json::Value) -> Result<Action, String> {
    let object = value
        .as_object()
        .ok_or("perform takes an object with target and action")?;
    let target = object
        .get("target")
        .and_then(serde_json::Value::as_str)
        .filter(|target| !target.is_empty())
        .ok_or("perform.target must be a non-empty string")?
        .to_owned();
    let semantic_target = object
        .get("semantic_target")
        .map(|value| {
            value
                .as_str()
                .filter(|target| !target.is_empty())
                .map(str::to_owned)
                .ok_or("perform.semantic_target must be a non-empty string")
        })
        .transpose()?;
    let name = object
        .get("action")
        .and_then(serde_json::Value::as_str)
        .ok_or("perform.action must be a string")?;
    let (action, allowed): (SemanticAction, &[&str]) = match name {
        "activate" => (SemanticAction::Activate, BASIC_PERFORM_FIELDS),
        "focus" => (SemanticAction::Focus, BASIC_PERFORM_FIELDS),
        "toggle_check" => (SemanticAction::ToggleCheck, BASIC_PERFORM_FIELDS),
        "increment" => (SemanticAction::Increment, BASIC_PERFORM_FIELDS),
        "decrement" => (SemanticAction::Decrement, BASIC_PERFORM_FIELDS),
        "expand" => (SemanticAction::Expand, BASIC_PERFORM_FIELDS),
        "collapse" => (SemanticAction::Collapse, BASIC_PERFORM_FIELDS),
        "scroll_into_view" => (SemanticAction::ScrollIntoView, BASIC_PERFORM_FIELDS),
        "show_context_menu" => (SemanticAction::ShowContextMenu, BASIC_PERFORM_FIELDS),
        "set_value" => (
            SemanticAction::SetValue(
                object
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("perform set_value requires a string value")?
                    .to_owned(),
            ),
            &["target", "semantic_target", "action", "value"],
        ),
        "set_selection" => (
            SemanticAction::SetSelection {
                anchor: required_index(object, "anchor")?,
                focus: required_index(object, "focus")?,
            },
            &["target", "semantic_target", "action", "anchor", "focus"],
        ),
        other => return Err(format!("unknown semantic action {other:?}")),
    };
    if let Some(unknown) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!(
            "unknown field {unknown:?} for semantic action {name:?}"
        ));
    }
    Ok(Action::Perform {
        target,
        semantic_target,
        action,
    })
}

fn required_index(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<usize, String> {
    object
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("perform set_selection requires a non-negative integer {name}"))
}

const BASIC_PERFORM_FIELDS: &[&str] = &["target", "semantic_target", "action"];

#[cfg(test)]
mod tests {
    use super::*;

    /// Complete payloads and standalone actions share one grammar, invocations address actions and
    /// parse strictly, and malformed or unknown operations are rejected strictly.
    #[test]
    fn the_headless_grammar_is_shared_and_strict() {
        // Phase: complete_payload_and_standalone_actions_share_one_grammar.
        {
            let actions = r#"[{"perform":{"target":"name","action":"set_value","value":"Ada"}},{"wait_for_named":{"target":"result","timeout_ms":750}},{"expect":{"target":"result","role":"static_text","name_contains":"Done","busy":false,"timeout_ms":1000}},{"tap_at":[4,5]}]"#;
            let standalone = parse_actions(actions).unwrap();
            let complete = parse_spec(&format!(
                r#"{{"viewport":[800,600],"actions":{actions},"audit":true,"audit_report":"audit.json","inspect":"tree.json","results":"results.json"}}"#
            ))
            .unwrap();
            assert_eq!(complete.actions, standalone);
            assert_eq!(complete.viewport, Some((800, 600)));
            assert!(complete.audit);
            assert_eq!(complete.results.as_deref(), Some("results.json"));
            assert!(parse_spec(r#"{"results":3}"#).is_err());
            assert_eq!(complete.audit_report.as_deref(), Some("audit.json"));
        }

        // Phase: invocations_address_actions_and_parse_strictly.
        {
            let actions = parse_actions(
                r#"[{"invoke":{"resource":"installer.review","action":"begin","confirmation_phrase":"/dev/vdb"}},
                    {"invoke":{"resource":"installer.account","action":"set-passphrase","args":{"passphrase":"s3cret-words-here","confirmation":"s3cret-words-here"}}},
                    {"invoke":{"resource":"installer","action":"next","expect_error":"action_unavailable"}}]"#,
            )
            .unwrap();
            let Action::Invoke(begin) = &actions[0] else {
                panic!("not an invocation");
            };
            assert_eq!(begin.resource, "installer.review");
            assert_eq!(begin.confirmation_phrase.as_deref(), Some("/dev/vdb"));
            assert!(!begin.confirm);
            let Action::Invoke(secret) = &actions[1] else {
                panic!("not an invocation");
            };
            assert!(
                !format!("{secret:?}").contains("s3cret"),
                "debug output must not print argument values"
            );
            let Action::Invoke(refused) = &actions[2] else {
                panic!("not an invocation");
            };
            assert_eq!(refused.expect_error.as_deref(), Some("action_unavailable"));
            for source in [
                r#"[{"invoke":{"action":"next"}}]"#,
                r#"[{"invoke":{"resource":"r","action":""}}]"#,
                r#"[{"invoke":{"resource":"r","action":"a","args":[]}}]"#,
                r#"[{"invoke":{"resource":"r","action":"a","confirm":"yes"}}]"#,
                r#"[{"invoke":{"resource":"r","action":"a","expect_error":"nope"}}]"#,
                r#"[{"invoke":{"resource":"r","action":"a","extra":1}}]"#,
            ] {
                assert!(parse_actions(source).is_err(), "accepted {source}");
            }
        }

        // Phase: malformed_and_unknown_operations_are_rejected_strictly.
        {
            for source in [
                r#"[{"tap_at":[1]}]"#,
                r#"[{"modifiers":["ctrl",3]}]"#,
                r#"[{"perform":{"target":"x","action":"activate","extra":1}}]"#,
                r#"[{"wait_for_named":{"target":"x","timeout_ms":0}}]"#,
                r#"[{"wait_for_named":{"target":"x","timeout_ms":60001}}]"#,
                r#"[{"wait_for_named":{"target":"","timeout_ms":100}}]"#,
                r#"[{"wait_for_named":{"target":"x","timeout_ms":100,"extra":1}}]"#,
                r#"[{"expect":{"target":"","focused":true}}]"#,
                r#"[{"expect":{"target":"x","focused":"yes"}}]"#,
                r#"[{"expect":{"target":"x","timeout_ms":0}}]"#,
                r#"[{"expect":{"target":"x","timeout_ms":60001}}]"#,
                r#"[{"expect":{"target":"x","timeout_ms":"soon"}}]"#,
                r#"[{"expect":{"target":"x","role":"tree-row"}}]"#,
                r#"[{"expect":{"target":"x","mystery":true}}]"#,
                r#"[{"mystery":"x"}]"#,
            ] {
                assert!(parse_actions(source).is_err(), "accepted {source}");
            }
            assert!(parse_spec(r#"{"actions":{},"dump":3}"#).is_err());
            assert!(parse_spec(r#"{"audit":"yes"}"#).is_err());
            assert!(parse_spec(r#"{"audit_report":3}"#).is_err());
        }
    }
}
