//! Identifiers of every service, written as URIs (search.md section 2.2.1): `mailto:jane@x.com`,
//! `tel:+4930123456`, `discord:janedoe`, `matrix:@jane:matrix.org`, `tilo:jane`. A
//! `std.Identifier` is held as that text, its scheme and its value. Search knows no service: a
//! new one is a new scheme.
//!
//! Bare forms are accepted where they are unambiguous: `jane@x.com` is a `mailto:`, `+4930…` a
//! `tel:`. `*` is the one pattern: `*@x.com`, `tel:+49*`, `discord:*`. Identifiers compare
//! without regard to case.

/// The identifier `text` as it is held, `scheme:value`, from a URI or a bare form.
///
/// # Errors
/// A message saying how to write it, when it is neither.
pub fn normalize(text: &str) -> Result<String, String> {
    let held = written(text, false)?;
    Ok(held)
}

/// The pattern `text` as it is matched: an identifier that may hold `*`, a bare form taken as
/// its scheme's, and `@x.com` as `mailto:*@x.com`.
///
/// # Errors
/// A message saying how to write it, when it is neither an identifier nor a pattern of one.
pub fn normalize_pattern(text: &str) -> Result<String, String> {
    written(text, true)
}

fn written(text: &str, pattern: bool) -> Result<String, String> {
    let text = text.trim();
    let how = || {
        format!(
            "`{text}` is not an identifier: write it as a URI such as `mailto:jane@example.com`, \
             `tel:+4930123456` or `discord:janedoe`, or as a bare mail address or phone number"
        )
    };
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return Err(how());
    }
    if !pattern && text.contains('*') {
        return Err(format!(
            "`{text}` is a pattern; an identifier names one account, with no `*`"
        ));
    }
    if let Some((scheme, value)) = text.split_once(':')
        && is_scheme(scheme)
        && (!text.contains('@') || text.find(':') < text.find('@'))
    {
        if value.is_empty() {
            return Err(how());
        }
        let scheme = scheme.to_ascii_lowercase();
        let value = if scheme == "tel" {
            phone(value)
        } else {
            value.to_lowercase()
        };
        return Ok(format!("{scheme}:{value}"));
    }
    if pattern && let Some(domain) = text.strip_prefix('@') {
        return Ok(format!("mailto:*@{}", domain.to_lowercase()));
    }
    if text.contains('@') && !text.starts_with('@') {
        return Ok(format!("mailto:{}", text.to_lowercase()));
    }
    if let Some(number) = text.strip_prefix('+')
        && !number.is_empty()
        && number
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | '(' | ')' | '.' | '*'))
    {
        return Ok(format!("tel:{}", phone(text)));
    }
    if pattern && text == "*" {
        return Ok("*".to_owned());
    }
    Err(how())
}

/// A scheme as URIs write one: a letter, then letters, digits, `+`, `-` or `.`.
fn is_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// A phone number without what people write between its digits.
fn phone(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '-' | '(' | ')' | '.'))
        .collect()
}

/// The scheme and the value of a held identifier.
#[must_use]
pub fn parts(held: &str) -> (&str, &str) {
    held.split_once(':').unwrap_or(("", held))
}

/// Whether the held identifier `id` matches the normalized pattern `pattern`, in which each `*`
/// stands for any text.
#[must_use]
pub fn matches(pattern: &str, id: &str) -> bool {
    let id = id.to_lowercase();
    let pattern = pattern.to_lowercase();
    let mut pieces = pattern.split('*');
    let Some(first) = pieces.next() else {
        return true;
    };
    let Some(mut rest) = id.strip_prefix(first) else {
        return false;
    };
    let pieces: Vec<&str> = pieces.collect();
    let Some((last, middle)) = pieces.split_last() else {
        return rest.is_empty();
    };
    for piece in middle {
        match rest.split_once(piece) {
            Some((_, after)) => rest = after,
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// What every identifier a pattern matches starts with: its text before the first `*`.
#[must_use]
pub fn prefix(pattern: &str) -> &str {
    pattern.split('*').next().unwrap_or(pattern)
}

/// What every identifier a pattern matches ends with: its text after the last `*` (all of it
/// for a pattern with no `*`).
#[must_use]
pub fn suffix(pattern: &str) -> &str {
    pattern.rsplit('*').next().unwrap_or(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_held_as_uris_from_their_written_and_bare_forms() {
        assert_eq!(
            normalize("mailto:Jane@YCombinator.com").as_deref(),
            Ok("mailto:jane@ycombinator.com")
        );
        assert_eq!(
            normalize("jane@ycombinator.com").as_deref(),
            Ok("mailto:jane@ycombinator.com")
        );
        assert_eq!(normalize("+49 30").ok(), None, "a space is no identifier");
        assert_eq!(normalize("+49-30-123").as_deref(), Ok("tel:+4930123"));
        assert_eq!(
            normalize("matrix:@jane:matrix.org").as_deref(),
            Ok("matrix:@jane:matrix.org")
        );
        assert_eq!(
            normalize("discord:JaneDoe").as_deref(),
            Ok("discord:janedoe")
        );
        assert!(normalize("janedoe").is_err());
        assert!(normalize("*@x.com").is_err());
        assert_eq!(parts("tel:+4930"), ("tel", "+4930"));
    }

    #[test]
    fn a_star_is_the_one_pattern() {
        let domain = normalize_pattern("*@ycombinator.com").unwrap();
        assert_eq!(domain, "mailto:*@ycombinator.com");
        assert_eq!(normalize_pattern("@ycombinator.com").unwrap(), domain);
        assert!(matches(&domain, "mailto:jane@ycombinator.com"));
        assert!(!matches(&domain, "mailto:jane@ycombinator.com.evil"));
        assert!(!matches(&domain, "tel:+49"));
        let german = normalize_pattern("tel:+49*").unwrap();
        assert!(matches(&german, "tel:+4930123"));
        assert!(!matches(&german, "tel:+4430123"));
        assert!(matches("discord:*", "discord:janedoe"));
        assert!(matches("*", "tel:+1"));
        assert!(matches("mailto:j*e@*.com", "mailto:jane@x.com"));
        assert_eq!(prefix("tel:+49*"), "tel:+49");
        assert_eq!(suffix("mailto:*@x.com"), "@x.com");
        assert_eq!(suffix("tel:+49"), "tel:+49");
    }
}
