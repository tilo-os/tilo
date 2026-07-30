//! Theme-derived colours and short labels shared by the app chrome and the
//! focusable list rows (`navrow`). Every colour is read from the `Theme` so
//! it tracks the OS light/dark/high-contrast track and accent.

use ui::theme::Theme;
use vello::peniko::Color;

use crate::model::{HttpMethod, StatusClass};
use crate::vars;

/// The accent colour for an HTTP method, derived from the theme's semantic
/// palette so it tracks the OS theme.
pub fn method_color(theme: &Theme, m: HttpMethod) -> Color {
    let c = &theme.tokens.colors;
    match m {
        HttpMethod::Get => c.semantic.success,
        HttpMethod::Post => c.semantic.info,
        HttpMethod::Put => c.semantic.warning,
        HttpMethod::Patch => c.accent[6],
        HttpMethod::Delete => c.semantic.danger,
        HttpMethod::Head | HttpMethod::Options => c.text_dim,
    }
}

pub fn status_color(theme: &Theme, class: StatusClass) -> Color {
    let c = &theme.tokens.colors;
    match class {
        StatusClass::Success => c.semantic.success,
        StatusClass::Redirect => c.semantic.info,
        StatusClass::ClientError => c.semantic.warning,
        StatusClass::ServerError => c.semantic.danger,
        StatusClass::Other => c.text_dim,
    }
}

pub fn token_color(theme: &Theme, kind: vars::TokenKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        vars::TokenKind::Key => c.semantic.info,
        vars::TokenKind::StringValue => c.semantic.success,
        vars::TokenKind::Number => c.accent[6],
        vars::TokenKind::Keyword => c.semantic.warning,
        vars::TokenKind::Punctuation => c.text_dim,
        vars::TokenKind::Plain => c.text_primary,
    }
}

/// A compact tag for a method, used on sidebar / history rows.
pub fn method_short(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PTCH",
        HttpMethod::Delete => "DEL",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPT",
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
        Some(i) => no_scheme[i..].to_string(),
        None => no_scheme.to_string(),
    }
}
