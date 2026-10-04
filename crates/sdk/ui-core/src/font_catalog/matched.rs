//! Script fallbacks matched to the UI face, and the features a text kind
//! resolves to.
//!
//! cosmic-text falls back per glyph to whatever face covers a character, at
//! the size and weight the run asked for. That is right for Arabic or CJK,
//! which have no x-height to match, but Cyrillic, Greek and Vietnamese set
//! that way sit visibly smaller or larger, lighter or heavier, than the Latin
//! around them. Tilo Sans covers Vietnamese and Greek itself; the matched
//! faces serve the scripts it lacks, and any other UI face's gaps. Before
//! shaping, every run is split here into spans the shaper can set as one
//! face:
//!
//! - a word in a script the run's face lacks goes to the face matched for
//!   that script ([`MATCHED`]), at the size that gives it the primary face's
//!   x-height and at the weight that looks like the primary's;
//! - punctuation stays with the word it closes or opens; spaces, digits
//!   and dashes between two faces are the primary face's;
//! - every span gets the OpenType features its [`TextKind`] asks for that
//!   its own face has; in prose, the links, email addresses and mentions
//!   [`crate::text_detect`] finds are spans of their own kind.
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
use super::optical::Optical;
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
/// word. A word the run's own face covers never reaches them.
const MATCHED: [Matched; 2] = [
    // Onest's straight-armed forms (`ss02`) are the closest Cyrillic to
    // Tilo Sans' Latin; at the same x-height its stems match at about 1.2x
    // the weight above 400.
    Matched {
        scripts: &[Script::Cyrillic],
        family: "Onest",
        features: &[*b"ss02"],
        scale: 1.2,
        offset: -90.0,
        italic: false,
    },
    // Italic Cyrillic, and Greek, Vietnamese and any other Latin letters a
    // UI face lacks (Hyperlegible has no Greek and no Vietnamese).
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

thread_local! {
    /// The person's "Always distinguish similar letters" setting, and how
    /// many security surfaces are being laid out or painted right now. Text
    /// is shaped on the thread that owns the renderer, so both are per
    /// thread; [`crate::text_render::TextRenderer`] sets them.
    static DISTINGUISH: std::cell::Cell<(bool, u32)> = const { std::cell::Cell::new((false, 0)) };
}

/// Turn the "Always distinguish similar letters" setting on or off for text
/// shaped on this thread. Returns whether it changed.
pub(crate) fn set_distinguish_everywhere(on: bool) -> bool {
    DISTINGUISH.with(|cell| {
        let (was, depth) = cell.get();
        cell.set((on, depth));
        was != on
    })
}

/// Enter (`true`) or leave (`false`) a security surface on this thread.
pub(crate) fn security_surface(enter: bool) {
    DISTINGUISH.with(|cell| {
        let (on, depth) = cell.get();
        let depth = if enter {
            depth.saturating_add(1)
        } else {
            depth.saturating_sub(1)
        };
        cell.set((on, depth));
    });
}

/// Whether text shaped on this thread now sets every look-alike apart: the
/// setting is on, or a security surface is being drawn.
pub(crate) fn distinguish_everywhere() -> bool {
    DISTINGUISH.with(|cell| {
        let (on, depth) = cell.get();
        on || depth > 0
    })
}

/// Split `spans` into spans of one face each, each in the optical cut of its
/// family for the size it is set at ([`super::optical`]), with features
/// resolved for `kind`. `base` is the buffer's own metrics, used for spans
/// that carry none. `distinguish` sets every look-alike apart whatever the
/// kind.
pub(crate) fn plan<'t, 'a>(
    db: &fontdb::Database,
    spans: impl IntoIterator<Item = (&'t str, Attrs<'a>)>,
    base: Metrics,
    kind: TextKind,
    optical: Optical,
    distinguish: bool,
) -> Vec<(&'t str, Attrs<'a>)> {
    plan_runs(
        db,
        spans.into_iter().map(|(text, attrs)| (text, attrs, None)),
        base,
        kind,
        optical,
        distinguish,
    )
}

/// [`plan`] for runs that may say their own kind (a mention or a link in
/// rich text). In `Prose`, the links, email addresses, mentions and room
/// aliases [`crate::text_detect`] finds across the runs are set as what
/// they are; a run with its own kind keeps it.
pub(crate) fn plan_runs<'t, 'a>(
    db: &fontdb::Database,
    runs: impl IntoIterator<Item = (&'t str, Attrs<'a>, Option<TextKind>)>,
    base: Metrics,
    kind: TextKind,
    optical: Optical,
    distinguish: bool,
) -> Vec<(&'t str, Attrs<'a>)> {
    let runs: Vec<(&'t str, Attrs<'a>, Option<TextKind>)> = runs.into_iter().collect();
    let detected = if kind == TextKind::Prose {
        match runs.as_slice() {
            [(text, _, None)] => crate::text_detect::identifier_kinds(text),
            [_] => Vec::new(),
            _ => crate::text_detect::identifier_kinds(
                &runs.iter().map(|(text, _, _)| *text).collect::<String>(),
            ),
        }
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    let mut offset = 0;
    for (text, attrs, own) in runs {
        let attrs = in_optical_cut(db, attrs, base, optical);
        let start = offset;
        offset += text.len();
        if let Some(own) = own {
            plan_span(db, text, &attrs, base, own, distinguish, &mut out);
            continue;
        }
        let mut at = 0;
        for (range, found) in &detected {
            let from = range.start.max(start).saturating_sub(start);
            let to = range.end.min(offset).saturating_sub(start);
            if from >= to {
                continue;
            }
            if let Some(before) = text.get(at..from).filter(|s| !s.is_empty()) {
                plan_span(db, before, &attrs, base, kind, distinguish, &mut out);
            }
            if let Some(span) = text.get(from..to) {
                plan_span(db, span, &attrs, base, *found, distinguish, &mut out);
            }
            at = to;
        }
        if let Some(rest) = text.get(at..).filter(|s| at == 0 || !s.is_empty()) {
            plan_span(db, rest, &attrs, base, kind, distinguish, &mut out);
        }
    }
    out
}

/// `attrs` in the cut of its family for the size the span is set at, when
/// its family (named, or the generic serif) is drawn in optical cuts.
fn in_optical_cut<'a>(
    db: &fontdb::Database,
    attrs: Attrs<'a>,
    base: Metrics,
    optical: Optical,
) -> Attrs<'a> {
    let family = match attrs.family {
        Family::Name(name) => name,
        Family::Serif => db.family_name(&Family::Serif),
        _ => return attrs,
    };
    let size = attrs.metrics_opt.map_or(base, Metrics::from).font_size;
    let italic = attrs.style != Style::Normal;
    let bold = attrs.weight.0 > 500;
    // A cut this font system lacks in the run's style and weight (the
    // standalone catalog's missing Regular, whose family still holds the
    // shared Bold) is served by the next smaller cut.
    let installed = |cut: &str| {
        db.faces().any(|face| {
            (face.style != fontdb::Style::Normal) == italic
                && (face.weight.0 > 500) == bold
                && face
                    .families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(cut))
        })
    };
    match super::optical::cut_where(family, size, optical, italic, attrs.weight.0, installed) {
        Some(cut) if cut != family => attrs.family(Family::Name(cut)),
        _ => attrs,
    }
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
    distinguish: bool,
    out: &mut Vec<(&'t str, Attrs<'a>)>,
) {
    // Most UI text: nothing to split and no features to choose.
    if kind == TextKind::Prose && !distinguish && text.is_ascii() {
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
    let primary_attrs = with_features(attrs, &[], &primary.features_for(kind, distinguish), true);
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
                (Some(fallback), Some(entry)) => fallback_attrs(
                    attrs,
                    entry,
                    &primary,
                    &fallback,
                    metrics,
                    kind,
                    distinguish,
                ),
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
    distinguish: bool,
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
    with_features(
        attrs,
        &own,
        &fallback.features_for(kind, distinguish),
        false,
    )
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
