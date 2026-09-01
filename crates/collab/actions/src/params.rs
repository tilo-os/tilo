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
}

impl ParamType {
    /// Wire spelling, and what an enumeration descriptor carries.
    pub fn label(self) -> &'static str {
        match self {
            ParamType::Text => "text",
            ParamType::Integer => "integer",
            ParamType::Number => "number",
            ParamType::Boolean => "boolean",
        }
    }
}

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
        }
    }

    /// Restrict this parameter to `choices`. Binding refuses anything else.
    #[must_use]
    pub fn with_choices<S: Into<Str>>(mut self, choices: impl IntoIterator<Item = S>) -> Self {
        self.choices = choices.into_iter().map(Into::into).collect();
        self
    }
}

/// One bound argument value.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
    Text(String),
    Integer(i64),
    Number(f64),
    Boolean(bool),
}

impl ArgValue {
    /// The declared type this value satisfies.
    pub fn ty(&self) -> ParamType {
        match self {
            ArgValue::Text(_) => ParamType::Text,
            ArgValue::Integer(_) => ParamType::Integer,
            ArgValue::Number(_) => ParamType::Number,
            ArgValue::Boolean(_) => ParamType::Boolean,
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
            _ => None,
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
                write!(f, "argument `{name}` must be a string, number or boolean")
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
            if !value.satisfies(param.ty) {
                return Err(ArgError::WrongType {
                    name,
                    expected: param.ty,
                    got: value.ty(),
                });
            }
            if !param.choices.is_empty() {
                let got = match &value {
                    ArgValue::Text(s) => s.clone(),
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
            let Some(v) = ArgValue::from_json(value) else {
                return Err(ArgError::Unrepresentable { name: name.clone() });
            };
            pairs.push((name.clone(), v));
        }
        Self::bind(params, pairs)
    }

    /// The text bound to `name`, if any.
    pub fn text(&self, name: &str) -> Option<&str> {
        match self.values.get(name) {
            Some(ArgValue::Text(s)) => Some(s.as_str()),
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
