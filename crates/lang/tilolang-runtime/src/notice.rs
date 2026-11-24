//! What an action says in passing in the window it runs from: `notice("Copied", kind: .success)`.
//!
//! The window's bar shows it where its location stands for a few seconds, with at most one button
//! beside it (the person's undo), and a newer one replaces it. The runtime keeps what the code said
//! until the host takes it ([`crate::Runtime::take_said`]), which is what the app's window shows:
//! a run with no window says it to nobody. Nothing is written to the workspace, unlike
//! [`crate::notify`], whose record every device of the person's reads.

/// How a notice reads: the tone its icon and colour take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

impl Kind {
    pub const ALL: &'static [Self] = &[Self::Info, Self::Success, Self::Warning, Self::Error];

    /// The kind `name` spells, as a choice of the language writes it: `info`, `success`,
    /// `warning` or `error`. Anything else is `info`.
    #[must_use]
    pub fn parse(name: &str) -> Self {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.word() == name)
            .unwrap_or_default()
    }

    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// One thing an action said in passing, as it waits for the window that shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    /// The words, as the action wrote them: a person reads them as they are.
    pub text: String,
    pub kind: Kind,
    /// Whether it offers the person's undo while it shows.
    pub undo: bool,
}
