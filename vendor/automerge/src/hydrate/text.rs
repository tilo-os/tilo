use std::{collections::HashMap, fmt::Display};

use crate::marks::{ExpandMark, Mark};
use crate::{text_value::ConcreteTextValue, PatchAction, ScalarValue, TextEncoding};

use super::HydrateError;

#[derive(Clone, PartialEq)]
pub struct Text {
    value: ConcreteTextValue,
    marks: HashMap<String, ScalarValue>,
    /// TILO(batched-marks): the marks a batch that makes this text writes
    /// over it ([`Text::with_mark`]).
    written_marks: Vec<(Mark, ExpandMark)>,
}

impl std::fmt::Debug for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Text")
            .field("value", &self.value.make_string())
            .field("marks", &self.marks)
            .field("written_marks", &self.written_marks)
            .finish()
    }
}

impl Text {
    pub(crate) fn apply(
        &mut self,
        text_encoding: TextEncoding,
        patch: PatchAction,
    ) -> Result<(), HydrateError> {
        match patch {
            PatchAction::SpliceText { index, value, .. } => {
                self.value
                    .splice_text_value(index, &value)
                    .map_err(|_| HydrateError::InvalidEncoding)?;
                Ok(())
            }
            PatchAction::Insert { index, values } => {
                let mut index = index;
                for value in values.iter() {
                    let value = super::Value::new(value.0.clone(), text_encoding);
                    let text = value.as_str();
                    self.value.splice(index, text);
                    index += text_encoding.width(text);
                }
                Ok(())
            }
            PatchAction::PutSeq { index, value, .. } => {
                self.value.remove(index);
                let value = super::Value::new(value.0, text_encoding);
                self.value.splice(index, value.as_str());
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
            written_marks: Vec::new(),
        }
    }

    /// TILO(batched-marks): this text with `mark` over it, as
    /// `Transactable::mark` with `expand` would set it once the text is
    /// written. A batch that makes the text (`batch_create_object` and the
    /// calls sharing it) writes its marks with it, in the order they were
    /// added, as the same marks set one after another on the new text. Each
    /// mark's start and end must fall between elements of the text, within
    /// it. Reading a document never fills these in.
    #[must_use]
    pub fn with_mark(mut self, mark: Mark, expand: ExpandMark) -> Self {
        self.written_marks.push((mark, expand));
        self
    }

    /// TILO(batched-marks): the marks [`Text::with_mark`] added.
    pub fn written_marks(&self) -> &[(Mark, ExpandMark)] {
        &self.written_marks
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
            written_marks: Vec::new(),
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
