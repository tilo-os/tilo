use ui::theme::Theme;
use ui::widget::Tone;
use vello::peniko::Color;

use crate::model::StatusClass;
use crate::vars;

pub fn status_tone(class: StatusClass) -> Tone {
    match class {
        StatusClass::Success => Tone::Success,
        StatusClass::Redirect => Tone::Info,
        StatusClass::ClientError => Tone::Warning,
        StatusClass::ServerError => Tone::Danger,
        StatusClass::Other => Tone::Neutral,
    }
}

/// Colour for a JSON token, from the theme's shared syntax palette.
pub fn token_color(theme: &Theme, kind: vars::TokenKind) -> Color {
    let syntax = &theme.tokens.syntax;
    match kind {
        vars::TokenKind::Key => syntax.function,
        vars::TokenKind::StringValue => syntax.string,
        vars::TokenKind::Number | vars::TokenKind::Keyword => syntax.number,
        vars::TokenKind::Punctuation => syntax.operator,
        vars::TokenKind::Plain => syntax.text,
    }
}

pub fn status_class(status: u16) -> StatusClass {
    match status {
        200..=299 => StatusClass::Success,
        300..=399 => StatusClass::Redirect,
        400..=499 => StatusClass::ClientError,
        500..=599 => StatusClass::ServerError,
        _ => StatusClass::Other,
    }
}

pub use api_tester_model::short_path;
