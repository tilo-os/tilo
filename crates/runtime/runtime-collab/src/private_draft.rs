//! Ephemeral editor handoff vocabulary. Never put this payload in an action.

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Target {
    Person { public_key: String },
}

/// UTF-8 byte endpoints, in anchor/caret order. Preedit remains display-only.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Editor {
    pub text: String,
    pub anchor: usize,
    pub caret: usize,
    pub preedit: String,
    pub preedit_cursor: Option<(usize, usize)>,
}
impl Editor {
    /// # Errors
    /// Refuses oversized text/composition or endpoints outside UTF-8 boundaries.
    pub fn validate(&self) -> Result<(), String> {
        if self.text.len() > 16384
            || self.preedit.len() > 4096
            || !self.text.is_char_boundary(self.anchor)
            || !self.text.is_char_boundary(self.caret)
            || self.preedit_cursor.is_some_and(|(a, b)| {
                !self.preedit.is_char_boundary(a) || !self.preedit.is_char_boundary(b)
            })
        {
            return Err("invalid private editor handoff".into());
        }
        Ok(())
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub workspace: String,
    pub target: Target,
    pub editor: Editor,
}

/// Multiplexed on the existing private native channel, not a public action RPC.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub private_draft: uuid::Uuid,
    pub acknowledge: bool,
}
