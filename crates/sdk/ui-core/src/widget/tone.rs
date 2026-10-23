//! `Tone`: the one vocabulary content composites use when a mark carries
//! meaning. Colour means that someone is there or that something is
//! happening, so a tone is either a status or a person; there is no
//! decorative tone.

use vello::peniko::Color;

use crate::theme::Theme;
use crate::tokens::colors::PresenceHue;

/// What a coloured mark (a card's leading stripe, a stat's ticker, an empty
/// state's glyph) means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    /// No status: secondary ink.
    #[default]
    Neutral,
    Success,
    Warning,
    Danger,
    /// Information that is not a warning (an update is available).
    Info,
    /// A person's or an agent's presence colour (a reviewer, an author).
    Presence(PresenceHue),
}

impl Tone {
    #[must_use]
    pub fn colour(self, theme: &Theme) -> Color {
        let colors = &theme.tokens.colors;
        match self {
            Self::Neutral => theme.text_dim,
            Self::Success => colors.semantic.success,
            Self::Warning => colors.semantic.warning,
            Self::Danger => colors.semantic.danger,
            Self::Info => colors.semantic.info,
            Self::Presence(hue) => colors.presence.hue(hue),
        }
    }

    /// The presence tone of the person `identity`.
    #[must_use]
    pub fn of_identity(identity: &str) -> Self {
        Self::Presence(PresenceHue::of_identity(identity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_resolve_to_status_or_presence_only() {
        let theme = Theme::dark();
        let colors = &theme.tokens.colors;
        assert_eq!(Tone::Danger.colour(&theme), colors.semantic.danger);
        assert_eq!(Tone::Neutral.colour(&theme), theme.text_dim);
        assert_eq!(
            Tone::of_identity("ada").colour(&theme),
            colors.presence.hue(PresenceHue::of_identity("ada"))
        );
    }
}
