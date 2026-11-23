//! The preview a search hit carries for the palette's preview band.

/// What the palette's preview band shows of a hit: a heading, short facts
/// side by side, items one per line and the start of its text. Data only; the
/// platform lays it out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchPreview {
    pub title: Option<String>,
    pub facts: Vec<String>,
    pub lines: Vec<String>,
    pub body: Option<String>,
}

impl SearchPreview {
    /// A preview headed `title`.
    #[must_use]
    pub fn titled(title: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            ..Self::default()
        }
    }

    /// One more fact beside the others.
    #[must_use]
    pub fn fact(mut self, fact: impl Into<String>) -> Self {
        self.facts.push(fact.into());
        self
    }

    /// One more item line.
    #[must_use]
    pub fn line(mut self, line: impl Into<String>) -> Self {
        self.lines.push(line.into());
        self
    }

    /// The start of its text.
    #[must_use]
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }
}
