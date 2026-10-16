//! Small presentation helpers shared by the views: which tone a status
//! reads as, the JSON token inks, and the short label of a URL.

use ui::theme::Theme;
use ui::widget::Tone;
use vello::peniko::Color;

use crate::model::StatusClass;
use crate::vars;

/// What a response status means, as the toolkit's status tone.
pub fn status_tone(class: StatusClass) -> Tone {
    match class {
        StatusClass::Success => Tone::Success,
        StatusClass::Redirect => Tone::Info,
        StatusClass::ClientError => Tone::Warning,
        StatusClass::ServerError => Tone::Danger,
        StatusClass::Other => Tone::Neutral,
    }
}

/// Colour for a highlighted JSON token, from the theme's shared syntax
/// palette so response bodies read like every other code surface.
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

/// Strip the scheme/host, leaving the path tail for compact history labels.
pub fn short_path(url: &str) -> String {
    let no_scheme = url.split("://").nth(1).unwrap_or(url);
    match no_scheme.find('/') {
        Some(i) => no_scheme.get(i..).unwrap_or(no_scheme).to_string(),
        None => no_scheme.to_string(),
    }
}
