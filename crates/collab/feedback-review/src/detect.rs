//! The deterministic floor: what review proposes with no model at all.
//!
//! Declared `Personal` values are proposed whole. Every text is then scanned
//! by pattern detectors whose matches are checked where a checksum exists
//! (Luhn for cards, ISO 7064 mod 97 for IBANs), so a digit run that merely
//! looks like a number is not reported as one with high confidence.

use std::sync::LazyLock;

use regex_lite::Regex;

use crate::view::{declared_occurrences, declared_values};
use crate::{
    Confidence, DeclaredSensitivity, RedactionBasis, RedactionCategory, RedactionProposal,
    ReviewText, ReviewTraceRequest, Span,
};

/// A pattern match inside one text, before it is addressed to a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub start: usize,
    pub end: usize,
    pub category: RedactionCategory,
    pub confidence: u8,
}

macro_rules! pattern {
    ($name:ident, $source:expr) => {
        static $name: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new($source).ok());
    };
}

pattern!(
    EMAIL,
    r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9](?:[A-Za-z0-9\-]*[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9\-]*[A-Za-z0-9])?)*\.[A-Za-z]{2,}"
);
pattern!(URL, r#"(?i)\bhttps?://[^\s<>"'`]+"#);
pattern!(BEARER, r"(?i)\bbearer\s+([A-Za-z0-9._~+/\-]{8,}=*)");
pattern!(
    JWT,
    r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]*"
);
pattern!(
    KEYED_SECRET,
    r"\b(?:sk|pk|rk)-[A-Za-z0-9_\-]{16,}|\bgh[pousr]_[A-Za-z0-9]{20,}|\bAKIA[0-9A-Z]{16}\b|\bxox[abprs]-[A-Za-z0-9\-]{10,}|\bglpat-[A-Za-z0-9_\-]{20,}"
);
pattern!(
    ASSIGNED_SECRET,
    r"(?i)\b(?:password|passwd|pwd|secret|api[_\-]?key|token|access[_\-]?token)\s*[:=]\s*([^\s,;&#]{4,})"
);
pattern!(OPAQUE, r"[A-Za-z0-9_\-+/]{32,}={0,2}");
pattern!(IBAN, r"\b[A-Z]{2}[0-9]{2}(?: ?[A-Z0-9]){11,30}\b");
pattern!(DIGIT_RUN, r"\b[0-9](?:[ \-]?[0-9]){11,22}\b");
pattern!(PHONE, r"(?:\+|\()?\b[0-9][0-9 ().\-/]{5,}[0-9]\b");

/// URL query or fragment keys whose values are credentials.
const SECRET_KEYS: &[&str] = &[
    "access_token",
    "api_key",
    "apikey",
    "auth",
    "code",
    "id_token",
    "jwt",
    "key",
    "password",
    "pwd",
    "refresh_token",
    "secret",
    "session",
    "sessionid",
    "sid",
    "sig",
    "signature",
    "token",
    "x-amz-credential",
    "x-amz-signature",
    "x-goog-signature",
];

fn matches(pattern: &LazyLock<Option<Regex>>, text: &str) -> Vec<(usize, usize)> {
    pattern.as_ref().map_or_else(Vec::new, |regex| {
        regex
            .find_iter(text)
            .map(|m| (m.start(), m.end()))
            .collect()
    })
}

fn captures(pattern: &LazyLock<Option<Regex>>, text: &str) -> Vec<(usize, usize)> {
    pattern.as_ref().map_or_else(Vec::new, |regex| {
        regex
            .captures_iter(text)
            .filter_map(|caps| caps.get(1).map(|m| (m.start(), m.end())))
            .collect()
    })
}

fn digits(value: &str) -> Vec<u32> {
    value.chars().filter_map(|c| c.to_digit(10)).collect()
}

/// The Luhn check that every payment-card number satisfies.
pub(crate) fn luhn(number: &[u32]) -> bool {
    let sum = number
        .iter()
        .rev()
        .enumerate()
        .fold(0_u32, |sum, (index, digit)| {
            let value = if index % 2 == 1 {
                let doubled = digit.saturating_mul(2);
                if doubled > 9 {
                    doubled.saturating_sub(9)
                } else {
                    doubled
                }
            } else {
                *digit
            };
            sum.saturating_add(value)
        });
    !number.is_empty() && sum % 10 == 0
}

/// ISO 13616 IBAN check: move the first four characters to the end, map
/// letters to 10..=35, and the number must be 1 mod 97.
pub(crate) fn iban_valid(candidate: &str) -> bool {
    let compact: Vec<char> = candidate.chars().filter(|c| !c.is_whitespace()).collect();
    if !(15..=34).contains(&compact.len()) {
        return false;
    }
    let (head, tail) = compact.split_at(4);
    tail.iter().chain(head).try_fold(0_u32, |remainder, c| {
        let value = c.to_digit(36)?;
        let shifted = if value >= 10 {
            remainder.checked_mul(100)?.checked_add(value)?
        } else {
            remainder.checked_mul(10)?.checked_add(value)?
        };
        Some(shifted % 97)
    }) == Some(1)
}

fn has_letter_and_digit(value: &str) -> bool {
    value.chars().any(|c| c.is_ascii_alphabetic()) && value.chars().any(|c| c.is_ascii_digit())
}

fn url_secrets(text: &str, start: usize, end: usize, hits: &mut Vec<Hit>) {
    let Some(url) = text.get(start..end) else {
        return;
    };
    // `scheme://user:password@host` (example shape) carries its password in the authority.
    if let Some(authority_start) = url.find("://").map(|index| index.saturating_add(3)) {
        let authority_end = url
            .get(authority_start..)
            .and_then(|rest| rest.find(['/', '?', '#']))
            .map_or(url.len(), |index| authority_start.saturating_add(index));
        if let Some(authority) = url.get(authority_start..authority_end)
            && let Some(at) = authority.rfind('@')
            && let Some(colon) = authority.get(..at).and_then(|userinfo| userinfo.find(':'))
            && colon.saturating_add(1) < at
        {
            hits.push(Hit {
                start: start
                    .saturating_add(authority_start)
                    .saturating_add(colon)
                    .saturating_add(1),
                end: start.saturating_add(authority_start).saturating_add(at),
                category: RedactionCategory::CredentialLike,
                confidence: 95,
            });
        }
    }
    // Query and fragment parameters, located in the original bytes rather
    // than a normalized parse, so the span points at what was on screen.
    let Some(parameters_start) = url.find(['?', '#']) else {
        return;
    };
    let mut offset = parameters_start.saturating_add(1);
    let Some(parameters) = url.get(offset..) else {
        return;
    };
    for pair in parameters.split(['&', '#', ';']) {
        if let Some(equals) = pair.find('=') {
            let key = pair.get(..equals).unwrap_or_default().to_ascii_lowercase();
            let value_len = pair.len().saturating_sub(equals).saturating_sub(1);
            let secret_key = SECRET_KEYS.contains(&key.as_str())
                || key.ends_with("_token")
                || key.ends_with("token");
            if value_len > 0 && secret_key {
                let value_start = start
                    .saturating_add(offset)
                    .saturating_add(equals)
                    .saturating_add(1);
                hits.push(Hit {
                    start: value_start,
                    end: value_start.saturating_add(value_len),
                    category: RedactionCategory::CredentialLike,
                    confidence: 90,
                });
            }
        }
        offset = offset.saturating_add(pair.len()).saturating_add(1);
    }
}

fn phone_like(candidate: &str) -> Option<u8> {
    let count = digits(candidate).len();
    if !(7..=15).contains(&count) {
        return None;
    }
    let international = candidate.starts_with('+') || candidate.starts_with("00");
    // Dates, times, versions and decimals are digit runs too; only accept
    // separators that phone numbers are written with.
    let iso_date = candidate.len() == 10
        && candidate.as_bytes().get(4) == Some(&b'-')
        && candidate.as_bytes().get(7) == Some(&b'-');
    if iso_date || candidate.contains('/') && !international {
        return None;
    }
    if candidate.contains('.') && !international {
        return None;
    }
    if international {
        Some(85)
    } else if count >= 9 {
        Some(65)
    } else {
        None
    }
}

/// Pattern hits in one text, with contained lower-confidence hits removed.
pub(crate) fn scan(text: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut push = |start, end, category, confidence| {
        hits.push(Hit {
            start,
            end,
            category,
            confidence,
        });
    };
    let urls = matches(&URL, text);
    for (start, end) in matches(&EMAIL, text) {
        // `user:password@host` inside a URL is userinfo, not an address.
        if !urls
            .iter()
            .any(|(url_start, url_end)| *url_start <= start && start < *url_end)
        {
            push(start, end, RedactionCategory::Email, 95);
        }
    }
    for (start, end) in captures(&BEARER, text) {
        push(start, end, RedactionCategory::CredentialLike, 95);
    }
    for (start, end) in matches(&JWT, text) {
        push(start, end, RedactionCategory::CredentialLike, 95);
    }
    for (start, end) in matches(&KEYED_SECRET, text) {
        push(start, end, RedactionCategory::CredentialLike, 95);
    }
    for (start, end) in captures(&ASSIGNED_SECRET, text) {
        push(start, end, RedactionCategory::CredentialLike, 90);
    }
    for (start, end) in matches(&OPAQUE, text) {
        if text.get(start..end).is_some_and(has_letter_and_digit) {
            push(start, end, RedactionCategory::CredentialLike, 70);
        }
    }
    for (start, end) in matches(&IBAN, text) {
        if text.get(start..end).is_some_and(iban_valid) {
            push(start, end, RedactionCategory::AccountNumber, 95);
        }
    }
    for (start, end) in matches(&DIGIT_RUN, text) {
        let number = digits(text.get(start..end).unwrap_or_default());
        if (13..=19).contains(&number.len()) && luhn(&number) {
            push(start, end, RedactionCategory::AccountNumber, 90);
        } else {
            push(start, end, RedactionCategory::AccountNumber, 55);
        }
    }
    for (start, end) in matches(&PHONE, text) {
        if let Some(confidence) = text.get(start..end).and_then(phone_like) {
            push(start, end, RedactionCategory::Phone, confidence);
        }
    }
    for (start, end) in urls {
        url_secrets(text, start, end, &mut hits);
    }
    // A shorter reading inside a stronger one (a digit run inside a card, a
    // token inside a URL parameter) adds nothing; keep the stronger.
    hits.sort_by_key(|hit| {
        (
            hit.start,
            std::cmp::Reverse(hit.end),
            std::cmp::Reverse(hit.confidence),
        )
    });
    let mut kept: Vec<Hit> = Vec::with_capacity(hits.len());
    for hit in hits {
        let covered = kept.iter().any(|other| {
            other.start <= hit.start && hit.end <= other.end && other.confidence >= hit.confidence
        });
        if !covered {
            kept.retain(|other| {
                !(hit.start <= other.start
                    && other.end <= hit.end
                    && hit.confidence > other.confidence)
            });
            kept.push(hit);
        }
    }
    kept
}

fn span(start: usize, end: usize) -> Option<Span> {
    Some(Span {
        start: u32::try_from(start).ok()?,
        end: u32::try_from(end).ok()?,
    })
}

fn text_proposals(
    step: u32,
    text: &ReviewText,
    declared: &[(String, RedactionCategory)],
) -> Vec<RedactionProposal> {
    let proposal = |span, category, confidence, basis| RedactionProposal {
        step,
        node: text.node.clone(),
        span,
        category,
        confidence,
        basis,
    };
    if text.sensitivity == DeclaredSensitivity::Personal {
        // The whole value is declared personal; a pattern inside it adds
        // nothing a reviewer needs.
        return span(0, text.text.len())
            .map(|whole| {
                proposal(
                    whole,
                    text.kind.unwrap_or(RedactionCategory::FreeFormPersonal),
                    Confidence::CERTAIN,
                    RedactionBasis::Declared,
                )
            })
            .into_iter()
            .collect();
    }
    // A declared value repeated in an undeclared text is still that value.
    let repeated = declared_occurrences(&text.text, declared)
        .into_iter()
        .filter_map(|(start, end, category)| {
            Some(proposal(
                span(start, end)?,
                category,
                Confidence::CERTAIN,
                RedactionBasis::Declared,
            ))
        });
    let patterns = scan(&text.text).into_iter().filter_map(|hit| {
        Some(proposal(
            span(hit.start, hit.end)?,
            hit.category,
            Confidence::new(hit.confidence).ok()?,
            RedactionBasis::Pattern,
        ))
    });
    repeated.chain(patterns).collect()
}

/// The deterministic floor for `request`: declared `Personal` values, whole
/// and wherever else they occur, and pattern matches in everything else.
/// Titles come from [`crate::merge`].
#[must_use]
pub fn floor(request: &ReviewTraceRequest) -> Vec<RedactionProposal> {
    let declared = declared_values(request);
    request
        .steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| Some((u32::try_from(index).ok()?, step)))
        .flat_map(|(index, step)| {
            let declared = &declared;
            step.texts
                .iter()
                .flat_map(move |text| text_proposals(index, text, declared))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StepKind;
    use crate::fixtures::{request, step, text};

    fn found(text: &str) -> Vec<(&str, RedactionCategory)> {
        scan(text)
            .into_iter()
            .map(|hit| (text.get(hit.start..hit.end).unwrap(), hit.category))
            .collect()
    }

    #[test]
    fn emails_are_found_with_their_exact_span() {
        assert_eq!(
            found("write to jana.novak+work@mail.example.cz today"),
            [("jana.novak+work@mail.example.cz", RedactionCategory::Email)]
        );
    }

    #[test]
    fn phone_numbers_are_found_but_dates_and_versions_are_not() {
        assert_eq!(
            found("call +49 30 1234 5678 now"),
            [("+49 30 1234 5678", RedactionCategory::Phone)]
        );
        assert_eq!(
            found("(030) 1234-5678"),
            [("(030) 1234-5678", RedactionCategory::Phone)]
        );
        assert!(found("on 2026-09-26 at 10:30").is_empty());
        assert!(found("version 1.24.3").is_empty());
        assert!(found("room 1234").is_empty());
    }

    #[test]
    fn card_numbers_need_a_luhn_checksum_to_be_confident() {
        let hits = scan("card 4111 1111 1111 1111 exp");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits.first().map(|hit| hit.confidence), Some(90));
        let near_miss = scan("card 4111 1111 1111 1112 exp");
        assert_eq!(near_miss.first().map(|hit| hit.confidence), Some(55));
        assert!(luhn(&digits("79927398713")));
        assert!(!luhn(&digits("79927398710")));
    }

    #[test]
    fn ibans_need_their_mod_97_check() {
        assert_eq!(
            found("pay DE89 3704 0044 0532 0130 00 please"),
            [(
                "DE89 3704 0044 0532 0130 00",
                RedactionCategory::AccountNumber
            )]
        );
        assert!(iban_valid("GB82WEST12345698765432"));
        assert!(!iban_valid("GB82WEST12345698765433"));
    }

    #[test]
    fn url_credentials_are_redacted_but_the_rest_of_the_url_is_kept() {
        let text = "open https://files.example.com/share/doc?id=7&token=abc123XYZ&view=1 now";
        assert_eq!(
            found(text),
            [("abc123XYZ", RedactionCategory::CredentialLike)]
        );
        let userinfo = "https://ana:s3cr3t@dav.example.org/cal";
        assert_eq!(
            found(userinfo),
            [("s3cr3t", RedactionCategory::CredentialLike)]
        );
        let fragment = "https://app.example.com/cb#access_token=zzTop42&state=x";
        assert_eq!(
            found(fragment),
            [("zzTop42", RedactionCategory::CredentialLike)]
        );
        assert!(found("https://tilo.so/docs?page=2").is_empty());
    }

    #[test]
    fn credential_shapes_are_found() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"; // example token
        assert_eq!(
            found(jwt).first().map(|(_, c)| *c),
            Some(RedactionCategory::CredentialLike)
        );
        assert_eq!(
            found("Authorization: Bearer 0123abcdEFGH4567"),
            [("0123abcdEFGH4567", RedactionCategory::CredentialLike)]
        );
        assert_eq!(
            found("password = correcthorse"),
            [("correcthorse", RedactionCategory::CredentialLike)]
        );
        assert_eq!(
            found("key sk-proj-abcdefghijklmnop1234"),
            [(
                "sk-proj-abcdefghijklmnop1234",
                RedactionCategory::CredentialLike
            )]
        );
        // Long ordinary words are not tokens.
        assert!(found("Donaudampfschifffahrtsgesellschaftskapitaen").is_empty());
    }

    #[test]
    fn plain_prose_proposes_nothing() {
        assert!(found("You pressed Send and nothing happened.").is_empty());
    }

    #[test]
    fn a_declared_name_is_proposed_wherever_it_is_repeated() {
        let mut to = text("r:to", DeclaredSensitivity::Personal, "Jana Novák");
        to.kind = Some(RedactionCategory::Name);
        let request = request(vec![
            step(StepKind::Edited, "To", vec![to]),
            step(
                StepKind::Appeared,
                "Sent",
                vec![text("r:toast", DeclaredSensitivity::Public, "Sent to Jana")],
            ),
        ]);
        let proposals = floor(&request);
        assert_eq!(proposals[0].category, RedactionCategory::Name);
        let repeated = proposals.iter().find(|p| p.node == "r:toast").unwrap();
        assert_eq!(repeated.basis, RedactionBasis::Declared);
        assert_eq!(request.redacted(repeated), Some("Jana"));
    }

    #[test]
    fn declared_personal_values_are_proposed_whole_and_certain() {
        let request = request(vec![step(
            StepKind::Edited,
            "Type",
            vec![
                text("r:to", DeclaredSensitivity::Personal, "Jana Novák"),
                text(
                    "r:subject",
                    DeclaredSensitivity::Public,
                    "mail jana@example.cz",
                ),
            ],
        )]);
        let proposals = floor(&request);
        assert_eq!(proposals.len(), 2);
        let declared = proposals.iter().find(|p| p.node == "r:to").unwrap();
        assert_eq!(declared.basis, RedactionBasis::Declared);
        assert_eq!(declared.confidence, Confidence::CERTAIN);
        assert_eq!(request.redacted(declared), Some("Jana Novák"));
        let pattern = proposals.iter().find(|p| p.node == "r:subject").unwrap();
        assert_eq!(pattern.category, RedactionCategory::Email);
        assert_eq!(request.redacted(pattern), Some("jana@example.cz"));
    }
}
