use std::{collections::HashMap, fmt::Display};

use crate::{text_value::ConcreteTextValue, PatchAction, ScalarValue, TextEncoding};

use super::HydrateError;

#[derive(Clone, PartialEq)]
pub struct Text {
    value: ConcreteTextValue,
    marks: HashMap<String, ScalarValue>,
}

impl std::fmt::Debug for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Text")
            .field("value", &self.value.make_string())
            .field("marks", &self.marks)
            .finish()
    }
}

impl Text {
    pub(crate) fn apply(&mut self, patch: PatchAction) -> Result<(), HydrateError> {
        match patch {
            PatchAction::SpliceText { index, value, .. } => {
                self.value
                    .splice_text_value(index, &value)
                    .map_err(|_| HydrateError::InvalidEncoding)?;
                Ok(())
            }
            PatchAction::DeleteSeq { index, length } => {
                for _ in 0..length {
                    self.value.remove(index);
                }
                Ok(())
            }
            PatchAction::Mark { marks: _ } => {
                todo!()
            }
            p => Err(HydrateError::InvalidTextOp(p)),
        }
    }

    pub fn new<S: AsRef<str>>(text_encoding: TextEncoding, text: S) -> Self {
        Self {
            value: ConcreteTextValue::new(text.as_ref(), text_encoding),
            marks: Default::default(),
        }
    }
}

impl From<&Text> for String {
    fn from(text: &Text) -> Self {
        text.value.make_string()
    }
}

impl Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value.make_string())
    }
}

impl From<ConcreteTextValue> for Text {
    fn from(value: ConcreteTextValue) -> Self {
        Self {
            value,
            marks: Default::default(),
        }
    }
}

impl From<&ConcreteTextValue> for Text {
    fn from(value: &ConcreteTextValue) -> Self {
        Self::from(value.clone())
    }
}

impl From<ConcreteTextValue> for crate::hydrate::Value {
    fn from(value: ConcreteTextValue) -> Self {
        crate::hydrate::Value::Text(Text::from(value))
    }
}

impl From<&ConcreteTextValue> for crate::hydrate::Value {
    fn from(value: &ConcreteTextValue) -> Self {
        Self::from(value.clone())
    }
}

// Native allocation traversal is read-only and does not populate caches.
use crate::resource_limits::retained::{NativeVisit, Visitor};
impl NativeVisit for Text {
    fn native_visit(
        &self,
        visitor: &mut Visitor<'_>,
    ) -> Result<(), crate::resource_limits::NativeResourceError> {
        let Self { value, marks } = self;
        // This map is private and has no mutating implementation; Mark apply
        // is explicitly unimplemented. Any future population needs a bound.
        if !marks.is_empty() || marks.capacity() != 0 {
            return Err(crate::resource_limits::NativeResourceError::Unaccounted);
        }
        value.native_visit(visitor)
    }
}
