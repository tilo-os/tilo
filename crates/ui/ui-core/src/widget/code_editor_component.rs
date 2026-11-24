//! [`CodeEditor`] as a described component.

use crate::component::{Bound, Component, Described};

use super::{CodeEditor, Language};

/// Code to read or write, with line numbers and its language's colouring.
#[derive(Component)]
#[component(name = "CodeEditor", role = "TextInput", label = "placeholder")]
pub struct CodeEditorProps {
    /// The text it edits.
    #[component(bind(merge = "splice"))]
    pub value: Bound<String>,
    /// What to type, shown while it is empty.
    pub placeholder: Option<String>,
    /// The language it colours (the language's own name, `rust`, `json`); plain text unless said.
    pub language: Option<Language>,
    /// The fewest lines it is tall for.
    pub min_lines: Option<i64>,
    /// The most lines it is tall for; past them it scrolls.
    pub max_lines: Option<i64>,
    /// The line numbers show.
    pub gutter: bool,
    /// It is read and copied, never edited.
    pub read_only: bool,
    /// The place to show, written `line` or `line:column` counted from 1; the same place asked for
    /// again needs a count after it (`3:1:2`), so a person's own scrolling is left alone.
    pub reveal: Option<String>,
}

impl Default for CodeEditorProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            placeholder: None,
            language: None,
            min_lines: None,
            max_lines: None,
            gutter: true,
            read_only: false,
            reveal: None,
        }
    }
}

impl Described for CodeEditor {
    type Props = CodeEditorProps;

    fn from_props(_key: &str, props: CodeEditorProps) -> Self {
        let CodeEditorProps {
            value,
            placeholder,
            language,
            min_lines,
            max_lines,
            gutter,
            read_only,
            reveal,
        } = props;
        let mut editor = Self::from_state(&value.state)
            .with_gutter(gutter)
            .with_placeholder(placeholder.as_deref().unwrap_or_default());
        if let Some(language) = language {
            editor = editor.with_language(language);
        }
        if let Some(lines) = min_lines {
            editor = editor.with_min_lines(usize::try_from(lines.max(1)).unwrap_or(1));
        }
        if let Some(lines) = max_lines {
            editor = editor.with_max_lines(usize::try_from(lines.max(1)).unwrap_or(1));
        }
        if read_only {
            editor = editor.read_only();
        }
        if let Some(place) = reveal {
            editor.goto_place(&place);
        }
        editor
    }

    fn set_props(&mut self, props: CodeEditorProps) -> bool {
        let CodeEditorProps {
            value,
            placeholder,
            language,
            min_lines,
            max_lines,
            gutter,
            read_only,
            reveal,
        } = props;
        if !self.inner.follows(&value.state) {
            self.inner.follow(&value.state);
        }
        self.inner.set_gutter(gutter);
        self.inner
            .set_placeholder(placeholder.as_deref().unwrap_or_default());
        if let Some(language) = language {
            self.inner.set_highlighter(language.highlighter());
        }
        self.set_read_only(read_only);
        if let Some(place) = reveal
            && self.revealed.as_deref() != Some(place.as_str())
        {
            self.goto_place(&place);
        }
        self.min_lines = min_lines.map(|lines| usize::try_from(lines.max(1)).unwrap_or(1));
        self.max_lines = max_lines.map(|lines| usize::try_from(lines.max(1)).unwrap_or(1));
        true
    }
}
