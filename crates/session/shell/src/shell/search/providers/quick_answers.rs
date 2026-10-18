//! Answers to queries that are not names: arithmetic, an address, and, when
//! nothing else matches, a web search.
//!
//! "2+2", `https://example.com` and "weather" are among the commonest things
//! typed into a launcher, and each used to end in "No results". The sum is
//! worked out as it is typed and Enter copies it; an address opens through
//! [`crate::shell::uri_open`], placed like any launch; and a query nothing
//! answers offers to search the web for it ([`SearchProvider::is_fallback`]),
//! which a person has to choose: nothing typed leaves the machine before.

use std::fmt::Write as _;

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "quick-answers";

const SUM_ID: &str = "sum";
const ADDRESS_ID: &str = "address";
const WEB_ID: &str = "web";

/// Where a web search goes: a search engine that does not profile the person
/// searching.
const WEB_SEARCH: &str = "https://duckduckgo.com/?q=";

/// The longest expression worked out; a longer query is prose.
const MAX_EXPRESSION_SCALARS: usize = 120;

pub struct QuickAnswersProvider {
    kinds: Vec<ResourceKind>,
}

impl Default for QuickAnswersProvider {
    fn default() -> Self {
        Self {
            kinds: vec![ResourceKind::Custom(t("search-kind-answers"))],
        }
    }
}

impl QuickAnswersProvider {
    pub fn new() -> Self {
        Self::default()
    }

    fn kind(&self) -> ResourceKind {
        self.kinds
            .first()
            .cloned()
            .unwrap_or_else(|| ResourceKind::Custom(t("search-kind-answers")))
    }
}

fn row(
    kind: ResourceKind,
    id: String,
    title: String,
    subtitle: Option<String>,
    score: i32,
    open: impl Fn(&Chrome) -> Result<(), ActionError> + 'static,
) -> SearchResult {
    let mut row = SearchResult::nullary_fallible(
        kind,
        PROVIDER_ID,
        id,
        title,
        subtitle,
        score,
        move |_doc, _root, chrome| open(chrome),
    );
    row.contract = Some(tilo_actions::ActionContract::view());
    row
}

fn open_uri(uri: &str, chrome: &Chrome) -> Result<(), ActionError> {
    crate::shell::uri_open::open_in_workspace(uri, chrome)
        .map(drop)
        .map_err(|error| {
            ActionError::Explained(match error {
                crate::shell::uri_open::UriOpenError::NoHandler { .. } => {
                    t("search-answer-no-browser")
                }
                crate::shell::uri_open::UriOpenError::NoWorkspace => t("doc-open-no-workspace"),
                crate::shell::uri_open::UriOpenError::InvalidUri => t("search-answer-bad-address"),
                crate::shell::uri_open::UriOpenError::Launch(error) => error.user_message(),
            })
        })
}

impl SearchProvider for QuickAnswersProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Quick answers"
    }

    fn display_name(&self) -> String {
        t("search-provider-quick-answers")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    fn is_fallback(&self, result: &SearchResult) -> bool {
        result
            .id
            .split_once(':')
            .is_some_and(|(answer, _)| answer == WEB_ID)
    }

    /// An answer's address carries what it answers (`sum:2+2`,
    /// `web:weather`), so it resolves without the query that found it.
    fn resolve(&self, id: &str, _ctx: SearchContext<'_>) -> Option<SearchResult> {
        let (_, typed) = id.split_once(':')?;
        self.answers(typed).into_iter().find(|row| row.id == id)
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&self.kind()) {
            return Vec::new();
        }
        self.answers(ctx.needle)
    }
}

impl QuickAnswersProvider {
    /// What answers `typed`: its sum, the address it is, or a web search.
    fn answers(&self, typed: &str) -> Vec<SearchResult> {
        let typed = typed.trim();
        if typed.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Some(value) = evaluate(typed) {
            let answer = format_number(value);
            let copied = answer.clone();
            out.push(row(
                self.kind(),
                format!("{SUM_ID}:{typed}"),
                answer,
                Some(t_args("search-answer-sum", &[("expression", typed.into())])),
                self.max_score(),
                move |chrome| {
                    ui::text_selection::clipboard_copy(&copied);
                    chrome.borrow_mut().acknowledge(t("search-answer-copied"));
                    Ok(())
                },
            ));
        }
        if let Some((uri, shown)) = address(typed) {
            out.push(row(
                self.kind(),
                format!("{ADDRESS_ID}:{typed}"),
                t_args("search-answer-open-address", &[("address", shown.into())]),
                None,
                self.max_score(),
                move |chrome| open_uri(&uri, chrome),
            ));
        } else {
            let uri = format!("{WEB_SEARCH}{}", percent_encode(typed));
            out.push(row(
                self.kind(),
                format!("{WEB_ID}:{typed}"),
                t_args("search-answer-web", &[("query", typed.into())]),
                None,
                1,
                move |chrome| open_uri(&uri, chrome),
            ));
        }
        out
    }
}

/// `typed` as an address to open, and how the row names it: a `http(s)`
/// URL, or a bare domain ("example.com", "docs.rs/serde") that opens over
/// https. A file name ("report.pdf") or a sentence is not an address.
fn address(typed: &str) -> Option<(String, String)> {
    if typed.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = typed.to_ascii_lowercase();
    for scheme in ["https://", "http://"] {
        if let Some(rest) = lower.strip_prefix(scheme) {
            let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
            return is_host(host).then(|| (typed.to_owned(), host.to_owned()));
        }
    }
    if lower.contains("://") {
        return None;
    }
    let host = lower.split(['/', '?', '#']).next().unwrap_or_default();
    let tld = host.rsplit('.').next().unwrap_or_default();
    // A domain names a known kind of top-level domain, never a file type.
    let file_type = matches!(
        tld,
        "pdf"
            | "txt"
            | "md"
            | "doc"
            | "docx"
            | "odt"
            | "ods"
            | "xls"
            | "xlsx"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "svg"
            | "zip"
            | "tar"
            | "gz"
            | "rs"
            | "py"
            | "js"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
            | "html"
            | "csv"
            | "mp3"
            | "mp4"
            | "mkv"
            | "sh"
            | "exe"
            | "deb"
            | "rpm"
            | "iso"
            | "log"
            | "conf"
    );
    (is_host(host) && host.contains('.') && !file_type)
        .then(|| (format!("https://{typed}"), host.to_owned()))
}

/// Whether `host` reads as a DNS name: labels of letters, digits and
/// hyphens, a final label of two or more letters.
fn is_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
        && labels.last().is_some_and(|tld| {
            (2..=24).contains(&tld.len()) && tld.chars().all(|c| c.is_ascii_alphabetic())
        })
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else if byte == b' ' {
            out.push('+');
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The value of an arithmetic expression: numbers (a decimal point or, with
/// none, a decimal comma), `+ - * / % ^`, `×`, `÷`, and parentheses. `None`
/// for anything else, for a lone number (nothing to work out), and for a
/// result that is not finite.
pub(crate) fn evaluate(typed: &str) -> Option<f64> {
    if typed.chars().count() > MAX_EXPRESSION_SCALARS {
        return None;
    }
    let decimal_comma = !typed.contains('.');
    let tokens = tokens(typed, decimal_comma)?;
    if !tokens.iter().any(|token| matches!(token, Token::Op(_))) {
        return None;
    }
    let mut parser = Parser { tokens, at: 0 };
    let value = parser.sum()?;
    (parser.at == parser.tokens.len() && value.is_finite()).then_some(value)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token {
    Number(f64),
    Op(char),
    Open,
    Close,
}

fn tokens(typed: &str, decimal_comma: bool) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    let mut chars = typed.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' => {
                chars.next();
            }
            '0'..='9' | '.' | ',' => {
                let mut number = String::new();
                while let Some(&digit) = chars.peek() {
                    match digit {
                        '0'..='9' | '.' => number.push(digit),
                        ',' if decimal_comma => number.push('.'),
                        _ => break,
                    }
                    chars.next();
                }
                out.push(Token::Number(number.parse().ok()?));
            }
            '+' | '-' | '*' | '/' | '%' | '^' => {
                out.push(Token::Op(c));
                chars.next();
            }
            '×' | 'x' => {
                out.push(Token::Op('*'));
                chars.next();
            }
            '÷' | ':' => {
                out.push(Token::Op('/'));
                chars.next();
            }
            '(' => {
                out.push(Token::Open);
                chars.next();
            }
            ')' => {
                out.push(Token::Close);
                chars.next();
            }
            _ => return None,
        }
    }
    Some(out)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.at).copied()
    }

    fn take(&mut self) -> Option<Token> {
        let token = self.peek();
        self.at = self.at.saturating_add(1);
        token
    }

    fn sum(&mut self) -> Option<f64> {
        let mut value = self.product()?;
        while let Some(Token::Op(op @ ('+' | '-'))) = self.peek() {
            self.take();
            let right = self.product()?;
            value = if op == '+' {
                value + right
            } else {
                value - right
            };
        }
        Some(value)
    }

    fn product(&mut self) -> Option<f64> {
        let mut value = self.power()?;
        while let Some(Token::Op(op @ ('*' | '/' | '%'))) = self.peek() {
            self.take();
            let right = self.power()?;
            value = match op {
                '*' => value * right,
                '/' => value / right,
                _ => value % right,
            };
        }
        Some(value)
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if self.peek() == Some(Token::Op('^')) {
            self.take();
            // Right-associative: 2^3^2 is 2^9.
            let exponent = self.power()?;
            return Some(base.powf(exponent));
        }
        Some(base)
    }

    fn unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some(Token::Op('-')) => {
                self.take();
                Some(-self.unary()?)
            }
            Some(Token::Op('+')) => {
                self.take();
                self.unary()
            }
            _ => self.atom(),
        }
    }

    fn atom(&mut self) -> Option<f64> {
        match self.take()? {
            Token::Number(value) => Some(value),
            Token::Open => {
                let value = self.sum()?;
                (self.take()? == Token::Close).then_some(value)
            }
            Token::Op(_) | Token::Close => None,
        }
    }
}

/// A result as a person writes it: at most ten significant digits, which
/// also rounds away what float arithmetic leaves behind (0.1 + 0.2 is 0.3),
/// and no trailing zeros.
fn format_number(value: f64) -> String {
    let rounded: f64 = format!("{value:.9e}").parse().unwrap_or(value);
    let magnitude = rounded.abs();
    if magnitude >= 1e15 || (magnitude > 0.0 && magnitude < 1e-6) {
        format!("{rounded:e}")
    } else {
        format!("{rounded}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_is_worked_out_and_prose_is_not() {
        assert_eq!(evaluate("2+2"), Some(4.0));
        assert_eq!(evaluate("2 + 3 * 4"), Some(14.0));
        assert_eq!(evaluate("(2 + 3) * 4"), Some(20.0));
        assert_eq!(evaluate("2^3^2"), Some(512.0));
        assert_eq!(evaluate("-3 + 1"), Some(-2.0));
        assert_eq!(evaluate("7 / 2"), Some(3.5));
        assert_eq!(evaluate("2,5 * 2"), Some(5.0), "a German decimal comma");
        assert_eq!(evaluate("12 × 3"), Some(36.0));
        assert_eq!(evaluate("42"), None, "a lone number is nothing to work out");
        assert_eq!(evaluate("notes"), None);
        assert_eq!(evaluate("2 +"), None);
        assert_eq!(evaluate("1/0"), None, "not a finite answer");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(4.0), "4");
        assert_eq!(format_number(3.5), "3.5");
        assert_eq!(format_number(1.0 / 3.0), "0.3333333333");
    }

    #[test]
    fn addresses_are_urls_and_domains_never_file_names_or_words() {
        assert_eq!(
            address("https://example.com/a?b=c"),
            Some(("https://example.com/a?b=c".into(), "example.com".into()))
        );
        assert_eq!(
            address("example.com"),
            Some(("https://example.com".into(), "example.com".into()))
        );
        assert_eq!(
            address("en.wikipedia.org/wiki/Tilo"),
            Some((
                "https://en.wikipedia.org/wiki/Tilo".into(),
                "en.wikipedia.org".into()
            ))
        );
        assert_eq!(address("report.pdf"), None);
        assert_eq!(address("main.rs"), None, "a source file, not Serbia");
        assert_eq!(address("weather"), None);
        assert_eq!(address("two words.com"), None);
        assert_eq!(address("ftp://example.com"), None);
        assert_eq!(percent_encode("wetter köln"), "wetter+k%C3%B6ln");
    }
}
