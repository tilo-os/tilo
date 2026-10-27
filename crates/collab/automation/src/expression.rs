//! The plain-logic layer: arithmetic, comparisons, text, lists and
//! dictionaries over the values bindings already carry.
//!
//! Nothing here is evaluated as code. An [`Expr`] is inert data naming one of
//! a fixed set of operations, so its type is known before anything runs: the
//! checker refuses a comparison between a number and a text, an arithmetic
//! operation on a list, a `map` over a field the items do not all have, before
//! a step is saved; the interpreter then applies the same operation to the
//! values it has.
//!
//! An expression only reads values — it never invokes anything — and one that
//! cannot be answered fails its step where it stands instead of producing a
//! value a later step would have to guess about.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::shortcut::Binding;

/// The binary operators of [`Expr::Compare`], [`Expr::Logic`] and
/// [`Expr::Math`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    // Comparisons.
    Equals,
    NotEquals,
    Less,
    LessOrEquals,
    Greater,
    GreaterOrEquals,
    // Boolean logic.
    And,
    Or,
    // Arithmetic.
    Add,
    Subtract,
    Multiply,
    Divide,
    #[serde(rename = "modulo")]
    Modulo,
    /// Text concatenation, which unlike `+` is not arithmetic.
    Concat,
}

impl Op {
    /// The word a person reads for this operator.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Equals => "is",
            Self::NotEquals => "is not",
            Self::Less => "<",
            Self::LessOrEquals => "<=",
            Self::Greater => ">",
            Self::GreaterOrEquals => ">=",
            Self::And => "and",
            Self::Or => "or",
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
            Self::Divide => "/",
            Self::Modulo => "modulo",
            Self::Concat => "concatenated with",
        }
    }

    /// Whether this operator compares rather than computes.
    #[must_use]
    pub const fn is_comparison(&self) -> bool {
        matches!(
            self,
            Self::Equals
                | Self::NotEquals
                | Self::Less
                | Self::LessOrEquals
                | Self::Greater
                | Self::GreaterOrEquals
        )
    }

    /// Whether this operator combines truth values.
    #[must_use]
    pub const fn is_logic(&self) -> bool {
        matches!(self, Self::And | Self::Or)
    }
}

/// One operation over values.
///
/// Every operation is total over the values it admits and refuses the ones it
/// does not: a text divided by a number, an array minus a number, a `map`
/// without a field are errors their step reports, never a `null` a later step
/// mistakes for a value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "expr", rename_all = "snake_case")]
pub enum Expr {
    /// The value a binding already has.
    Value {
        binding: Binding,
    },
    /// A binary operation: `Compare` for ordering and equality, `Logic` for
    /// truth values, `Math` for numbers and text concatenation.
    Compare {
        op: Op,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Logic {
        op: Op,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Math {
        op: Op,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `-value`.
    Negate {
        value: Box<Expr>,
    },
    /// Neither null, false, zero, nor an empty string, array or object.
    Truthy {
        value: Box<Expr>,
    },
    Not {
        value: Box<Expr>,
    },
    /// The length of a text, an array or a dictionary, in characters,
    /// elements or members.
    Length {
        value: Box<Expr>,
    },
    /// `text` with every occurrence of `find` replaced by `replace`.
    Replace {
        text: Box<Expr>,
        find: Box<Expr>,
        replace: Box<Expr>,
    },
    /// `text` cut at every occurrence of `separator`; `None` cuts at every
    /// character.
    Split {
        text: Box<Expr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        separator: Option<Box<Expr>>,
    },
    /// The first `count` elements of a list, or fewer when it is shorter.
    Take {
        items: Box<Expr>,
        count: Box<Expr>,
    },
    /// The one field `field` of a dictionary or resource, as a field access
    /// addresses it (`title`, `channels.0.name`).
    Field {
        value: Box<Expr>,
        field: String,
    },
    /// A dictionary of the given members.
    Object {
        members: Vec<Member>,
    },
    /// The elements of `items` for which `test` holds, in order.
    Filter {
        items: Box<Expr>,
        test: Box<Expr>,
    },
    /// `field` of each element of `items`, in order: `map over a field`.
    Map {
        items: Box<Expr>,
        field: String,
    },
    /// The elements of `items` from smallest to largest: numbers, or texts in
    /// lexicographic order.
    Sort {
        items: Box<Expr>,
    },
    /// The first element of `items`, when it has one.
    First {
        items: Box<Expr>,
    },
}

/// One member of an [`Expr::Object`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub name: String,
    pub value: Expr,
}

impl Member {
    /// The member `name`, whose value is `value`.
    #[must_use]
    pub fn new(name: &str, value: Expr) -> Self {
        Self {
            name: name.to_owned(),
            value,
        }
    }
}

impl Expr {
    /// A binary operation whose operator decides which kind it is. Parsed
    /// from a program, an operator and its operands may arrive under the
    /// wrong variant; this puts it where it belongs.
    #[must_use]
    pub fn operation(op: Op, left: Self, right: Self) -> Self {
        if op.is_comparison() {
            Self::Compare {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }
        } else if op.is_logic() {
            Self::Logic {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }
        } else {
            Self::Math {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }
        }
    }

    /// The JSON Schema describing what this expression produces, as far as
    /// its shape is knowable without a value: a step after a
    /// [`Compute`](crate::StepKind::Compute) reaches into a dictionary or a
    /// list it built as it would into any declared result.
    #[must_use]
    pub fn schema(&self) -> Option<Value> {
        let typed = |name: &str| Some(serde_json::json!({"type": name}));
        match self {
            Self::Math { op: Op::Concat, .. }
            | Self::Replace { .. }
            | Self::Field { .. }
            | Self::Value {
                binding: Binding::Template(_),
            } => typed("string"),
            Self::Math { .. } | Self::Negate { .. } => typed("number"),
            Self::Truthy { .. } | Self::Not { .. } | Self::Compare { .. } | Self::Logic { .. } => {
                typed("boolean")
            }
            Self::Length { .. } => typed("integer"),
            Self::Split { .. } => {
                Some(serde_json::json!({"type": "array", "items": {"type": "string"}}))
            }
            Self::Map { .. } => Some(serde_json::json!({"type": "array"})),
            Self::Take { .. } | Self::Filter { .. } | Self::Sort { .. } => typed("array"),
            Self::Object { members } => {
                let mut properties = Map::new();
                for member in members {
                    properties.insert(
                        member.name.clone(),
                        member
                            .value
                            .schema()
                            .unwrap_or_else(|| serde_json::json!({})),
                    );
                }
                Some(serde_json::json!({
                    "type": "object",
                    "properties": Value::Object(properties),
                }))
            }
            _ => None,
        }
    }

    /// Whether this expression or anything inside it reads a secret binding.
    /// A secret is a value like any other here; where it may be *shown* is
    /// the binding's business, not the expression's.
    #[must_use]
    pub fn reads(&self, binding_reads: &dyn Fn(&Binding) -> bool) -> bool {
        match self {
            Self::Value { binding } => binding_reads(binding),
            Self::Compare { left, right, .. }
            | Self::Logic { left, right, .. }
            | Self::Math { left, right, .. } => {
                left.reads(binding_reads) || right.reads(binding_reads)
            }
            Self::Negate { value }
            | Self::Truthy { value }
            | Self::Not { value }
            | Self::Length { value }
            | Self::Field { value, .. }
            | Self::Map { items: value, .. }
            | Self::Sort { items: value }
            | Self::First { items: value } => value.reads(binding_reads),
            Self::Replace {
                text,
                find,
                replace,
            } => {
                text.reads(binding_reads)
                    || find.reads(binding_reads)
                    || replace.reads(binding_reads)
            }
            Self::Split { text, separator } => {
                text.reads(binding_reads)
                    || separator
                        .as_ref()
                        .is_some_and(|separator| separator.reads(binding_reads))
            }
            Self::Take { items, count } => items.reads(binding_reads) || count.reads(binding_reads),
            Self::Object { members } => members
                .iter()
                .any(|member| member.value.reads(binding_reads)),
            Self::Filter { items, test } => items.reads(binding_reads) || test.reads(binding_reads),
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value { binding } => write!(f, "{}", describe_binding(binding)),
            Self::Compare { op, left, right }
            | Self::Logic { op, left, right }
            | Self::Math { op, left, right } => write!(f, "{left} {} {right}", op.as_str()),
            Self::Negate { value } => write!(f, "-{value}"),
            Self::Truthy { value } => write!(f, "{value} is set"),
            Self::Not { value } => write!(f, "not {value}"),
            Self::Length { value } => write!(f, "length of {value}"),
            Self::Replace {
                text,
                find,
                replace,
            } => {
                write!(f, "{text} with {find} replaced by {replace}")
            }
            Self::Split { text, separator } => match separator {
                Some(separator) => write!(f, "{text} split at {separator}"),
                None => write!(f, "{text} split into characters"),
            },
            Self::Take { items, count } => write!(f, "first {count} of {items}"),
            Self::Field { value, field } => write!(f, "{value} {field}"),
            Self::Object { members } => {
                f.write_str("{")?;
                for (index, member) in members.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {}", member.name, member.value)?;
                }
                f.write_str("}")
            }
            Self::Filter { items, test } => write!(f, "{items} where {test}"),
            Self::Map { items, field } => write!(f, "{field} of each of {items}"),
            Self::Sort { items } => write!(f, "{items} sorted"),
            Self::First { items } => write!(f, "first of {items}"),
        }
    }
}

/// A binding as an expression names it in words: its own name where it has
/// one, its value otherwise.
fn describe_binding(binding: &Binding) -> String {
    match binding {
        Binding::Literal(Value::String(text)) | Binding::Template(text) => format!("“{text}”"),
        Binding::Literal(value) => value.to_string(),
        Binding::Input(name) => format!("input {name}"),
        Binding::Var(name) => format!("variable {name}"),
        Binding::StepOutput { step, pointer } => {
            if pointer.is_empty() {
                format!("step {step}")
            } else {
                format!("step {step} {pointer}")
            }
        }
        Binding::Object(_) => "a dictionary".into(),
        Binding::List(_) => "a list".into(),
        Binding::Expression(expr) => expr.to_string(),
    }
}

/// Why an expression could not produce a value, or never could.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum ExprError {
    /// A value of the wrong type reached an operation.
    Type { what: String },
    // Every variant is spelled out as a struct so the JSON one carries in a
    // run report says what went wrong even when it holds nothing.
    /// A number operation would have left the numbers JSON can hold.
    OutOfRange {},
    /// A division or modulo by zero.
    DivideByZero {},
    /// The field an access or a `map` named is not there.
    MissingField { field: String },
    /// A comparison of values that have no order between them.
    Unordered { left: String, right: String },
    /// A `first` of an empty list.
    Empty {},
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Type { what } => f.write_str(what),
            Self::OutOfRange {} => f.write_str("the result is too large a number to hold"),
            Self::DivideByZero {} => f.write_str("it divides by zero"),
            Self::MissingField { field } => write!(f, "there is no field `{field}`"),
            Self::Unordered { left, right } => write!(f, "a {left} and a {right} have no order"),
            Self::Empty {} => f.write_str("the list is empty"),
        }
    }
}

impl std::error::Error for ExprError {}

/// Whether `value` is truthy: neither null, false, zero, nor an empty string,
/// array or object.
#[must_use]
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

/// The name a value's type goes by in an error a person reads.
#[must_use]
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a text",
        Value::Array(_) => "a list",
        Value::Object(_) => "a dictionary",
    }
}

fn wrong(what: impl Into<String>) -> ExprError {
    ExprError::Type { what: what.into() }
}

/// `Compare`: equality between any two values the checker admitted, order
/// only between numbers, or between texts.
///
/// # Errors
/// [`ExprError::Unordered`] when ordered values of different kinds meet.
pub fn compare(op: Op, left: &Value, right: &Value) -> Result<Value, ExprError> {
    let ordered = |l: &Value, r: &Value| -> Result<std::cmp::Ordering, ExprError> {
        match (l, r) {
            (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
                (Some(a), Some(b)) => a.partial_cmp(&b).ok_or_else(|| unordered(l, r)),
                _ => Err(ExprError::OutOfRange {}),
            },
            (Value::String(a), Value::String(b)) => Ok(a.cmp(b)),
            (Value::Bool(a), Value::Bool(b)) => Ok(a.cmp(b)),
            _ => Err(unordered(l, r)),
        }
    };
    // A whole number and the same number written with a fraction are the
    // same number: `1` and `1.0` are one value, not two kinds.
    let equal = match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => left == right,
    };
    let holds = match op {
        Op::Equals => equal,
        Op::NotEquals => !equal,
        Op::Less => ordered(left, right)?.is_lt(),
        Op::LessOrEquals => ordered(left, right)?.is_le(),
        Op::Greater => ordered(left, right)?.is_gt(),
        Op::GreaterOrEquals => ordered(left, right)?.is_ge(),
        _ => {
            return Err(wrong(format!("`{}` is not a comparison", op.as_str())));
        }
    };
    Ok(Value::Bool(holds))
}

fn unordered(left: &Value, right: &Value) -> ExprError {
    ExprError::Unordered {
        left: type_name(left).to_string(),
        right: type_name(right).to_string(),
    }
}

/// `Math`: numbers, or texts joined with [`Op::Concat`].
///
/// # Errors
/// A type the operation is not defined for, a division by zero, or a result
/// outside the numbers JSON holds.
pub fn arithmetic(op: Op, left: &Value, right: &Value) -> Result<Value, ExprError> {
    if op == Op::Concat {
        return Ok(Value::String(format!("{}{}", joins(left)?, joins(right)?)));
    }
    let number = |value: &Value| -> Result<f64, ExprError> {
        match value {
            Value::Number(number) => number.as_f64().ok_or(ExprError::OutOfRange {}),
            other => Err(wrong(format!(
                "arithmetic needs numbers, and {} is not one",
                type_name(other)
            ))),
        }
    };
    let (a, b) = (number(left)?, number(right)?);
    let computed = match op {
        Op::Add => a + b,
        Op::Subtract => a - b,
        Op::Multiply => a * b,
        Op::Divide | Op::Modulo if b == 0.0 => return Err(ExprError::DivideByZero {}),
        Op::Divide => a / b,
        Op::Modulo => a % b,
        _ => {
            return Err(wrong(format!("`{}` is not arithmetic", op.as_str())));
        }
    };
    if !computed.is_finite() {
        return Err(ExprError::OutOfRange {});
    }
    // A whole answer stays whole: a person who adds 1 and 2 expects 3, not
    // 3.0, and an integer parameter refuses the latter.
    Ok(match exact(computed) {
        Some(whole) => Value::from(whole),
        None => Value::from(computed),
    })
}

/// `number` as a whole number, when it is one that an `i64` holds exactly.
fn exact(number: f64) -> Option<i64> {
    if number.fract() != 0.0 || number.abs() >= 9.0e18 {
        return None;
    }
    let number = number.to_string();
    number.parse().ok()
}

/// How a value reads once it is joined into text: a person writing a name
/// from a counter or a count expects that number in it.
fn joins(value: &Value) -> Result<String, ExprError> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(flag) => Ok(flag.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => Err(wrong(format!(
            "only a text, a number or a truth value joins into text, and {} cannot",
            type_name(value)
        ))),
    }
}

/// `Negate`.
///
/// # Errors
/// When the value is not a number.
pub fn negate(value: &Value) -> Result<Value, ExprError> {
    arithmetic(Op::Subtract, &Value::from(0), value)
}

/// `Length` of a text, list or dictionary.
///
/// # Errors
/// When the value is none of those.
pub fn length(value: &Value) -> Result<Value, ExprError> {
    let size = match value {
        // Characters, not bytes: someone counting a name counts what they see.
        Value::String(text) => text.chars().count(),
        Value::Array(items) => items.len(),
        Value::Object(fields) => fields.len(),
        other => {
            return Err(wrong(format!(
                "only a text, a list or a dictionary has a length, and {} has none",
                type_name(other)
            )));
        }
    };
    Ok(Value::from(size))
}

/// `Field`: the part a dotted path names.
///
/// # Errors
/// A path through a value that has no fields, or a member that is not there.
pub fn field(value: &Value, field: &str) -> Result<Value, ExprError> {
    let mut current = value.clone();
    for token in field_tokens(field) {
        current = match &current {
            Value::Object(members) => {
                members
                    .get(token)
                    .cloned()
                    .ok_or_else(|| ExprError::MissingField {
                        field: token.to_string(),
                    })?
            }
            Value::Array(items) => {
                let index: usize = token
                    .parse()
                    .map_err(|_| wrong(format!("`{token}` is not a position in a list")))?;
                items.get(index).cloned().ok_or(ExprError::Empty {})?
            }
            other => {
                return Err(wrong(format!(
                    "{} has no field `{token}`",
                    type_name(other)
                )));
            }
        };
    }
    Ok(current)
}

/// The parts of a dotted field path, so `channels.0.name` reaches into a list
/// as a JSON pointer would.
pub fn field_tokens(field: &str) -> impl Iterator<Item = &str> {
    field.split('.').filter(|token| !token.is_empty())
}

/// `Map`: one field of each element of a list.
///
/// # Errors
/// When an element has no such field, or is not a dictionary.
pub fn map_field(items: &Value, field: &str) -> Result<Value, ExprError> {
    let Value::Array(elements) = items else {
        return Err(wrong(format!(
            "map needs a list, and {} is not one",
            type_name(items)
        )));
    };
    elements
        .iter()
        .map(|element| self::field(element, field))
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

/// `Split`.
///
/// # Errors
/// When the text is not a text.
pub fn split(text: &Value, separator: Option<&Value>) -> Result<Value, ExprError> {
    let text = as_text(text, "split")?;
    let pieces: Vec<Value> = match separator {
        None => text.chars().map(|c| Value::from(c.to_string())).collect(),
        Some(Value::String(separator)) if separator.is_empty() => {
            text.chars().map(|c| Value::from(c.to_string())).collect()
        }
        Some(Value::String(separator)) => text
            .split(separator.as_str())
            .map(|piece| Value::from(piece.to_string()))
            .collect(),
        Some(other) => {
            return Err(wrong(format!(
                "a split needs a text to split at, and {} is not one",
                type_name(other)
            )));
        }
    };
    Ok(Value::Array(pieces))
}

/// `Replace`.
///
/// # Errors
/// When any of the three is not a text.
pub fn replace(text: &Value, find: &Value, replace: &Value) -> Result<Value, ExprError> {
    let text = as_text(text, "replace")?;
    let find = as_text(find, "replace")?;
    let replaced = as_text(replace, "replace")?;
    Ok(Value::String(if find.is_empty() {
        text.to_string()
    } else {
        text.replace(find, replaced)
    }))
}

fn as_text<'v>(value: &'v Value, what: &str) -> Result<&'v str, ExprError> {
    match value {
        Value::String(text) => Ok(text),
        other => Err(wrong(format!(
            "{what} works on text, and {} is not one",
            type_name(other)
        ))),
    }
}

/// `Take`: the first `count` elements of a list.
///
/// # Errors
/// When the value is not a list or the count is not a whole, non-negative
/// number.
pub fn take(items: &Value, count: &Value) -> Result<Value, ExprError> {
    let Value::Array(elements) = items else {
        return Err(wrong(format!(
            "take needs a list, and {} is not one",
            type_name(items)
        )));
    };
    let count = whole(count, "take")?;
    Ok(Value::Array(elements.iter().take(count).cloned().collect()))
}

fn whole(value: &Value, what: &str) -> Result<usize, ExprError> {
    match value {
        Value::Number(number) => match number.as_u64() {
            Some(count) => usize::try_from(count).map_err(|_| ExprError::OutOfRange {}),
            None => match number.as_i64() {
                // Taking a negative number of elements takes none rather than
                // counting from the end: a bound is a count.
                Some(count) if count < 0 => Ok(0),
                _ => Err(wrong(format!(
                    "{what} needs a whole number, and {number} is not one"
                ))),
            },
        },
        other => Err(wrong(format!(
            "{what} needs a whole number, and {} is not one",
            type_name(other)
        ))),
    }
}

/// `Sort` of numbers or of texts.
///
/// # Errors
/// When the elements are neither all numbers nor all texts.
pub fn sort(items: &Value) -> Result<Value, ExprError> {
    let Value::Array(elements) = items else {
        return Err(wrong(format!(
            "sort needs a list, and {} is not one",
            type_name(items)
        )));
    };
    let mut elements = elements.clone();
    if elements.iter().all(Value::is_number) {
        elements.sort_by(|a, b| {
            let (a, b) = (a.as_f64().unwrap_or(0.0), b.as_f64().unwrap_or(0.0));
            a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
        });
    } else if elements.iter().all(Value::is_string) {
        elements.sort_by(|a, b| {
            let (a, b) = (a.as_str().unwrap_or(""), b.as_str().unwrap_or(""));
            a.cmp(b)
        });
    } else {
        return Err(wrong(
            "a list sorts when it holds numbers only or texts only",
        ));
    }
    Ok(Value::Array(elements))
}

/// `Object`: a dictionary of evaluated members.
#[must_use]
pub fn object(members: Vec<(String, Value)>) -> Value {
    let mut map = Map::new();
    for (name, value) in members {
        map.insert(name, value);
    }
    Value::Object(map)
}
