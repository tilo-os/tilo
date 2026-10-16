//! What a review model is shown.
//!
//! A model inside Tilo's own supervised runtime, which has no route off the
//! device, sees the whole trace ([`Exposure::Full`]). Any other model sees
//! declared `Personal` values as typed placeholders ([`Exposure::Placeholders`]):
//! the floor proposes those values already, so such a model has nothing to
//! add about them. A `Personal` text is then shown as its placeholder, and a
//! declared value that also occurs inside another text is replaced there too.
//! Each shown text keeps a map back to the original bytes, so a quote the
//! model makes from what it saw is addressed to the real text, and a quote
//! that reaches into a placeholder is refused.

use crate::{DeclaredSensitivity, RedactionCategory, ReviewTraceRequest, Span, TextOrigin};

/// How much of a trace a model may see, decided by where the model runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exposure {
    /// A supervised runtime on this device with no network egress.
    Full,
    /// Anything else: declared values are replaced by placeholders.
    Placeholders,
}

/// The placeholder a model sees for a hidden value of `category`.
#[must_use]
pub const fn placeholder(category: RedactionCategory) -> &'static str {
    match category {
        RedactionCategory::Name => "\u{2039}name\u{203A}",
        RedactionCategory::Email => "\u{2039}email\u{203A}",
        RedactionCategory::Phone => "\u{2039}phone\u{203A}",
        RedactionCategory::Address => "\u{2039}address\u{203A}",
        RedactionCategory::AccountNumber => "\u{2039}account number\u{203A}",
        RedactionCategory::CredentialLike => "\u{2039}credential\u{203A}",
        RedactionCategory::FreeFormPersonal => "\u{2039}personal\u{203A}",
    }
}

/// Shorter parts of a declared name are hidden too; below this they would
/// hide ordinary words.
const MIN_HIDDEN_CHARS: usize = 3;

/// Every declared value of `request` that must not be shown, with its
/// category, longest first. A declared name also hides each of its words, so
/// "Hi Jana" does not reveal the "Jana Novák" typed into the To field.
pub(crate) fn declared_values(request: &ReviewTraceRequest) -> Vec<(String, RedactionCategory)> {
    let mut values: Vec<(String, RedactionCategory)> = Vec::new();
    let mut add = |value: &str, category| {
        let value = value.trim();
        if value
            .chars()
            .nth(MIN_HIDDEN_CHARS.saturating_sub(1))
            .is_some()
            && !values.iter().any(|(known, _)| known == value)
        {
            values.push((value.to_owned(), category));
        }
    };
    for text in request.steps.iter().flat_map(|step| &step.texts) {
        if text.sensitivity != DeclaredSensitivity::Personal {
            continue;
        }
        let category = text.kind.unwrap_or(RedactionCategory::FreeFormPersonal);
        add(&text.text, category);
        if category == RedactionCategory::Name {
            for word in text.text.split_whitespace() {
                add(word.trim_matches(|c: char| !c.is_alphanumeric()), category);
            }
        }
    }
    values.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
    values
}

/// Non-overlapping byte ranges of `text` holding a declared value, in order.
pub(crate) fn declared_occurrences(
    text: &str,
    values: &[(String, RedactionCategory)],
) -> Vec<(usize, usize, RedactionCategory)> {
    let mut found: Vec<(usize, usize, RedactionCategory)> = Vec::new();
    for (value, category) in values {
        for (start, matched) in text.match_indices(value.as_str()) {
            let end = start.saturating_add(matched.len());
            if !found.iter().any(|(s, e, _)| start < *e && *s < end) {
                found.push((start, end, *category));
            }
        }
    }
    found.sort_unstable_by_key(|(start, _, _)| *start);
    found
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Segment {
    shown_start: usize,
    shown_end: usize,
    original_start: usize,
    original_end: usize,
    hidden: bool,
}

/// One text as a model sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelText {
    pub step: u32,
    pub node: String,
    pub sensitivity: DeclaredSensitivity,
    pub origin: TextOrigin,
    /// The text with every declared value replaced by its placeholder.
    pub shown: String,
    segments: Vec<Segment>,
}

impl ModelText {
    /// Whether anything of the original is visible, so a model may quote it.
    #[must_use]
    pub fn quotable(&self) -> bool {
        self.segments.iter().any(|segment| !segment.hidden)
    }

    /// The original span behind `shown_start..shown_end` of [`Self::shown`].
    /// A range may take in a whole placeholder, and then covers the value it
    /// hides; it is `None` when either end lies inside a placeholder, or the
    /// range is empty or out of bounds.
    #[must_use]
    pub fn original_span(&self, shown_start: usize, shown_end: usize) -> Option<Span> {
        if shown_start >= shown_end || shown_end > self.shown.len() {
            return None;
        }
        let start = self.segments.iter().find(|segment| {
            segment.shown_start <= shown_start && shown_start < segment.shown_end
        })?;
        let end = self
            .segments
            .iter()
            .find(|segment| segment.shown_start < shown_end && shown_end <= segment.shown_end)?;
        let start = if start.hidden {
            (start.shown_start == shown_start).then_some(start.original_start)?
        } else {
            start
                .original_start
                .saturating_add(shown_start.saturating_sub(start.shown_start))
        };
        let end = if end.hidden {
            (end.shown_end == shown_end).then_some(end.original_end)?
        } else {
            end.original_start
                .saturating_add(shown_end.saturating_sub(end.shown_start))
        };
        Some(Span {
            start: u32::try_from(start).ok()?,
            end: u32::try_from(end).ok()?,
        })
    }
}

fn shown_text(text: &str, hide: &[(usize, usize, RedactionCategory)]) -> (String, Vec<Segment>) {
    let mut shown = String::with_capacity(text.len());
    let mut segments = Vec::new();
    let mut cursor = 0;
    let visible = |shown: &mut String, segments: &mut Vec<Segment>, from: usize, to: usize| {
        if let Some(part) = text.get(from..to).filter(|part| !part.is_empty()) {
            segments.push(Segment {
                shown_start: shown.len(),
                shown_end: shown.len().saturating_add(part.len()),
                original_start: from,
                original_end: to,
                hidden: false,
            });
            shown.push_str(part);
        }
    };
    for (start, end, category) in hide {
        visible(&mut shown, &mut segments, cursor, *start);
        let mask = placeholder(*category);
        segments.push(Segment {
            shown_start: shown.len(),
            shown_end: shown.len().saturating_add(mask.len()),
            original_start: *start,
            original_end: *end,
            hidden: true,
        });
        shown.push_str(mask);
        cursor = *end;
    }
    visible(&mut shown, &mut segments, cursor, text.len());
    (shown, segments)
}

/// Every text of `request`, in request order, as a model with `exposure`
/// may see it.
#[must_use]
pub fn model_view(request: &ReviewTraceRequest, exposure: Exposure) -> Vec<ModelText> {
    let values = match exposure {
        Exposure::Full => Vec::new(),
        Exposure::Placeholders => declared_values(request),
    };
    let mut view = Vec::new();
    for (index, step) in request.steps.iter().enumerate() {
        let Ok(step_index) = u32::try_from(index) else {
            continue;
        };
        for text in &step.texts {
            let hide = if exposure == Exposure::Placeholders
                && text.sensitivity == DeclaredSensitivity::Personal
            {
                vec![(
                    0,
                    text.text.len(),
                    text.kind.unwrap_or(RedactionCategory::FreeFormPersonal),
                )]
            } else {
                declared_occurrences(&text.text, &values)
            };
            let (shown, segments) = shown_text(&text.text, &hide);
            view.push(ModelText {
                step: step_index,
                node: text.node.clone(),
                sensitivity: text.sensitivity,
                origin: text.origin,
                shown,
                segments,
            });
        }
    }
    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StepKind;
    use crate::fixtures::{request, step, text};

    fn mail() -> ReviewTraceRequest {
        let mut to = text("r:to", DeclaredSensitivity::Personal, "Jana Novák");
        to.kind = Some(RedactionCategory::Name);
        request(vec![step(
            StepKind::Edited,
            "Message",
            vec![
                to,
                text("r:note", DeclaredSensitivity::Personal, "my diagnosis"),
                text(
                    "r:body",
                    DeclaredSensitivity::Public,
                    "Hi Jana, Jana Novák said: my diagnosis. Call Petr",
                ),
            ],
        )])
    }

    #[test]
    fn declared_values_never_appear_in_what_a_model_sees() {
        let view = model_view(&mail(), Exposure::Placeholders);
        let shown: Vec<&str> = view.iter().map(|text| text.shown.as_str()).collect();
        assert_eq!(
            shown,
            [
                "\u{2039}name\u{203A}",
                "\u{2039}personal\u{203A}",
                "Hi \u{2039}name\u{203A}, \u{2039}name\u{203A} said: \u{2039}personal\u{203A}. Call Petr",
            ]
        );
        for text in &view {
            for secret in ["Jana", "Novák", "diagnosis"] {
                assert!(
                    !text.shown.contains(secret),
                    "{secret} reached {}",
                    text.node
                );
            }
        }
        assert!(!view[0].quotable());
        assert!(view[2].quotable());
    }

    #[test]
    fn a_supervised_local_model_sees_the_whole_trace() {
        let request = mail();
        let view = model_view(&request, Exposure::Full);
        for (shown, original) in view.iter().zip(request.steps[0].texts.iter()) {
            assert_eq!(shown.shown, original.text);
            assert_eq!(shown.original_span(0, 2), Some(Span { start: 0, end: 2 }));
        }
    }

    #[test]
    fn a_quote_maps_back_to_the_original_bytes_and_never_into_a_placeholder() {
        let request = mail();
        let body = &model_view(&request, Exposure::Placeholders)[2];
        let petr = body.shown.find("Petr").unwrap();
        let span = body.original_span(petr, petr + 4).unwrap();
        let original = &request.steps[0].texts[2].text;
        assert_eq!(
            original.get(span.start as usize..span.end as usize),
            Some("Petr")
        );

        let hi = body.original_span(0, 2).unwrap();
        assert_eq!(hi, Span { start: 0, end: 2 });

        let name = body.shown.find('\u{2039}').unwrap();
        assert_eq!(body.original_span(name, name + 3), None);
        assert_eq!(body.original_span(0, name + 3), None);
        // Spanning a whole placeholder addresses the original value it hides.
        let comma = body.shown.find(',').unwrap();
        let spanning = body.original_span(0, comma).unwrap();
        assert_eq!(
            original.get(spanning.start as usize..spanning.end as usize),
            Some("Hi Jana")
        );
    }
}
