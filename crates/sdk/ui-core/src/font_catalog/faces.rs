//! What a face can do, read from its own tables.
//!
//! Every decision the toolkit makes about a face — whether it covers a
//! character, which OpenType features a [`TextKind`] turns on, how large a
//! fallback must be set to share its x-height — is answered here, from the
//! face's `cmap`, `GSUB`/`GPOS` feature lists, `fvar` axes and `OS/2`/`MVAR`
//! metrics. Nothing names a font: a face that gains a feature (Tilo Sans'
//! distinct I and l, a slashed zero) is used for it with no code change.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use cosmic_text::fontdb;
use skrifa::raw::TableProvider;
use skrifa::raw::tables::gsub::{SingleSubst, SubstitutionSubtables};
use skrifa::{FontRef, MetadataProvider, Tag};

use crate::text_render::TextKind;

/// Characters a "distinct I and l" stylistic set or character variant is
/// expected to touch: capital I, lowercase l and their accented forms, and
/// the one and the bar they are confused with. A set that also changes any
/// other encoded character (a titling J, a single-storey a) is a different
/// stylistic choice and is never applied for a kind.
const CONFUSABLE_IL: &str = "IÌÍÎÏĨĪĬĮİǏȈȊỈỊlĺļľŀłƚ1|¦ǀ";

/// What one face supports.
#[derive(Debug)]
pub(crate) struct FaceCaps {
    /// Every character the face maps, sorted.
    codepoints: Vec<u32>,
    /// `GSUB` and `GPOS` feature tags, sorted.
    features: Vec<[u8; 4]>,
    /// Stylistic sets and character variants that only make I and l (and
    /// their look-alikes) distinct.
    distinct_il: Vec<[u8; 4]>,
    /// x-height in em at sampled `wght` positions (the default location for
    /// a face without a weight axis), for interpolation.
    x_height: Vec<(f32, f32)>,
    /// The `wght` axis range, when the face is variable in weight.
    weight_axis: Option<(f32, f32)>,
    pub(crate) monospaced: bool,
}

impl FaceCaps {
    fn read(data: &[u8], index: u32, monospaced: bool) -> Option<Self> {
        let font = FontRef::from_index(data, index).ok()?;
        let mut codepoints: Vec<u32> = font.charmap().mappings().map(|(c, _)| c).collect();
        codepoints.sort_unstable();
        codepoints.dedup();
        let mut features = Vec::new();
        if let Ok(gsub) = font.gsub()
            && let Ok(list) = gsub.feature_list()
        {
            features.extend(
                list.feature_records()
                    .iter()
                    .map(|r| r.feature_tag().to_be_bytes()),
            );
        }
        if let Ok(gpos) = font.gpos()
            && let Ok(list) = gpos.feature_list()
        {
            features.extend(
                list.feature_records()
                    .iter()
                    .map(|r| r.feature_tag().to_be_bytes()),
            );
        }
        features.sort_unstable();
        features.dedup();
        let weight_axis = font
            .axes()
            .iter()
            .find(|axis| axis.tag() == Tag::new(b"wght"))
            .map(|axis| (axis.min_value(), axis.max_value()));
        let x_height = match weight_axis {
            Some((min, max)) => {
                let mut samples: Vec<(f32, f32)> = [
                    100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0, 800.0, 900.0,
                ]
                .into_iter()
                .map(|weight: f32| weight.clamp(min, max))
                .filter_map(|weight| Some((weight, x_height_em(&font, Some(weight))?)))
                .collect();
                samples.dedup_by(|a, b| (a.0 - b.0).abs() < f32::EPSILON);
                samples
            }
            None => x_height_em(&font, None)
                .map(|em| vec![(400.0, em)])
                .unwrap_or_default(),
        };
        Some(Self {
            distinct_il: distinct_il_features(&font),
            codepoints,
            features,
            x_height,
            weight_axis,
            monospaced,
        })
    }

    pub(crate) fn covers(&self, character: char) -> bool {
        self.codepoints.binary_search(&u32::from(character)).is_ok()
    }

    pub(crate) fn has_feature(&self, tag: [u8; 4]) -> bool {
        self.features.binary_search(&tag).is_ok()
    }

    /// The face's x-height in em at `weight`, interpolated between samples.
    pub(crate) fn x_height_em(&self, weight: f32) -> Option<f32> {
        let first = self.x_height.first()?;
        if weight <= first.0 {
            return Some(first.1);
        }
        for pair in self.x_height.windows(2) {
            let [(w0, x0), (w1, x1)] = pair else { continue };
            if weight <= *w1 {
                let span = (w1 - w0).max(f32::EPSILON);
                return Some(x0 + (x1 - x0) * (weight - w0) / span);
            }
        }
        self.x_height.last().map(|(_, em)| *em)
    }

    /// `weight` clamped to the face's `wght` axis.
    pub(crate) fn clamp_weight(&self, weight: f32) -> f32 {
        self.weight_axis
            .map_or(weight, |(min, max)| weight.clamp(min, max))
    }

    /// The features `kind` asks for that this face has, in application
    /// order.
    pub(crate) fn kind_features(&self, kind: TextKind) -> Vec<[u8; 4]> {
        let wanted: &[[u8; 4]] = match kind {
            TextKind::Prose | TextKind::Code | TextKind::Identifier => &[],
            TextKind::Numeric => &[*b"tnum", *b"lnum"],
            TextKind::Secret => &[*b"tnum", *b"lnum", *b"zero"],
        };
        let mut out: Vec<[u8; 4]> = wanted
            .iter()
            .copied()
            .filter(|tag| self.has_feature(*tag))
            .collect();
        if matches!(kind, TextKind::Secret | TextKind::Identifier) {
            out.extend(self.distinct_il.iter().copied());
        }
        out
    }
}

/// x-height in em at a `wght` location: `OS/2.sxHeight` as varied by `MVAR`,
/// else the measured height of the face's `x`.
fn x_height_em(font: &FontRef<'_>, weight: Option<f32>) -> Option<f32> {
    let location = match weight {
        Some(weight) => font.axes().location([(Tag::new(b"wght"), weight)]),
        None => font.axes().location(std::iter::empty::<(Tag, f32)>()),
    };
    let metrics = font.metrics(skrifa::instance::Size::unscaled(), &location);
    let upem = f32::from(metrics.units_per_em.max(1));
    let units = metrics.x_height.filter(|h| *h > 0.0).or_else(|| {
        let glyph = font.charmap().map('x')?;
        font.glyph_metrics(skrifa::instance::Size::unscaled(), &location)
            .bounds(glyph)
            .map(|bounds| bounds.y_max)
            .filter(|h| *h > 0.0)
    })?;
    Some(units / upem)
}

/// Stylistic sets (`ssNN`) and character variants (`cvNN`) whose single or
/// alternate substitutions start only from I, l and their look-alikes, and
/// from at least one I or l.
fn distinct_il_features(font: &FontRef<'_>) -> Vec<[u8; 4]> {
    let charmap = font.charmap();
    let mut glyph_chars: HashMap<u32, char> = HashMap::new();
    for (code, glyph) in charmap.mappings() {
        if let Some(character) = char::from_u32(code) {
            glyph_chars.entry(glyph.to_u32()).or_insert(character);
        }
    }
    let Ok(gsub) = font.gsub() else {
        return Vec::new();
    };
    let (Ok(features), Ok(lookups)) = (gsub.feature_list(), gsub.lookup_list()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for record in features.feature_records() {
        let tag = record.feature_tag().to_be_bytes();
        if !matches!(&tag, [b's', b's', ..] | [b'c', b'v', ..]) || out.contains(&tag) {
            continue;
        }
        let Ok(feature) = record.feature(features.offset_data()) else {
            continue;
        };
        let mut sources = Vec::new();
        for index in feature.lookup_list_indices() {
            let Ok(lookup) = lookups.lookups().get(usize::from(index.get())) else {
                continue;
            };
            let Ok(subtables) = lookup.subtables() else {
                continue;
            };
            match subtables {
                SubstitutionSubtables::Single(tables) => {
                    for table in tables.iter().flatten() {
                        let coverage = match &table {
                            SingleSubst::Format1(t) => t.coverage(),
                            SingleSubst::Format2(t) => t.coverage(),
                        };
                        if let Ok(coverage) = coverage {
                            sources.extend(coverage.iter().map(|g| u32::from(g.to_u16())));
                        }
                    }
                }
                SubstitutionSubtables::Alternate(tables) => {
                    for table in tables.iter().flatten() {
                        if let Ok(coverage) = table.coverage() {
                            sources.extend(coverage.iter().map(|g| u32::from(g.to_u16())));
                        }
                    }
                }
                _ => {}
            }
        }
        let encoded: Vec<char> = sources
            .iter()
            .filter_map(|glyph| glyph_chars.get(glyph).copied())
            .collect();
        if only_distinguishes_il(&encoded) {
            out.push(tag);
        }
    }
    out
}

/// Whether a feature that substitutes these encoded characters is a
/// "distinct I and l" set: it changes I or l and nothing but their
/// look-alikes.
pub(crate) fn only_distinguishes_il(substituted: &[char]) -> bool {
    substituted.iter().any(|c| matches!(c, 'I' | 'l'))
        && substituted.iter().all(|c| CONFUSABLE_IL.contains(*c))
}

/// PostScript name, collection index, first family name and style: the same
/// face loaded into another database (a clone, a browser bundle) shares it.
type CapsKey = (String, u32, String, bool);

/// The capabilities of `id` in `db`, read once per face per process.
pub(crate) fn caps(db: &fontdb::Database, id: fontdb::ID) -> Option<Arc<FaceCaps>> {
    static CACHE: OnceLock<Mutex<HashMap<CapsKey, Option<Arc<FaceCaps>>>>> = OnceLock::new();
    let face = db.face(id)?;
    let key = (
        face.post_script_name.clone(),
        face.index,
        face.families
            .first()
            .map(|(name, _)| name.clone())
            .unwrap_or_default(),
        face.style == fontdb::Style::Italic,
    );
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(found) = cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return found.clone();
    }
    let monospaced = face.monospaced;
    let read = db
        .with_face_data(id, |data, index| FaceCaps::read(data, index, monospaced))
        .flatten()
        .map(Arc::new);
    cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(key)
        .or_insert(read)
        .clone()
}
