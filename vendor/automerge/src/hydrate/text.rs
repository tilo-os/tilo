use std::{collections::HashMap, fmt::Display};

use crate::marks::{ExpandMark, Mark};
use crate::{text_value::ConcreteTextValue, PatchAction, ScalarValue, TextEncoding};

use super::HydrateError;

#[derive(Clone)]
pub struct Text {
    value: Letters,
    marks: HashMap<String, ScalarValue>,
    /// TILO(batched-marks): the marks a batch that makes this text writes
    /// over it ([`Text::with_mark`]).
    written_marks: Vec<(Mark, ExpandMark)>,
}

/// TILO(written-text): a text made from a string keeps the string until a
/// patch edits it, so writing a new text, or reading one into a hydrated
/// value, costs no tree of its letters.
#[derive(Clone)]
enum Letters {
    Written(String, TextEncoding),
    Edited(ConcreteTextValue),
}

impl Letters {
    fn make_string(&self) -> String {
        match self {
            Self::Written(text, _) => text.clone(),
            Self::Edited(value) => value.make_string(),
        }
    }

    fn edited(&mut self) -> &mut ConcreteTextValue {
        if let Self::Written(text, encoding) = self {
            *self = Self::Edited(ConcreteTextValue::new(text, *encoding));
        }
        match self {
            Self::Edited(value) => value,
            Self::Written(..) => unreachable!("made edited above"),
        }
    }

    fn encoding(&self) -> TextEncoding {
        match self {
            Self::Written(_, encoding) => *encoding,
            Self::Edited(ConcreteTextValue::Utf8CodeUnit(_)) => TextEncoding::Utf8CodeUnit,
            Self::Edited(ConcreteTextValue::Utf16CodeUnit(_)) => TextEncoding::Utf16CodeUnit,
            Self::Edited(ConcreteTextValue::CodePoint(_)) => TextEncoding::UnicodeCodePoint,
            Self::Edited(ConcreteTextValue::Grapheme(_)) => TextEncoding::GraphemeCluster,
        }
    }
}

impl PartialEq for Letters {
    /// The same letters, counted the same way.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Edited(a), Self::Edited(b)) => a == b,
            (Self::Written(a, _), Self::Written(b, _)) => {
                a == b && self.encoding() == other.encoding()
            }
            _ => self.encoding() == other.encoding() && self.make_string() == other.make_string(),
        }
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
            && self.marks == other.marks
            && self.written_marks == other.written_marks
    }
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
                    .edited()
                    .splice_text_value(index, &value)
                    .map_err(|_| HydrateError::InvalidEncoding)?;
                Ok(())
            }
            PatchAction::Insert { index, values } => {
                let mut index = index;
                for value in values.iter() {
                    let value = super::Value::new(value.0.clone(), text_encoding);
                    let text = value.as_str();
                    self.value.edited().splice(index, text);
                    index += text_encoding.width(text);
                }
                Ok(())
            }
            PatchAction::PutSeq { index, value, .. } => {
                let edited = self.value.edited();
                edited.remove(index);
                let value = super::Value::new(value.0, text_encoding);
                edited.splice(index, value.as_str());
                Ok(())
            }
            PatchAction::DeleteSeq { index, length } => {
                let edited = self.value.edited();
                for _ in 0..length {
                    edited.remove(index);
                }
                Ok(())
            }
            PatchAction::Mark { marks: _ } => {
                todo!()
            }
            p => Err(HydrateError::InvalidTextOp(p)),
        }
    }

    /// The text and its marks, as a model module and its host pass it.
    pub(crate) fn parts(&self) -> (String, &HashMap<String, ScalarValue>) {
        (self.value.make_string(), &self.marks)
    }

    /// Text of these parts, as a model module and its host pass it.
    pub(crate) fn from_parts(text: &str, marks: HashMap<String, ScalarValue>) -> Self {
        Self {
            value: Letters::Written(text.to_owned(), TextEncoding::platform_default()),
            marks,
            written_marks: Vec::new(),
        }
    }

    pub fn new<S: AsRef<str>>(text_encoding: TextEncoding, text: S) -> Self {
        Self {
            value: Letters::Written(text.as_ref().to_owned(), text_encoding),
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

    /// TILO(written-text): the text, borrowed when it was made from a
    /// string and not edited since.
    pub(crate) fn as_str(&self) -> std::borrow::Cow<'_, str> {
        match &self.value {
            Letters::Written(text, _) => std::borrow::Cow::Borrowed(text),
            Letters::Edited(value) => std::borrow::Cow::Owned(value.make_string()),
        }
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
            value: Letters::Edited(value),
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
