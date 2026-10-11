//! Typed action parameters: what an invocation must carry, and the strict bind
//! that checks it **before** anything runs.
//!
//! An [`ActionSpec`](crate::resource::ActionSpec) used to describe its argument
//! in prose — `"Value 0–1"`, `"Rate, e.g. 0.5 / 1 / 2"`, `"grayscale / sepia /
//! invert"` — and hand the raw string to a closure that parsed it leniently. A
//! typo became a silent default. That is tolerable when a human is looking at a
//! prompt and can see the result; it is not tolerable for a caller with no
//! screen, whose only description of the surface is the enumeration it was
//! handed.
//!
//! So a parameter is declared, not narrated: a [`name`](ActionParam::name), a
//! [`ParamType`], whether it is [`required`](ActionParam::required), optional
//! [`choices`](ActionParam::choices), and a description that explains *meaning*
//! rather than syntax. [`ActionArgs::bind`] then refuses a missing, unknown,
//! wrong-typed or out-of-choice argument up front, so a bad call is a refusal
//! and never a half-applied edit.
//!
//! ## One vocabulary, both sides
//!
//! This is deliberately the *same* vocabulary the compositor's headless action
//! API already speaks (`ParamType` / `ActionParam` / `ArgValue` / strict
//! `ActionArgs::bind`), lifted here so the app-side action model and the
//! shell-side one share a single definition instead of converging by accident.
//! The `const` constructors keep `const PARAMS: &[ActionParam] = &[…]`
//! compiling verbatim; the owned constructors are what an app needs, because an
//! app builds its parameters at runtime out of document state.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Marks the legacy single-string invocation slot as carrying a canonical
/// JSON argument object. The record-separator prefix cannot be produced by a
/// normal palette text field, preserving unambiguous backward compatibility.
pub const ARGUMENTS_JSON_PREFIX: &str = "\u{1e}tilo-args:";

/// A string that is either a `&'static str` (so a parameter list can be a
/// `const`) or owned (so an app can build one per frame from its document).
pub type Str = Cow<'static, str>;

/// The type of one action parameter. Deliberately small: these are values a
/// caller with no screen can produce from a descriptor alone.
///
/// [`Text`](Self::Text), [`Integer`](Self::Integer) and
/// [`Boolean`](Self::Boolean) carry the compositor's existing wire spellings
/// unchanged. [`Number`](Self::Number) is the addition the app surface needs:
/// the lossy parses this module replaces were overwhelmingly fractional
/// (opacity, speed, volume), and rounding them to an integer would throw away
/// exactly the values they exist to carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    Text,
    Integer,
    /// A real number. JSON carries it as a number; an integer literal is
    /// accepted and widened, because `1` and `1.0` are the same rate.
    Number,
    Boolean,
    Object,
    Array,
    /// Stable address of another published Tilo resource.
    Resource,
    /// RFC 3339 timestamp.
    DateTime,
    /// Text that must be redacted from logs, history, and confirmation copy.
    Secret,
}

impl ParamType {
    /// Wire spelling, and what an enumeration descriptor carries.
    pub fn label(self) -> &'static str {
        match self {
            ParamType::Text => "text",
            ParamType::Integer => "integer",
            ParamType::Number => "number",
            ParamType::Boolean => "boolean",
            ParamType::Object => "object",
            ParamType::Array => "array",
            ParamType::Resource => "resource",
            ParamType::DateTime => "datetime",
            ParamType::Secret => "secret",
        }
    }
}

/// Schema annotation naming the kind of resource a [`ParamType::Resource`]
/// parameter takes; see [`ActionParam::of_resource_kind`]. An annotation,
/// not an assertion: binding does not check it.
pub const RESOURCE_KIND_ANNOTATION: &str = "resource_kind";

/// One typed parameter an action declares.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionParam {
    pub name: Str,
    pub ty: ParamType,
    /// Whether an invocation must supply it. An optional parameter's default
    /// belongs to the action and is stated in `description` — the surface does
    /// not invent one.
    pub required: bool,
    pub description: Str,
    /// When non-empty, the only values this parameter accepts.
    ///
    /// This is what turns a prose list (`"grayscale / sepia / invert"`) into
    /// something a caller can *read* and something [`ActionArgs::bind`] can
    /// *enforce*. A value outside the set is refused by name rather than
    /// quietly falling through to a default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Str>,
    /// JSON Schema fragment applied after the basic type check. This carries
    /// ranges, lengths, patterns, formats, item/object schemas, and defaults
    /// without growing a second incompatible validation vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<std::sync::Arc<serde_json::Value>>,
    /// Explicitly permits JSON null. Optional means omitted; nullable means
    /// present with no value, and callers need to distinguish the two.
    #[serde(default)]
    pub nullable: bool,
}

impl ActionParam {
    /// A required parameter with static strings — usable in a `const`.
    pub const fn required(name: &'static str, ty: ParamType, description: &'static str) -> Self {
        Self {
            name: Cow::Borrowed(name),
            ty,
            required: true,
            description: Cow::Borrowed(description),
            choices: Vec::new(),
            schema: None,
            nullable: false,
        }
    }

    /// An optional parameter with static strings — usable in a `const`.
    pub const fn optional(name: &'static str, ty: ParamType, description: &'static str) -> Self {
        Self {
            name: Cow::Borrowed(name),
            ty,
            required: false,
            description: Cow::Borrowed(description),
            choices: Vec::new(),
            schema: None,
            nullable: false,
        }
    }

    /// A required parameter whose strings are built at runtime — what an app
    /// declaring parameters out of live document state needs.
    pub fn required_owned(
        name: impl Into<Str>,
        ty: ParamType,
        description: impl Into<Str>,
    ) -> Self {
        Self {
            name: name.into(),
            ty,
            required: true,
            description: description.into(),
            choices: Vec::new(),
            schema: None,
            nullable: false,
        }
    }

    /// An optional parameter whose strings are built at runtime.
    pub fn optional_owned(
        name: impl Into<Str>,
        ty: ParamType,
        description: impl Into<Str>,
    ) -> Self {
        Self {
            name: name.into(),
            ty,
            required: false,
            description: description.into(),
            choices: Vec::new(),
            schema: None,
            nullable: false,
        }
    }

    /// Restrict this parameter to `choices`. Binding refuses anything else.
    #[must_use]
    pub fn with_choices<S: Into<Str>>(mut self, choices: impl IntoIterator<Item = S>) -> Self {
        self.choices = choices.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_schema(mut self, schema: serde_json::Value) -> Self {
        self.schema = Some(std::sync::Arc::new(schema));
        self
    }

    /// Retain an immutable schema already shared by the current type generation.
    /// Wire serialization and strict argument validation are unchanged.
    #[must_use]
    pub fn with_shared_schema(mut self, schema: std::sync::Arc<serde_json::Value>) -> Self {
        self.schema = Some(schema);
        self
    }

    /// Name the kind of resource this [`ParamType::Resource`] parameter
    /// takes (e.g. `files.entry`), as the [`RESOURCE_KIND_ANNOTATION`] of its
    /// schema. Surfaces use it to offer the action on resources of that kind
    /// and its sub-kinds.
    #[must_use]
    pub fn of_resource_kind(mut self, kind: impl Into<String>) -> Self {
        let mut schema = match self.schema.take() {
            Some(schema) => match std::sync::Arc::unwrap_or_clone(schema) {
                serde_json::Value::Object(schema) => schema,
                _ => serde_json::Map::new(),
            },
            _ => serde_json::Map::new(),
        };
        schema.insert(
            RESOURCE_KIND_ANNOTATION.into(),
            serde_json::Value::String(kind.into()),
        );
        self.schema = Some(std::sync::Arc::new(serde_json::Value::Object(schema)));
        self
    }

    /// The kind of resource this parameter takes, when it is a resource
    /// parameter that names one ([`Self::of_resource_kind`]).
    #[must_use]
    pub fn resource_kind(&self) -> Option<&str> {
        if self.ty != ParamType::Resource {
            return None;
        }
        self.schema
            .as_ref()?
            .get(RESOURCE_KIND_ANNOTATION)?
            .as_str()
            .filter(|kind| !kind.is_empty())
    }

    #[must_use]
    pub fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }

    /// Validate this parameter's descriptor independently of any supplied
    /// value, suitable for an untrusted publication boundary.
    pub fn validate_declaration(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.name.trim().is_empty() {
            errors.push("parameter name must be non-empty".into());
        }
        if self.description.trim().is_empty() {
            errors.push("parameter description must be non-empty".into());
        }
        if self.ty == ParamType::Secret && !self.choices.is_empty() {
            errors.push("secret parameters must not publish possible values".into());
        }
        for choice in &self.choices {
            let valid = match self.ty {
                ParamType::Text => true,
                ParamType::Integer => choice.parse::<i64>().is_ok(),
                ParamType::Number => choice.parse::<f64>().is_ok(),
                ParamType::Boolean => matches!(choice.as_ref(), "true" | "false"),
                ParamType::Object => {
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(choice)
                        .is_ok()
                }
                ParamType::Array => serde_json::from_str::<Vec<serde_json::Value>>(choice).is_ok(),
                ParamType::Resource => {
                    !choice.trim().is_empty()
                        && !choice.contains(crate::KEY_SEP)
                        && choice.contains(':')
                }
                ParamType::DateTime => looks_like_rfc3339(choice),
                ParamType::Secret => false,
            };
            if !valid {
                errors.push(format!(
                    "choice `{choice}` does not have declared type {}",
                    self.ty.label()
                ));
            }
        }
        if let Some(schema) = &self.schema
            && let Err(reason) = validate_shared_schema_definition(schema)
        {
            errors.push(format!("invalid schema: {reason}"));
        }
        errors
    }
}

const VALID_SCHEMA_LIMIT: usize = 64;

thread_local! {
    static VALID_SCHEMAS: std::cell::RefCell<Vec<std::sync::Weak<serde_json::Value>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Publications reuse immutable type-generation schemas. Remember only successful
/// structural checks, never argument values, descriptor fields or authority.
/// Weak identities keep neither the schema tree nor its generation alive and
/// prevent an allocation at a recycled address from borrowing an old result.
/// `Arc::make_mut` dissociates these weak identities before changing a schema.
fn validate_shared_schema_definition(
    schema: &std::sync::Arc<serde_json::Value>,
) -> Result<(), String> {
    let known = VALID_SCHEMAS.with(|schemas| {
        schemas
            .borrow()
            .iter()
            .any(|validated| std::ptr::eq(validated.as_ptr(), std::sync::Arc::as_ptr(schema)))
    });
    if known {
        return Ok(());
    }
    validate_schema_definition(schema)?;
    VALID_SCHEMAS.with(|schemas| {
        let mut schemas = schemas.borrow_mut();
        schemas.retain(|schema| schema.strong_count() != 0);
        if schemas.len() == VALID_SCHEMA_LIMIT {
            schemas.remove(0);
        }
        schemas.push(std::sync::Arc::downgrade(schema));
    });
    Ok(())
}

/// One bound argument value.
#[derive(Clone, PartialEq)]
pub enum ArgValue {
    Text(String),
    Integer(i64),
    Number(f64),
    Boolean(bool),
    Object(serde_json::Map<String, serde_json::Value>),
    Array(Vec<serde_json::Value>),
    Resource(String),
    DateTime(String),
    Secret(String),
    Null,
}

impl std::fmt::Debug for ArgValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(value) => f.debug_tuple("Text").field(value).finish(),
            Self::Integer(value) => f.debug_tuple("Integer").field(value).finish(),
            Self::Number(value) => f.debug_tuple("Number").field(value).finish(),
            Self::Boolean(value) => f.debug_tuple("Boolean").field(value).finish(),
            Self::Object(value) => f.debug_tuple("Object").field(value).finish(),
            Self::Array(value) => f.debug_tuple("Array").field(value).finish(),
            Self::Resource(value) => f.debug_tuple("Resource").field(value).finish(),
            Self::DateTime(value) => f.debug_tuple("DateTime").field(value).finish(),
            Self::Secret(_) => f.debug_tuple("Secret").field(&"[REDACTED]").finish(),
            Self::Null => f.write_str("Null"),
        }
    }
}

impl ArgValue {
    /// The declared type this value satisfies.
    pub fn ty(&self) -> ParamType {
        match self {
            ArgValue::Text(_) | ArgValue::Null => ParamType::Text,
            ArgValue::Integer(_) => ParamType::Integer,
            ArgValue::Number(_) => ParamType::Number,
            ArgValue::Boolean(_) => ParamType::Boolean,
            ArgValue::Object(_) => ParamType::Object,
            ArgValue::Array(_) => ParamType::Array,
            ArgValue::Resource(_) => ParamType::Resource,
            ArgValue::DateTime(_) => ParamType::DateTime,
            ArgValue::Secret(_) => ParamType::Secret,
            // Null is validated against `ActionParam::nullable`, so its
            // nominal type is irrelevant. Text keeps diagnostics stable.
        }
    }

    /// Whether this value satisfies `want`.
    ///
    /// Exact, with one deliberate widening: an [`Integer`](Self::Integer)
    /// satisfies [`Number`](ParamType::Number). JSON has one numeric type and
    /// spells `1.0` as `1`, so refusing it would make a perfectly good rate of
    /// `1` un-passable over the wire. The widening is one-way — a fractional
    /// value never satisfies `Integer`.
    pub fn satisfies(&self, want: ParamType) -> bool {
        self.ty() == want || (want == ParamType::Number && self.ty() == ParamType::Integer)
    }

    /// Read as JSON, using the same spelling the compositor's control socket
    /// already accepts: string → `Text`, bool → `Boolean`, integral number →
    /// `Integer`, fractional number → `Number`.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::String(s) => Some(ArgValue::Text(s.clone())),
            serde_json::Value::Bool(b) => Some(ArgValue::Boolean(*b)),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Some(ArgValue::Integer(i)),
                None => n.as_f64().map(ArgValue::Number),
            },
            serde_json::Value::Object(value) => Some(ArgValue::Object(value.clone())),
            serde_json::Value::Array(value) => Some(ArgValue::Array(value.clone())),
            serde_json::Value::Null => Some(ArgValue::Null),
        }
    }

    /// Write as JSON, the inverse of [`from_json`](Self::from_json).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            ArgValue::Text(s) => serde_json::Value::String(s.clone()),
            ArgValue::Integer(i) => serde_json::Value::from(*i),
            ArgValue::Number(f) => serde_json::Number::from_f64(*f)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            ArgValue::Boolean(b) => serde_json::Value::Bool(*b),
            ArgValue::Object(value) => serde_json::Value::Object(value.clone()),
            ArgValue::Array(value) => serde_json::Value::Array(value.clone()),
            ArgValue::Resource(value) | ArgValue::DateTime(value) | ArgValue::Secret(value) => {
                serde_json::Value::String(value.clone())
            }
            ArgValue::Null => serde_json::Value::Null,
        }
    }
}

/// Why a set of arguments does not satisfy an action's parameters.
///
/// Each variant names the parameter, because a caller that cannot see the
/// screen has nothing else to correct against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgError {
    Missing {
        name: String,
    },
    Unknown {
        name: String,
    },
    WrongType {
        name: String,
        expected: ParamType,
        got: ParamType,
    },
    /// The value was the right type but outside the parameter's declared
    /// [`choices`](ActionParam::choices).
    NotAllowed {
        name: String,
        value: String,
        allowed: Vec<String>,
    },
    /// The JSON value was not one of the shapes an [`ArgValue`] can carry (an
    /// object, an array, or `null`).
    Unrepresentable {
        name: String,
    },
    Constraint {
        name: String,
        reason: String,
    },
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::Missing { name } => {
                write!(f, "required argument `{name}` was not supplied")
            }
            ArgError::Unknown { name } => {
                write!(f, "`{name}` is not a parameter of this action")
            }
            ArgError::WrongType {
                name,
                expected,
                got,
            } => write!(
                f,
                "argument `{name}` must be {}, got {}",
                expected.label(),
                got.label()
            ),
            ArgError::NotAllowed {
                name,
                value,
                allowed,
            } => write!(
                f,
                "argument `{name}` must be one of {}, got `{value}`",
                allowed.join(", ")
            ),
            ArgError::Unrepresentable { name } => {
                write!(f, "argument `{name}` has an unsupported JSON value")
            }
            ArgError::Constraint { name, reason } => {
                write!(f, "argument `{name}` {reason}")
            }
        }
    }
}

impl std::error::Error for ArgError {}

/// Arguments for one invocation, already checked against the action's declared
/// parameters.
///
/// There is no way to build one except through [`bind`](Self::bind) or
/// [`none`](Self::none), so an action's builder never has to re-validate: if it
/// holds an `ActionArgs`, every required parameter is present and every value
/// has the declared type. That is what keeps a bad call from becoming a
/// half-applied edit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionArgs {
    values: BTreeMap<String, ArgValue>,
}

impl ActionArgs {
    /// The empty argument set — what a nullary action is invoked with.
    pub fn none() -> Self {
        Self::default()
    }

    /// Check `supplied` against `params` and bind it.
    ///
    /// Fails on the first problem rather than accumulating: the caller has to
    /// fix them one at a time anyway, and a partial bind must never reach an
    /// action.
    ///
    /// # Errors
    ///
    /// Returns the first missing, unknown, wrong-typed, or disallowed argument.
    pub fn bind(
        params: &[ActionParam],
        supplied: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<Self, ArgError> {
        let mut values: BTreeMap<String, ArgValue> = BTreeMap::new();
        for (name, value) in supplied {
            let Some(param) = params.iter().find(|p| p.name.as_ref() == name.as_str()) else {
                // An unknown argument is refused rather than dropped. A silently
                // ignored argument is the failure mode this whole surface exists
                // to remove: the caller believes it asked for something it did
                // not get.
                return Err(ArgError::Unknown { name });
            };
            if matches!(value, ArgValue::Null) && !param.nullable {
                return Err(ArgError::Constraint {
                    name,
                    reason: "may not be null".into(),
                });
            }
            if !matches!(value, ArgValue::Null) && !value.satisfies(param.ty) {
                return Err(ArgError::WrongType {
                    name,
                    expected: param.ty,
                    got: value.ty(),
                });
            }
            match &value {
                ArgValue::Resource(address)
                    if address.trim().is_empty()
                        || address.contains(crate::KEY_SEP)
                        || !address.contains(':') =>
                {
                    return Err(ArgError::Constraint {
                        name,
                        reason: "must be a stable resource address (`kind:id`)".into(),
                    });
                }
                ArgValue::DateTime(value) if !looks_like_rfc3339(value) => {
                    return Err(ArgError::Constraint {
                        name,
                        reason: "must be an RFC 3339 date-time with a timezone".into(),
                    });
                }
                _ => {}
            }
            if let Some(schema) = &param.schema {
                validate_schema(&value.to_json(), schema).map_err(|reason| {
                    ArgError::Constraint {
                        name: name.clone(),
                        reason,
                    }
                })?;
            }
            if !param.choices.is_empty() {
                let got = match &value {
                    ArgValue::Text(s)
                    | ArgValue::Resource(s)
                    | ArgValue::DateTime(s)
                    | ArgValue::Secret(s) => s.clone(),
                    other => other.to_json().to_string(),
                };
                if !param.choices.iter().any(|c| c.as_ref() == got.as_str()) {
                    return Err(ArgError::NotAllowed {
                        name,
                        value: got,
                        allowed: param
                            .choices
                            .iter()
                            .map(std::string::ToString::to_string)
                            .collect(),
                    });
                }
            }
            values.insert(name, value);
        }
        for param in params.iter().filter(|p| p.required) {
            if !values.contains_key(param.name.as_ref()) {
                return Err(ArgError::Missing {
                    name: param.name.to_string(),
                });
            }
        }
        Ok(Self { values })
    }

    /// Bind arguments that arrived as a JSON object — the shape a control
    /// socket delivers.
    ///
    /// # Errors
    ///
    /// Returns [`ArgError`] when a JSON value is unrepresentable or the
    /// supplied arguments do not satisfy `params`.
    pub fn bind_json(
        params: &[ActionParam],
        supplied: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, ArgError> {
        let mut pairs = Vec::with_capacity(supplied.len());
        for (name, value) in supplied {
            let Some(param) = params.iter().find(|param| param.name.as_ref() == name) else {
                return Err(ArgError::Unknown { name: name.clone() });
            };
            let value = match (param.ty, value) {
                (ParamType::Resource, serde_json::Value::String(value)) => {
                    Some(ArgValue::Resource(value.clone()))
                }
                (ParamType::DateTime, serde_json::Value::String(value)) => {
                    Some(ArgValue::DateTime(value.clone()))
                }
                (ParamType::Secret, serde_json::Value::String(value)) => {
                    Some(ArgValue::Secret(value.clone()))
                }
                _ => ArgValue::from_json(value),
            };
            let Some(v) = value else {
                return Err(ArgError::Unrepresentable { name: name.clone() });
            };
            pairs.push((name.clone(), v));
        }
        Self::bind(params, pairs)
    }

    /// The text bound to `name`, if any.
    pub fn text(&self, name: &str) -> Option<&str> {
        match self.values.get(name) {
            Some(
                ArgValue::Text(s)
                | ArgValue::Resource(s)
                | ArgValue::DateTime(s)
                | ArgValue::Secret(s),
            ) => Some(s.as_str()),
            _ => None,
        }
    }

    /// The integer bound to `name`, if any.
    pub fn integer(&self, name: &str) -> Option<i64> {
        match self.values.get(name) {
            Some(ArgValue::Integer(i)) => Some(*i),
            _ => None,
        }
    }

    /// The JSON object bound to `name`, if any.
    pub fn object(&self, name: &str) -> Option<&serde_json::Map<String, serde_json::Value>> {
        match self.values.get(name) {
            Some(ArgValue::Object(object)) => Some(object),
            _ => None,
        }
    }

    /// The JSON array bound to `name`, if any.
    pub fn array(&self, name: &str) -> Option<&[serde_json::Value]> {
        match self.values.get(name) {
            Some(ArgValue::Array(values)) => Some(values),
            _ => None,
        }
    }

    /// The number bound to `name`, if any. An integer argument widens, matching
    /// [`ArgValue::satisfies`].
    #[allow(
        clippy::cast_precision_loss,
        reason = "Number parameters deliberately widen integer JSON values to f64"
    )]
    pub fn number(&self, name: &str) -> Option<f64> {
        match self.values.get(name) {
            Some(ArgValue::Number(f)) => Some(*f),
            Some(ArgValue::Integer(i)) => Some(*i as f64),
            _ => None,
        }
    }

    /// The boolean bound to `name`, if any.
    pub fn boolean(&self, name: &str) -> Option<bool> {
        match self.values.get(name) {
            Some(ArgValue::Boolean(b)) => Some(*b),
            _ => None,
        }
    }

    /// The raw bound value, for a caller that wants to switch on the type.
    pub fn get(&self, name: &str) -> Option<&ArgValue> {
        self.values.get(name)
    }

    /// Whether anything is bound.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Every bound argument as JSON — the inverse of
    /// [`bind_json`](Self::bind_json), so a caller can echo back exactly what it
    /// sent.
    pub fn to_json(&self) -> serde_json::Map<String, serde_json::Value> {
        self.values
            .iter()
            .map(|(k, v)| (k.clone(), v.to_json()))
            .collect()
    }
}

fn looks_like_rfc3339(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

/// Distinct schema patterns kept compiled. A directory publishes a handful;
/// past this the cache starts again rather than growing with whatever a
/// caller's schemas name.
const COMPILED_PATTERNS: usize = 64;

/// `pattern` compiled, from the cache when an earlier argument or schema
/// already compiled it: every validated value used to compile its schema's
/// pattern again.
fn schema_pattern(pattern: &str) -> Result<regex::Regex, regex::Error> {
    static COMPILED: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<String, regex::Regex>>,
    > = std::sync::LazyLock::new(std::sync::Mutex::default);
    let mut compiled = COMPILED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(expression) = compiled.get(pattern) {
        return Ok(expression.clone());
    }
    let expression = regex::Regex::new(pattern)?;
    if compiled.len() >= COMPILED_PATTERNS {
        compiled.clear();
    }
    compiled.insert(pattern.to_owned(), expression.clone());
    drop(compiled);
    Ok(expression)
}

fn validate_string_schema(
    text: &str,
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let length = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
    if let Some(minimum) = schema.get("minLength").and_then(serde_json::Value::as_u64)
        && length < minimum
    {
        return Err(format!("must contain at least {minimum} characters"));
    }
    if let Some(maximum) = schema.get("maxLength").and_then(serde_json::Value::as_u64)
        && length > maximum
    {
        return Err(format!("must contain at most {maximum} characters"));
    }
    if let Some(pattern) = schema.get("pattern") {
        let pattern = pattern
            .as_str()
            .ok_or_else(|| "has a non-string schema pattern".to_string())?;
        let expression = schema_pattern(pattern)
            .map_err(|error| format!("has an invalid schema pattern: {error}"))?;
        if !expression.is_match(text) {
            return Err(format!("must match pattern `{pattern}`"));
        }
    }
    if let Some(format) = schema.get("format") {
        let format = format
            .as_str()
            .ok_or_else(|| "has a non-string schema format".to_string())?;
        let valid = match format {
            "date-time" => looks_like_rfc3339(text),
            "uuid" => uuid::Uuid::parse_str(text).is_ok(),
            "uri" => text.split_once(':').is_some_and(|(scheme, rest)| {
                !scheme.is_empty()
                    && !rest.is_empty()
                    && scheme
                        .chars()
                        .next()
                        .is_some_and(|ch| ch.is_ascii_alphabetic())
                    && scheme
                        .chars()
                        .skip(1)
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
                    && !text.chars().any(char::is_whitespace)
            }),
            _ => return Err(format!("uses unsupported schema format `{format}`")),
        };
        if !valid {
            return Err(format!("must match format `{format}`"));
        }
    }
    Ok(())
}

fn validate_object_schema(
    object: &serde_json::Map<String, serde_json::Value>,
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
        for key in required.iter().filter_map(serde_json::Value::as_str) {
            if !object.contains_key(key) {
                return Err(format!("must contain property `{key}`"));
            }
        }
    }
    if let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    {
        for (key, property_schema) in properties {
            if let Some(property) = object.get(key) {
                validate_schema(property, property_schema)
                    .map_err(|reason| format!("property `{key}` {reason}"))?;
            }
        }
        if schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false)) {
            for key in object.keys() {
                if !properties.contains_key(key) {
                    return Err(format!("contains unknown property `{key}`"));
                }
            }
        }
    }
    Ok(())
}

/// Validate the commonly useful, deterministic JSON Schema vocabulary used by
/// action inputs and outputs. Unknown annotations are intentionally ignored;
/// all assertion keywords accepted here are enforced centrally.
pub(crate) fn validate_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
) -> Result<(), String> {
    let Some(schema) = schema.as_object() else {
        return Err("has a schema that is not an object".into());
    };
    if let Some(alternatives) = schema.get("oneOf").and_then(serde_json::Value::as_array) {
        validate_one_of(value, alternatives)?;
    }
    validate_schema_type(value, schema)?;
    validate_schema_equality(value, schema)?;
    validate_schema_number(value, schema)?;
    validate_schema_string(value, schema)?;
    validate_schema_array(value, schema)?;
    validate_schema_object(value, schema)
}

/// Exactly one alternative must accept `value`. When none does, the refusal
/// says why in terms of the alternative the value evidently meant — the one
/// whose required keys and constants it has (`{"SpliceText": {..}}` meant
/// the `SpliceText` alternative) — or else names the alternatives, so a
/// caller can correct the call rather than guess.
fn validate_one_of(
    value: &serde_json::Value,
    alternatives: &[serde_json::Value],
) -> Result<(), String> {
    let mut failures = Vec::new();
    let mut matched = 0_usize;
    for alternative in alternatives {
        match validate_schema(value, alternative) {
            Ok(()) => matched = matched.saturating_add(1),
            Err(reason) => failures.push((alternative, reason)),
        }
    }
    match matched {
        1 => return Ok(()),
        0 => {}
        _ => return Err("matches more than one `oneOf` alternative".into()),
    }
    let meant: Vec<_> = failures
        .iter()
        .filter(|(alternative, _)| selects(value, alternative))
        .collect();
    if let [(alternative, reason)] = meant.as_slice() {
        return Err(match alternative_name(alternative) {
            Some(name) => format!("as `{name}`: {reason}"),
            None => reason.clone(),
        });
    }
    let names: Option<Vec<String>> = alternatives.iter().map(alternative_name).collect();
    Err(match names {
        Some(names) if !names.is_empty() => format!("must be one of: {}", names.join(", ")),
        _ => "must match exactly one `oneOf` alternative".into(),
    })
}

/// Whether `value` carries what tells `alternative` apart: the values of its
/// constant or enumerated properties (a tag, or one of several), or else all
/// of its required keys.
fn selects(value: &serde_json::Value, alternative: &serde_json::Value) -> bool {
    let Some(schema) = alternative.as_object() else {
        return false;
    };
    if let Some(expected) = schema.get("const") {
        return value == expected;
    }
    let Some(object) = value.as_object() else {
        return false;
    };
    let tags: Vec<(&String, &serde_json::Value)> = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, property)| property.get("const").is_some() || property.get("enum").is_some())
        .collect();
    if !tags.is_empty() {
        return tags.iter().all(|(key, property)| {
            object.get(key.as_str()).is_some_and(|given| {
                property.get("const") == Some(given)
                    || property
                        .get("enum")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|names| names.contains(given))
            })
        });
    }
    let required = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    !required.is_empty()
        && required
            .iter()
            .filter_map(serde_json::Value::as_str)
            .all(|key| object.contains_key(key))
}

/// What an alternative is called: its title, its constant, the constant of
/// its first required property, or its one required key.
fn alternative_name(alternative: &serde_json::Value) -> Option<String> {
    let schema = alternative.as_object()?;
    if let Some(title) = schema.get("title").and_then(serde_json::Value::as_str) {
        return Some(title.to_owned());
    }
    if let Some(constant) = schema.get("const") {
        return Some(
            constant
                .as_str()
                .map_or_else(|| constant.to_string(), str::to_owned),
        );
    }
    let required = schema.get("required")?.as_array()?;
    let first = required.first()?.as_str()?;
    if let Some(constant) = schema
        .get("properties")
        .and_then(|properties| properties.get(first))
        .and_then(|property| property.get("const"))
        .and_then(serde_json::Value::as_str)
    {
        return Some(constant.to_owned());
    }
    (required.len() == 1).then(|| first.to_owned())
}

fn validate_schema_type(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(expected) = schema.get("type") {
        let matches_type = |name: &str| match name {
            "null" => value.is_null(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "number" => value.is_number(),
            "integer" => value
                .as_number()
                .is_some_and(|number| number.as_i64().is_some() || number.as_u64().is_some()),
            "string" => value.is_string(),
            _ => false,
        };
        let valid = expected.as_str().is_some_and(matches_type)
            || expected.as_array().is_some_and(|types| {
                !types.is_empty()
                    && types.iter().all(serde_json::Value::is_string)
                    && types
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .any(matches_type)
            });
        if !valid {
            return Err(format!("must have JSON Schema type {expected}"));
        }
    }
    Ok(())
}

fn validate_schema_equality(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array)
        && !values.contains(value)
    {
        return Err("is not one of the allowed values".into());
    }
    if let Some(expected) = schema.get("const")
        && value != expected
    {
        return Err("does not equal the required value".into());
    }
    Ok(())
}

fn validate_schema_number(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(number) = value.as_f64() {
        if let Some(minimum) = schema.get("minimum").and_then(serde_json::Value::as_f64)
            && number < minimum
        {
            return Err(format!("must be at least {minimum}"));
        }
        if let Some(maximum) = schema.get("maximum").and_then(serde_json::Value::as_f64)
            && number > maximum
        {
            return Err(format!("must be at most {maximum}"));
        }
    }
    Ok(())
}

fn validate_schema_string(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(text) = value.as_str() {
        validate_string_schema(text, schema)?;
    }
    Ok(())
}

fn validate_schema_array(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(items) = value.as_array() {
        let length = u64::try_from(items.len()).unwrap_or(u64::MAX);
        if let Some(minimum) = schema.get("minItems").and_then(serde_json::Value::as_u64)
            && length < minimum
        {
            return Err(format!("must contain at least {minimum} items"));
        }
        if let Some(maximum) = schema.get("maxItems").and_then(serde_json::Value::as_u64)
            && length > maximum
        {
            return Err(format!("must contain at most {maximum} items"));
        }
        if schema
            .get("uniqueItems")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            && items
                .iter()
                .enumerate()
                .any(|(index, item)| items.get(..index).is_some_and(|prior| prior.contains(item)))
        {
            return Err("must contain unique items".into());
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                validate_schema(item, item_schema)
                    .map_err(|reason| format!("item {index} {reason}"))?;
            }
        }
    }
    Ok(())
}

fn validate_schema_object(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(object) = value.as_object() {
        validate_object_schema(object, schema)?;
    }
    Ok(())
}

/// Validate one JSON value against the deterministic JSON Schema subset used
/// by Tilo action contracts.
///
/// # Errors
///
/// Returns a human-readable contract violation when `schema` is not an object
/// or `value` fails one of its supported assertions.
pub fn validate_json_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
) -> Result<(), String> {
    validate_schema(value, schema)
}

const SCHEMA_ASSERTIONS: &[&str] = &[
    "type",
    "enum",
    "const",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minItems",
    "maxItems",
    "uniqueItems",
    "items",
    "required",
    "properties",
    "additionalProperties",
    "oneOf",
];
const SCHEMA_ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
];

/// Validate the assertion vocabulary itself before a surface is published.
/// Runtime value validation is too late to discover a malformed regular
/// expression or an unsupported format: callers must be able to trust every
/// schema they enumerate.
pub(crate) fn validate_schema_definition(schema: &serde_json::Value) -> Result<(), String> {
    let object = schema
        .as_object()
        .ok_or_else(|| "schema must be an object".to_string())?;
    for keyword in object.keys() {
        if !SCHEMA_ASSERTIONS.contains(&keyword.as_str())
            && !SCHEMA_ANNOTATIONS.contains(&keyword.as_str())
        {
            return Err(format!(
                "schema uses unsupported keyword `{keyword}`; Tilo rejects unknown keywords instead of silently weakening a contract"
            ));
        }
    }
    validate_schema_definition_type(object)?;
    validate_schema_definition_strings(object)?;
    validate_schema_definition_collections(object)?;
    validate_schema_definition_numbers(object)
}

fn validate_schema_definition_type(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(types) = object.get("type") {
        let supported = |name: &str| {
            matches!(
                name,
                "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
            )
        };
        let valid = types.as_str().is_some_and(supported)
            || types.as_array().is_some_and(|types| {
                !types.is_empty()
                    && types
                        .iter()
                        .all(|value| value.as_str().is_some_and(supported))
            });
        if !valid {
            return Err("schema has an unsupported `type`".into());
        }
    }
    Ok(())
}

fn validate_schema_definition_strings(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(pattern) = object.get("pattern") {
        let pattern = pattern
            .as_str()
            .ok_or_else(|| "schema `pattern` must be a string".to_string())?;
        schema_pattern(pattern)
            .map_err(|error| format!("schema has an invalid pattern: {error}"))?;
    }
    if let Some(format) = object.get("format") {
        let format = format
            .as_str()
            .ok_or_else(|| "schema `format` must be a string".to_string())?;
        if !matches!(format, "date-time" | "uuid" | "uri") {
            return Err(format!("schema uses unsupported format `{format}`"));
        }
    }
    Ok(())
}

fn validate_schema_definition_collections(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if let Some(required) = object.get("required")
        && !required
            .as_array()
            .is_some_and(|values| values.iter().all(serde_json::Value::is_string))
    {
        return Err("schema `required` must be an array of strings".into());
    }
    if object.get("enum").is_some_and(|values| !values.is_array()) {
        return Err("schema `enum` must be an array".into());
    }
    if object
        .get("additionalProperties")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("schema `additionalProperties` must be a boolean".into());
    }
    if object
        .get("uniqueItems")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("schema `uniqueItems` must be a boolean".into());
    }
    if let Some(properties) = object.get("properties") {
        let properties = properties
            .as_object()
            .ok_or_else(|| "schema `properties` must be an object".to_string())?;
        for (name, child) in properties {
            validate_schema_definition(child)
                .map_err(|reason| format!("property `{name}` {reason}"))?;
        }
    }
    if let Some(items) = object.get("items") {
        validate_schema_definition(items).map_err(|reason| format!("items {reason}"))?;
    }
    if let Some(alternatives) = object.get("oneOf") {
        let alternatives = alternatives
            .as_array()
            .filter(|alternatives| !alternatives.is_empty())
            .ok_or_else(|| "schema `oneOf` must be a non-empty array".to_string())?;
        for (index, alternative) in alternatives.iter().enumerate() {
            validate_schema_definition(alternative)
                .map_err(|reason| format!("oneOf alternative {index} {reason}"))?;
        }
    }
    Ok(())
}

fn validate_schema_definition_numbers(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    for keyword in ["minimum", "maximum"] {
        if object.get(keyword).is_some_and(|value| !value.is_number()) {
            return Err(format!("schema `{keyword}` must be a number"));
        }
    }
    for keyword in ["minLength", "maxLength", "minItems", "maxItems"] {
        if object
            .get(keyword)
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err(format!("schema `{keyword}` must be a non-negative integer"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod declaration_reuse_tests {
    use std::sync::Arc;
    use std::time::Instant;

    use serde_json::json;

    use super::{ActionArgs, ActionParam, ParamType};

    #[test]
    fn shared_schema_edits_and_descriptor_changes_are_still_refused() {
        let original = ActionParam::required("body", ParamType::Text, "Body")
            .with_schema(json!({"type": "string", "minLength": 3}));
        assert!(original.validate_declaration().is_empty());
        assert!(
            ActionArgs::bind_json(
                std::slice::from_ref(&original),
                json!({"body": "a"}).as_object().unwrap()
            )
            .is_err()
        );
        let mut edited = original.clone();
        assert!(edited.validate_declaration().is_empty());
        Arc::make_mut(edited.schema.as_mut().unwrap())["minLength"] = json!(-1);
        assert!(edited.validate_declaration()[0].contains("non-negative integer"));
        assert!(original.validate_declaration().is_empty());
        let mut renamed = original.clone();
        renamed.name = " ".into();
        assert_eq!(
            renamed.validate_declaration(),
            ["parameter name must be non-empty"]
        );
        let mut secret = original;
        secret.ty = ParamType::Secret;
        secret.choices.push("private".into());
        assert!(secret.validate_declaration()[0].contains("must not publish possible values"));
    }

    #[test]
    fn last_owner_schema_mutation_dissociates_prior_validation() {
        let mut parameter = ActionParam::required("body", ParamType::Text, "Body")
            .with_schema(json!({"type": "string"}));
        assert!(parameter.validate_declaration().is_empty());
        Arc::make_mut(parameter.schema.as_mut().unwrap())["type"] = json!("unsupported");
        assert_eq!(
            parameter.validate_declaration(),
            ["invalid schema: schema has an unsupported `type`"]
        );
        Arc::make_mut(parameter.schema.as_mut().unwrap())["type"] = json!("string");
        assert!(parameter.validate_declaration().is_empty());
    }

    #[test]
    fn retired_generations_are_released_and_new_invalid_schemas_are_refused() {
        for _ in 0..128 {
            let schema = Arc::new(json!({"type": "string"}));
            let retired = Arc::downgrade(&schema);
            let parameter =
                ActionParam::required("body", ParamType::Text, "Body").with_shared_schema(schema);
            assert!(parameter.validate_declaration().is_empty());
            drop(parameter);
            assert!(retired.upgrade().is_none());
            let invalid = ActionParam::required("body", ParamType::Text, "Body")
                .with_schema(json!({"pattern": "["}));
            assert!(invalid.validate_declaration()[0].contains("invalid pattern"));
        }
    }

    #[test]
    fn shared_declaration_observes_recursive_publication_work() {
        let properties: serde_json::Map<String, serde_json::Value> = (0..128)
            .map(|index| {
                (
                    format!("field{index}"),
                    json!({"type": "array", "items": {"type": "object", "properties": {
                        "text": {"type": "string", "minLength": 1},
                        "count": {"type": "integer", "minimum": 0}
                    }, "additionalProperties": false}}),
                )
            })
            .collect();
        let parameter = ActionParam::required("request", ParamType::Object, "Request")
            .with_shared_schema(Arc::new(
                json!({"type": "object", "properties": properties}),
            ));
        let start = Instant::now();
        for _ in 0..1000 {
            assert!(
                std::hint::black_box(&parameter)
                    .validate_declaration()
                    .is_empty()
            );
        }
        eprintln!(
            "ACTION_SCHEMA_DECLARATION_OBSERVATION iterations=1000 fields=128 elapsed_ns={} includes_gpu_or_display=false",
            start.elapsed().as_nanos()
        );
    }
}

#[cfg(test)]
mod rich_schema_tests {
    use super::*;
    use serde_json::json;

    /// Structured arguments are typed and recursively validated; resource, datetime, secret and
    /// nullable values round-trip; a malformed schema is rejected before it describes values; unique
    /// array items are published and enforced.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn action_parameters_are_typed_validated_and_round_trip() {
        structured_arguments_are_typed_and_recursively_validated();
        resource_datetime_secret_and_nullable_values_round_trip();
        malformed_schema_is_rejected_before_it_describes_values();
        unique_array_items_are_published_and_enforced();
    }

    #[test]
    fn shared_schema_preserves_wire_binding_and_copy_on_write() {
        use crate::{ActionSpec, EncodedAction, ExecutionLocus};
        use std::sync::Arc;

        let schema = serde_json::json!({"type": "integer", "minimum": 1});
        let original =
            ActionParam::required("count", ParamType::Integer, "Count").with_schema(schema.clone());
        let clone = original.clone();
        assert!(Arc::ptr_eq(
            original.schema.as_ref().unwrap(),
            clone.schema.as_ref().unwrap()
        ));
        let expected = serde_json::json!({
            "name":"count", "ty":"integer", "required":true,
            "description":"Count", "schema":schema, "nullable":false
        });
        assert_eq!(serde_json::to_value(&clone).unwrap(), expected);
        let decoded: ActionParam = serde_json::from_value(expected).unwrap();
        assert_eq!(decoded, original);
        let spec = ActionSpec::typed(
            "count",
            "Count",
            "Count",
            ExecutionLocus::ClientLocal,
            vec![clone],
            |args| EncodedAction {
                type_id: "test.count".into(),
                payload: serde_json::to_vec(&args.to_json()).unwrap(),
            },
        );
        let valid = serde_json::json!({"count":2});
        let encoded = spec.build_json(valid.as_object().unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&encoded.payload).unwrap(),
            valid
        );
        assert!(
            spec.build_json(serde_json::json!({"count":0}).as_object().unwrap())
                .is_err()
        );
        assert_eq!(spec.build_text("2"), Some(encoded));
        assert_eq!(
            spec.build_text("0").unwrap().type_id,
            crate::resource::INVALID_ARGUMENTS_TYPE
        );
        let mut edited = original.clone();
        Arc::make_mut(edited.schema.as_mut().unwrap())["minimum"] = serde_json::json!(3);
        assert_eq!(original.schema.as_ref().unwrap()["minimum"], 1);
        assert!(ActionArgs::bind_json(&[edited], valid.as_object().unwrap()).is_err());
        assert!(spec.build_json(valid.as_object().unwrap()).is_ok());

        let resource = ActionParam::required("target", ParamType::Resource, "Target")
            .with_schema(serde_json::json!({"type":"string"}));
        let annotated = resource.clone().of_resource_kind("test.page");
        assert_eq!(resource.resource_kind(), None);
        assert_eq!(annotated.resource_kind(), Some("test.page"));
        assert_eq!(
            resource
                .schema
                .as_ref()
                .unwrap()
                .get(RESOURCE_KIND_ANNOTATION),
            None
        );
    }

    #[test]
    fn a_refusal_names_the_alternative_an_enumerated_tag_selects() {
        let schema = json!({"oneOf": [
            {"type": "object", "required": ["action"], "properties": {
                "action": {"enum": ["draw", "paint"]}, "args": {"type": "object"}
            }, "additionalProperties": false},
            {"type": "object", "required": ["action", "args"], "properties": {
                "action": {"const": "apply"}, "args": {"type": "object", "required": ["action"]}
            }, "additionalProperties": false},
        ]});
        assert!(validate_schema(&json!({"action": "draw", "args": {}}), &schema).is_ok());
        let refused = validate_schema(&json!({"action": "apply", "args": {}}), &schema);
        assert_eq!(
            refused,
            Err("as `apply`: property `args` must contain property `action`".into())
        );
        let refused = validate_schema(&json!({"action": "paint", "args": 3}), &schema).unwrap_err();
        assert!(refused.contains("property `args`"), "{refused}");
    }

    fn structured_arguments_are_typed_and_recursively_validated() {
        let params = vec![
            ActionParam::required("request", ParamType::Object, "request").with_schema(json!({
                "required": ["items"],
                "additionalProperties": false,
                "properties": {
                    "items": {"minItems": 1, "items": {"minimum": 1}}
                }
            })),
        ];
        let good = json!({"request": {"items": [1, 2]}})
            .as_object()
            .cloned()
            .unwrap();
        assert!(ActionArgs::bind_json(&params, &good).is_ok());

        let bad = json!({"request": {"items": [0], "surprise": true}})
            .as_object()
            .cloned()
            .unwrap();
        assert!(matches!(
            ActionArgs::bind_json(&params, &bad),
            Err(ArgError::Constraint { .. })
        ));

        assert!(
            validate_schema(
                &json!("#A0b1C2"),
                &json!({
                    "type": "string",
                    "pattern": "^#[0-9A-Fa-f]{6}$"
                })
            )
            .is_ok()
        );
        assert!(validate_schema(&json!(42), &json!({"type": "string"})).is_err());
        assert!(
            validate_schema(
                &json!("red"),
                &json!({
                    "type": "string",
                    "pattern": "^#[0-9A-Fa-f]{6}$"
                })
            )
            .is_err()
        );
    }

    fn resource_datetime_secret_and_nullable_values_round_trip() {
        let params = vec![
            ActionParam::required("resource", ParamType::Resource, "target"),
            ActionParam::required("at", ParamType::DateTime, "time"),
            ActionParam::required("token", ParamType::Secret, "credential"),
            ActionParam::optional("metadata", ParamType::Object, "metadata").nullable(),
        ];
        let supplied = json!({
            "resource": "files.entry:abc",
            "at": "2026-09-01T00:00:00Z",
            "token": "not-for-logs",
            "metadata": null
        })
        .as_object()
        .cloned()
        .unwrap();
        let bound = ActionArgs::bind_json(&params, &supplied).unwrap();
        assert_eq!(bound.text("resource"), Some("files.entry:abc"));
        assert_eq!(bound.to_json(), supplied);

        for invalid in [
            json!({"resource": "label only", "at": "2026-09-01T00:00:00Z", "token": "x"}),
            json!({"resource": "files.entry:abc", "at": "tomorrow", "token": "x"}),
            json!({"resource": "files.entry:abc", "at": "2026-09-01T00:00:00", "token": "x"}),
            json!({"resource": "files.entry:abc", "at": "2026-02-31T00:00:00Z", "token": "x"}),
        ] {
            assert!(matches!(
                ActionArgs::bind_json(&params, invalid.as_object().unwrap()),
                Err(ArgError::Constraint { .. })
            ));
        }
    }

    fn malformed_schema_is_rejected_before_it_describes_values() {
        assert!(validate_schema_definition(&json!({"pattern":"["})).is_err());
        assert!(validate_schema_definition(&json!({"format":"tilo-maybe"})).is_err());
        assert!(validate_schema_definition(&json!({"oneOf": []})).is_err());
        let one_of = json!({
            "oneOf": [
                {"const": "Workspace"},
                {"type": "object", "required": ["App"], "properties": {
                    "App": {"type": "string"}
                }, "additionalProperties": false}
            ]
        });
        assert!(validate_schema_definition(&one_of).is_ok());
        assert!(validate_json_schema(&json!("Workspace"), &one_of).is_ok());
        assert!(validate_json_schema(&json!({"App": "so.tilo.notes"}), &one_of).is_ok());
        assert!(validate_json_schema(&json!({"App": "notes", "extra": true}), &one_of).is_err());
        assert!(
            validate_schema_definition(&json!({
                "properties": {"nested": {"type":"mystery"}}
            }))
            .is_err()
        );
        assert!(validate_schema_definition(&json!({"minProperties": 1})).is_err());
        assert!(validate_schema_definition(&json!({"additionalProperties": {}})).is_err());
        assert!(validate_schema_definition(&json!({"title": "accepted annotation"})).is_ok());
    }

    fn unique_array_items_are_published_and_enforced() {
        let schema = json!({
            "type": "array",
            "uniqueItems": true,
            "items": {"type": "string"}
        });
        assert!(validate_schema_definition(&schema).is_ok());
        assert!(
            validate_json_schema(&json!(["photos.photo:a", "photos.photo:b"]), &schema).is_ok()
        );
        assert!(
            validate_json_schema(&json!(["photos.photo:a", "photos.photo:a"]), &schema).is_err()
        );
        assert!(validate_schema_definition(&json!({"uniqueItems": "yes"})).is_err());
    }
}
