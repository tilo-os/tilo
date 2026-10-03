//! Script fallbacks matched to the UI face, and the features a text kind
//! resolves to.
//!
//! cosmic-text falls back per glyph to whatever face covers a character, at
//! the size and weight the run asked for. That is right for Arabic or CJK,
//! which have no x-height to match, but Greek, Cyrillic and Vietnamese set
//! that way sit visibly smaller or larger, lighter or heavier, than the Latin
//! around them. Before shaping, every run is split here into spans the
//! shaper can set as one face:
//!
//! - a word in a script the run's face lacks goes to the face matched for
//!   that script ([`MATCHED`]), at the size that gives it the primary face's
//!   x-height and at the weight that looks like the primary's;
//! - punctuation stays with the word it closes or opens; spaces, digits
//!   and dashes between two faces are the primary face's;
//! - every span gets the OpenType features its [`TextKind`] asks for that
//!   its own face has.
//!
//! The split is a pure function of the text, the style and the faces, run on
//! the one shaping path that both measurement and painting use, so the two
//! can never disagree.

use std::sync::Arc;

use cosmic_text::{
    Align, Attrs, AttrsList, Buffer, BufferLine, Family, FeatureTag, FontFeatures, LineEnding,
    Metrics, Shaping, Style, Weight, fontdb,
};
use unicode_properties::general_category::{GeneralCategory, UnicodeGeneralCategory};
use unicode_script::{Script, UnicodeScript};

use super::faces::{FaceCaps, caps};
use crate::text_render::TextKind;

/// One face matched to the UI face for scripts it lacks.
struct Matched {
    scripts: &'static [Script],
    family: &'static str,
    /// Features that bring the face's shapes closer to the UI face's.
    features: &'static [[u8; 4]],
    /// The fallback weight for a UI weight `w` is `scale * w + offset`,
    /// clamped to the face's axis, so the stems look as heavy as the UI
    /// face's at every weight.
    scale: f32,
    offset: f32,
    /// Whether the face has an italic: an upright-only face is skipped for
    /// italic text, which keeps a true italic from a later entry.
    italic: bool,
}

/// Matched fallbacks; for each script the first usable entry wins. A face is
/// usable when the font system has it in the run's style and it covers the
/// word.
const MATCHED: [Matched; 3] = [
    // Onest's straight-armed forms (`ss02`) are the closest Cyrillic to
    // Instrument Sans; at the same x-height its stems match at about 1.2x
    // the weight above 400.
    Matched {
        scripts: &[Script::Cyrillic],
        family: "Onest",
        features: &[*b"ss02"],
        scale: 1.2,
        offset: -90.0,
        italic: false,
    },
    // Geologica is far heavier than its axis value suggests: at the same
    // x-height its stems match Instrument Sans' at about 215 for Regular,
    // 350 for Medium, 480 for SemiBold and 615 for Bold.
    Matched {
        scripts: &[Script::Greek],
        family: "Geologica",
        features: &[],
        scale: 1.33,
        offset: -317.0,
        italic: false,
    },
    // Italic Greek and Cyrillic, Vietnamese, and any other Latin letters the
    // UI face lacks.
    Matched {
        scripts: &[Script::Cyrillic, Script::Greek, Script::Latin],
        family: "Noto Sans",
        features: &[],
        scale: 1.0,
        offset: 0.0,
        italic: true,
    },
];

/// Which face a stretch of text is set in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Face {
    Primary,
    Matched(usize),
}

/// Each [`MATCHED`] entry resolved against one run's style: `None` until
/// asked for, then the face's capabilities when the font system has it.
type Resolved = Vec<Option<Option<Arc<FaceCaps>>>>;

/// Split `spans` into spans of one face each, with features resolved for
/// `kind`. `base` is the buffer's own metrics, used for spans that carry
/// none.
pub(crate) fn plan<'t, 'a>(
    db: &fontdb::Database,
    spans: impl IntoIterator<Item = (&'t str, Attrs<'a>)>,
    base: Metrics,
    kind: TextKind,
) -> Vec<(&'t str, Attrs<'a>)> {
    let mut out = Vec::new();
    for (text, attrs) in spans {
        plan_span(db, text, &attrs, base, kind, &mut out);
    }
    out
}

fn query(db: &fontdb::Database, attrs: &Attrs<'_>) -> Option<fontdb::ID> {
    db.query(&fontdb::Query {
        families: &[attrs.family],
        weight: attrs.weight,
        stretch: attrs.stretch,
        style: attrs.style,
    })
}

fn plan_span<'t, 'a>(
    db: &fontdb::Database,
    text: &'t str,
    attrs: &Attrs<'a>,
    base: Metrics,
    kind: TextKind,
    out: &mut Vec<(&'t str, Attrs<'a>)>,
) {
    // Most UI text: nothing to split and no features to choose.
    if kind == TextKind::Prose && text.is_ascii() {
        out.push((text, attrs.clone()));
        return;
    }
    let primary = (!text.is_empty())
        .then(|| query(db, attrs))
        .flatten()
        .and_then(|id| caps(db, id));
    let Some(primary) = primary else {
        out.push((text, attrs.clone()));
        return;
    };
    let primary_attrs = with_features(attrs, &[], &primary.kind_features(kind), true);
    if primary.monospaced || text.is_ascii() {
        out.push((text, primary_attrs));
        return;
    }
    let (faces, resolved) = assign_faces(db, text, attrs, &primary);
    let metrics = attrs.metrics_opt.map_or(base, Metrics::from);
    let attrs_for = |face: Face| match face {
        Face::Primary => primary_attrs.clone(),
        Face::Matched(index) => {
            let fallback = resolved.get(index).cloned().flatten().flatten();
            match (fallback, MATCHED.get(index)) {
                (Some(fallback), Some(entry)) => {
                    fallback_attrs(attrs, entry, &primary, &fallback, metrics, kind)
                }
                _ => primary_attrs.clone(),
            }
        }
    };
    let mut start = 0;
    let mut current = faces.first().map_or(Face::Primary, |(_, face)| *face);
    for &(offset, face) in &faces {
        if face != current {
            if let Some(slice) = text.get(start..offset) {
                out.push((slice, attrs_for(current)));
            }
            start = offset;
            current = face;
        }
    }
    if let Some(slice) = text.get(start..) {
        out.push((slice, attrs_for(current)));
    }
}

/// The face of every character of `text`, as `(byte offset, face)`.
fn assign_faces(
    db: &fontdb::Database,
    text: &str,
    attrs: &Attrs<'_>,
    primary: &FaceCaps,
) -> (Vec<(usize, Face)>, Resolved) {
    // Words: a script's letters plus the marks that follow them. Everything
    // else (spaces, punctuation, digits, symbols) is decided afterwards.
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut faces: Vec<Option<Face>> = vec![None; chars.len()];
    let mut resolved: Resolved = vec![None; MATCHED.len()];
    let mut index = 0;
    while index < chars.len() {
        let Some(&(_, first)) = chars.get(index) else {
            break;
        };
        let script = first.script();
        if matches!(script, Script::Common | Script::Inherited | Script::Unknown) {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while let Some(&(_, c)) = chars.get(end) {
            let s = c.script();
            if s == script || s == Script::Inherited {
                end += 1;
            } else {
                break;
            }
        }
        let word: Vec<char> = chars
            .get(index..end)
            .unwrap_or_default()
            .iter()
            .map(|(_, c)| *c)
            .collect();
        let face = if word.iter().all(|c| primary.covers(*c)) {
            Face::Primary
        } else {
            MATCHED
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.scripts.contains(&script))
                .find_map(|(i, _)| {
                    let slot = resolved.get_mut(i)?;
                    if slot.is_none() {
                        *slot = Some(resolve(db, i, attrs));
                    }
                    let fallback = slot.as_ref()?.as_ref()?;
                    word.iter()
                        .all(|c| fallback.covers(*c))
                        .then_some(Face::Matched(i))
                })
                .unwrap_or(Face::Primary)
        };
        for slot in faces.get_mut(index..end).unwrap_or_default() {
            *slot = Some(face);
        }
        index = end;
    }
    // Between two words of one face, everything joins that face. Between
    // two faces, punctuation that closes the word before (a comma, a full
    // stop, a closing quote) stays with it and punctuation that opens the
    // word after (an opening quote or bracket) goes with it; spaces, digits,
    // dashes and symbols between them are the primary face's, so they match
    // across a line. A leading or trailing run joins its one neighbour. A
    // character stays in a matched face only when that face has it.
    let mut index = 0;
    while index < chars.len() {
        if faces.get(index).copied().flatten().is_some() {
            index += 1;
            continue;
        }
        let mut end = index;
        while faces.get(end).is_some_and(Option::is_none) {
            end += 1;
        }
        let run = chars.get(index..end).unwrap_or_default();
        let before = index
            .checked_sub(1)
            .and_then(|i| faces.get(i).copied().flatten());
        let after = faces.get(end).copied().flatten();
        let closing = run.iter().take_while(|(_, c)| closes(*c)).count();
        let opening = run.len() - run.iter().rev().take_while(|(_, c)| opens(*c)).count();
        for (offset, &(_, c)) in run.iter().enumerate() {
            let target = match (before, after) {
                (Some(before), Some(after)) if before == after => before,
                (Some(before), Some(_)) if offset < closing => before,
                (Some(_), Some(after)) if offset >= opening => after,
                (Some(_), Some(_)) | (None, None) => Face::Primary,
                (Some(only), None) | (None, Some(only)) => only,
            };
            let face = match target {
                Face::Matched(m) => {
                    let covered = resolved
                        .get(m)
                        .and_then(|slot| slot.as_ref()?.as_ref())
                        .is_some_and(|fallback| fallback.covers(c));
                    if covered { target } else { Face::Primary }
                }
                Face::Primary => Face::Primary,
            };
            if let Some(slot) = faces.get_mut(index + offset) {
                *slot = Some(face);
            }
        }
        index = end;
    }
    let faces = chars
        .iter()
        .zip(faces)
        .map(|((offset, _), face)| (*offset, face.unwrap_or(Face::Primary)))
        .collect();
    (faces, resolved)
}

/// Punctuation that ends what comes before it: a comma, a full stop, a
/// closing bracket or quote.
fn closes(c: char) -> bool {
    matches!(
        c.general_category(),
        GeneralCategory::ClosePunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::OtherPunctuation
    )
}

/// Punctuation that begins what comes after it: an opening bracket or quote.
fn opens(c: char) -> bool {
    matches!(
        c.general_category(),
        GeneralCategory::OpenPunctuation | GeneralCategory::InitialPunctuation
    )
}

/// The matched face `index` in this run's style, when the font system has
/// it.
fn resolve(db: &fontdb::Database, index: usize, attrs: &Attrs<'_>) -> Option<Arc<FaceCaps>> {
    let entry = MATCHED.get(index)?;
    if attrs.style != Style::Normal && !entry.italic {
        return None;
    }
    let id = db.query(&fontdb::Query {
        families: &[Family::Name(entry.family)],
        weight: attrs.weight,
        stretch: attrs.stretch,
        style: attrs.style,
    })?;
    let face = db.face(id)?;
    if !face.families.iter().any(|(name, _)| name == entry.family) {
        return None;
    }
    caps(db, id)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the weight is clamped to 1..=1000 before narrowing"
)]
fn weight_of(value: f32) -> Weight {
    Weight(value.round().clamp(1.0, 1000.0) as u16)
}

/// `attrs` set in a matched fallback: its family, its mapped weight, the
/// size that matches the primary face's x-height, its own features plus the
/// kind's, and the run's line height unchanged.
fn fallback_attrs<'a>(
    attrs: &Attrs<'a>,
    entry: &Matched,
    primary: &FaceCaps,
    fallback: &FaceCaps,
    metrics: Metrics,
    kind: TextKind,
) -> Attrs<'a> {
    let weight = f32::from(attrs.weight.0);
    let mapped = fallback.clamp_weight(entry.scale.mul_add(weight, entry.offset));
    let factor = match (
        primary.x_height_em(primary.clamp_weight(weight)),
        fallback.x_height_em(mapped),
    ) {
        (Some(primary), Some(fallback)) if fallback > 0.0 => primary / fallback,
        _ => 1.0,
    };
    let own: Vec<[u8; 4]> = entry
        .features
        .iter()
        .copied()
        .filter(|tag| fallback.has_feature(*tag))
        .collect();
    with_features(attrs, &own, &fallback.kind_features(kind), false)
        .family(Family::Name(entry.family))
        .weight(weight_of(mapped))
        .metrics(Metrics::new(
            metrics.font_size * factor,
            metrics.line_height,
        ))
}

/// `attrs` with `own` and `kind` features ahead of its explicit ones, so an
/// explicit setting still wins. A fallback face keeps only the explicit
/// features that mean the same in every face: stylistic sets and character
/// variants name glyphs of the face they were chosen for.
fn with_features<'a>(
    attrs: &Attrs<'a>,
    own: &[[u8; 4]],
    kind: &[[u8; 4]],
    primary: bool,
) -> Attrs<'a> {
    if own.is_empty() && kind.is_empty() && primary {
        return attrs.clone();
    }
    let mut features = FontFeatures::new();
    for tag in own.iter().chain(kind) {
        features.set(FeatureTag::new(tag), 1);
    }
    for feature in &attrs.font_features.features {
        let tag = feature.tag.as_bytes();
        if primary || !matches!(tag, [b's', b's', ..] | [b'c', b'v', ..]) {
            features.set(feature.tag, feature.value);
        }
    }
    attrs.clone().font_features(features)
}

/// Put planned spans into `buffer`: one span as plain text, several as rich
/// text. cosmic-text 0.19's rich setter omits the empty paragraph after a
/// final line feed that its plain setter keeps; it is restored so a planned
/// paragraph has the same lines as an unplanned one.
pub(crate) fn set_spans(
    buffer: &mut Buffer,
    spans: &[(&str, Attrs<'_>)],
    defaults: &Attrs<'_>,
    align: Option<Align>,
) {
    if let [(text, attrs)] = spans {
        buffer.set_text(text, attrs, Shaping::Advanced, align);
        return;
    }
    buffer.set_rich_text(
        spans.iter().map(|(text, attrs)| (*text, attrs.clone())),
        defaults,
        Shaping::Advanced,
        align,
    );
    if spans
        .iter()
        .rev()
        .find(|(text, _)| !text.is_empty())
        .is_some_and(|(text, _)| text.ends_with('\n'))
    {
        let terminal = spans
            .last()
            .map_or_else(|| defaults.clone(), |(_, attrs)| attrs.clone());
        let mut line = BufferLine::new(
            "",
            LineEnding::None,
            AttrsList::new(&terminal),
            Shaping::Advanced,
        );
        line.set_align(align);
        buffer.lines.push(line);
    }
}

#[cfg(test)]
#[path = "matched_tests.rs"]
mod tests;
