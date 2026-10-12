//! The library's model: what is stored, and the pure queries the window
//! asks of it.
//!
//! [`PhotoLibrary`] is the record: one [`LibraryPhoto`] per item of a
//! [`Collection`] and the [`Album`]s that file them. `#[derive(Model)]` is
//! the whole storage contract; each field merges as its type says (people
//! and tags per label, a crop and a turn independently). The pixels never
//! enter the document: a photograph names its bytes by content hash, and
//! they stay in the app's content store. The record's verbs are in
//! [`crate::resource`].

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Collection, Id, JsonSchema, Model, Refusal, Set};

/// Bounded, explainable intelligence derived on a device from a photograph.
///
/// This record contains no pixels, embeddings, face geometry, machine-local
/// file names, or opaque model output.  A local analysis is private until a
/// user or trusted caller explicitly publishes it through `SetImageAnalysis`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageAnalysis {
    /// The text read from the pixels. Line layout is intentionally flattened:
    /// the shared search layer needs words, not page coordinates.
    #[serde(default)]
    pub ocr_text: String,
    /// A concise, local description suitable for finding a photograph later.
    #[serde(default)]
    pub description: String,
    /// Recognized non-person subjects, for example "bicycle" or "beach".
    #[serde(default)]
    pub objects: Vec<String>,
    /// Pet labels are separate from generic objects because people browse
    /// their animals differently from everything else in a scene.
    #[serde(default)]
    pub pets: Vec<String>,
    /// How the result was made. It gives collaborators enough context to
    /// decide whether to trust or refresh it without leaking local paths.
    #[serde(default)]
    pub provenance: AnalysisProvenance,
    /// Provenance for the text currently stored in `ocr_text`. Kept separate
    /// from visual analysis so a later OCR or vision job cannot erase the
    /// other component's audit trail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_provenance: Option<AnalysisProvenance>,
    /// Provenance for `description`, `objects`, and `pets`. Older records use
    /// `provenance`; newly produced records populate this field explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision_provenance: Option<AnalysisProvenance>,
}

/// Text read from a photograph's pixels, and how.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OcrComponent {
    pub text: String,
    pub provenance: AnalysisProvenance,
}

/// What a photograph shows, as image analysis found it, and how.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VisionComponent {
    pub description: String,
    pub objects: Vec<String>,
    pub pets: Vec<String>,
    pub provenance: AnalysisProvenance,
}

/// The bounded provenance attached to an analysis result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisProvenance {
    /// `local-ocr`, `local-vision`, `local-description` (legacy), or `user`.
    #[serde(default = "default_analysis_source")]
    pub source: String,
    /// Stable engine/model family identifier, never an error or command line.
    #[serde(default)]
    pub engine: String,
    /// Signed local model identifier when a vision result was produced. Empty
    /// for OCR, user-authored, and older records.
    #[serde(default)]
    pub model_id: String,
    /// Immutable signed model revision when a vision result was produced.
    /// Empty preserves backwards compatibility with pre-provenance records.
    #[serde(default)]
    pub model_revision: String,
    /// OCR language when text recognition was used.
    #[serde(default)]
    pub language: String,
    /// The content hash this result describes. Empty means a legacy/path-only
    /// image and makes the result deliberately refreshable, not transferable.
    #[serde(default)]
    pub content: String,
    /// Explicit sharing semantics: `shared` records were accepted through the
    /// document action; `local` records must never be written to the CRDT.
    #[serde(default = "default_analysis_scope")]
    pub scope: String,
}

fn default_analysis_source() -> String {
    "user".to_owned()
}
fn default_analysis_scope() -> String {
    "shared".to_owned()
}

impl Default for AnalysisProvenance {
    fn default() -> Self {
        Self {
            source: default_analysis_source(),
            engine: String::new(),
            model_id: String::new(),
            model_revision: String::new(),
            language: String::new(),
            content: String::new(),
            scope: default_analysis_scope(),
        }
    }
}

impl ImageAnalysis {
    /// Join every human-searchable field without exposing provenance internals.
    #[must_use]
    pub fn searchable_text(&self) -> String {
        [
            self.ocr_text.as_str(),
            self.description.as_str(),
            &self.objects.join(" "),
            &self.pets.join(" "),
        ]
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
    }

    /// Whether the record has any useful result to retain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ocr_text.is_empty()
            && self.description.is_empty()
            && self.objects.is_empty()
            && self.pets.is_empty()
    }

    /// The vision component that belongs to `content`, if one has been
    /// completed. Empty model output still counts: completion provenance is
    /// what prevents a whole-library scan from retrying a blank scene forever.
    #[must_use]
    pub fn vision_provenance_for_content(
        &self,
        content: Option<&str>,
    ) -> Option<&AnalysisProvenance> {
        let provenance = self.vision_provenance.as_ref().or_else(|| {
            matches!(
                self.provenance.source.as_str(),
                "local-vision" | "local-description"
            )
            .then_some(&self.provenance)
        })?;
        match content {
            Some(content) => (provenance.content == content).then_some(provenance),
            None => provenance.content.is_empty().then_some(provenance),
        }
    }

    /// Whether the current bytes have already been processed by this exact
    /// signed model revision.
    #[must_use]
    pub fn has_vision_component(&self, content: Option<&str>, model_revision: &str) -> bool {
        self.vision_provenance_for_content(content)
            .is_some_and(|provenance| provenance.model_revision == model_revision)
    }
}

/// A deliberately small, explicitly shared location attached to one photo.
///
/// Coordinates are useful for browsing a personal library, but are sensitive:
/// this is never inferred from an IP address, uploaded for reverse geocoding,
/// or copied from a machine-local file path.  A location reaches the shared
/// document only through `SetPhotoLocation`; clearing it removes the complete
/// shared record.  The label is a collaborator-visible convenience, not an
/// authority over the coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PhotoLocation {
    /// WGS-84 latitude in decimal degrees.
    pub latitude: f64,
    /// WGS-84 longitude in decimal degrees.
    pub longitude: f64,
    /// Optional user-facing place name, e.g. "Golden Gate Park".
    #[serde(default)]
    pub label: Option<String>,
    /// Why this location exists and whether it was deliberately shared.
    #[serde(default)]
    pub provenance: LocationProvenance,
}

/// Compact, inspectable provenance for photo location metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocationProvenance {
    /// `user`, `map`, `exif`, or `device`.  `map` records a deliberate
    /// placement on Photos' bundled offline reference map. No network-derived source is silently
    /// accepted: hosted geocoding would be a privacy boundary, not metadata.
    #[serde(default = "default_location_source")]
    pub source: String,
    /// `shared` is required for durable locations. `local` is reserved for
    /// an app-private draft and must never be persisted in the CRDT.
    #[serde(default = "default_location_scope")]
    pub scope: String,
}

fn default_location_source() -> String {
    "user".to_owned()
}
fn default_location_scope() -> String {
    "shared".to_owned()
}

impl Default for LocationProvenance {
    fn default() -> Self {
        Self {
            source: default_location_source(),
            scope: default_location_scope(),
        }
    }
}

/// Null Island, shared by the person: what a place is before it is set.
impl Default for PhotoLocation {
    fn default() -> Self {
        Self {
            latitude: 0.0,
            longitude: 0.0,
            label: None,
            provenance: LocationProvenance::default(),
        }
    }
}

impl PhotoLocation {
    /// Coordinates must be finite WGS-84 degrees and the optional label must
    /// be short enough to remain safe in collaboration/search projections.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.latitude.is_finite()
            && self.longitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
            && self.label.as_ref().is_none_or(|label| {
                !label.trim().is_empty()
                    && label.chars().count() <= 256
                    && !label.chars().any(char::is_control)
            })
            && matches!(
                self.provenance.source.as_str(),
                "user" | "map" | "exif" | "device"
            )
            && self.provenance.scope == "shared"
    }

    /// Stable resource identity for a named place.  Coordinates are rounded
    /// to ~11m so two metadata records in the same small place cluster rather
    /// than producing an unhelpful resource for every camera jitter sample.
    #[must_use]
    pub fn place_key(&self) -> String {
        let lat = (self.latitude * 10_000.0).round() as i32;
        let lon = (self.longitude * 10_000.0).round() as i32;
        format!("{lat}:{lon}:{}", self.label.as_deref().unwrap_or_default())
    }

    /// Human-safe detail, deliberately excluding exact coordinates from card
    /// summaries where accidental shoulder-surfing is more likely.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| "Shared location".to_owned())
    }
}

/// The most labels of one kind a photograph carries.
pub const MAX_LABELS: usize = 32;
/// The longest label, folder or capture value, in characters.
pub const MAX_LABEL_CHARS: usize = 128;
/// The longest album name, in characters.
pub const MAX_NAME_CHARS: usize = 256;
/// The most photographs one album holds.
pub const MAX_ALBUM_MEMBERS: usize = 4_096;
const MAX_ALBUM_ID_CHARS: usize = 160;
const MAX_RESOURCE_ID_CHARS: usize = 4_096;
const MAX_ANALYSIS_TEXT_CHARS: usize = crate::analysis::MAX_OCR_CHARS;
const MAX_ANALYSIS_LABELS: usize = 64;
const MAX_ANALYSIS_LABEL_CHARS: usize = 128;
/// A content hash is 64 lowercase hex digits.
pub(crate) const CONTENT_HEX_LEN: usize = 64;

/// A durable entry in Photos' shared library. Pixel bytes remain in the
/// content store; this is intentionally only addressable metadata.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = LibraryPhoto::valid)]
pub struct LibraryPhoto {
    /// The photograph's stable id, derived from its content.
    #[model(readonly)]
    pub id: String,
    /// Name: what the photograph is called, its file's name when it came
    /// in.
    #[model(label, readonly)]
    pub name: String,
    /// The picture: its name, size and content address.
    #[model(readonly)]
    pub image: OpenImage,
    /// Favorite
    pub favorite: bool,
    /// Folder: a user-organized label. This is deliberately metadata rather
    /// than an on-disk directory: a shared path only has meaning on the
    /// machine that happened to import the picture.
    pub folder: Option<String>,
    /// People: named people visible in the photograph. Compact labels, not
    /// biometric data or a second copy of the image.
    pub people: Set<String>,
    /// Tags: freeform, durable search labels.
    pub tags: Set<String>,
    /// Capture time, as authored: keeping the original string avoids
    /// silently changing a collaborator's timezone or precision.
    pub captured_at: Option<String>,
    /// Place: an explicit location. Absence means no location was shared;
    /// it is not a failed lookup and never triggers a network request.
    pub location: Option<PhotoLocation>,
    /// Recognized text: explicitly shared text recognition. Device-local
    /// analysis stays in the caller's cache until it is shared deliberately.
    /// Text and image analysis are kept apart, so finishing one never
    /// erases the other, whichever device finished first.
    pub ocr: Option<OcrComponent>,
    /// What is in it: explicitly shared image analysis, its description,
    /// objects and pets.
    pub vision: Option<VisionComponent>,
    /// Crop and turn: the photograph's own; editing one photograph never
    /// touches another's.
    pub edits: Edits,
}

impl LibraryPhoto {
    #[must_use]
    pub fn from_image(image: OpenImage) -> Self {
        let id = match &image.content {
            Some(hash) => format!("photos.photo:{hash}"),
            None => format!("photos.photo:path:{}", image.path),
        };
        Self {
            id,
            name: image.name.clone(),
            image,
            ..Self::default()
        }
    }

    /// The photograph's item id in the library.
    #[must_use]
    pub fn item(&self) -> Id {
        Id::from_raw(self.id.clone())
    }

    /// The shared analysis: its text and image components together.
    #[must_use]
    pub fn analysis(&self) -> Option<ImageAnalysis> {
        if self.ocr.is_none() && self.vision.is_none() {
            return None;
        }
        let mut analysis = ImageAnalysis::default();
        if let Some(ocr) = &self.ocr {
            analysis.ocr_text.clone_from(&ocr.text);
            analysis.ocr_provenance = Some(ocr.provenance.clone());
        }
        if let Some(vision) = &self.vision {
            analysis.description.clone_from(&vision.description);
            analysis.objects.clone_from(&vision.objects);
            analysis.pets.clone_from(&vision.pets);
            analysis.vision_provenance = Some(vision.provenance.clone());
        }
        if let Some(provenance) = analysis
            .vision_provenance
            .clone()
            .or_else(|| analysis.ocr_provenance.clone())
        {
            analysis.provenance = provenance;
        }
        Some(analysis)
    }

    /// Share `analysis`, or none: its text and image components.
    pub fn set_analysis(&mut self, analysis: Option<ImageAnalysis>) {
        let Some(analysis) = analysis else {
            (self.ocr, self.vision) = (None, None);
            return;
        };
        let ocr = analysis
            .ocr_provenance
            .clone()
            .or_else(|| (!analysis.ocr_text.is_empty()).then(|| analysis.provenance.clone()));
        self.ocr = ocr.map(|provenance| OcrComponent {
            text: analysis.ocr_text.clone(),
            provenance,
        });
        let vision = analysis.vision_provenance.clone().or_else(|| {
            (!analysis.description.is_empty()
                || !analysis.objects.is_empty()
                || !analysis.pets.is_empty())
            .then(|| analysis.provenance.clone())
        });
        self.vision = vision.map(|provenance| VisionComponent {
            description: analysis.description,
            objects: analysis.objects,
            pets: analysis.pets,
            provenance,
        });
    }

    /// The people labels, in order.
    #[must_use]
    pub fn people(&self) -> Vec<String> {
        self.people.iter().cloned().collect()
    }

    /// The tags, in order.
    #[must_use]
    pub fn tags(&self) -> Vec<String> {
        self.tags.iter().cloned().collect()
    }

    /// What every replica accepts of a photograph a library holds: named by
    /// what it is, its labels compact, its edits a real crop and turn, and
    /// shared analysis and places that say where they came from.
    fn valid(&self) -> Result<(), Refusal> {
        if self.id != Self::from_image(self.image.clone()).id || !valid_resource_id(&self.id) {
            return Err(Refusal::new("a photograph is named by its content"));
        }
        if !valid_metadata(
            self.folder.as_deref(),
            &self.people(),
            &self.tags(),
            self.captured_at.as_deref(),
        ) {
            return Err(Refusal::new(
                "photo metadata is short, non-empty labels and a canonical ISO 8601 capture time",
            ));
        }
        if !self.edits.crop.is_valid_selection() || self.edits.rotate_quarters >= 4 {
            return Err(Refusal::new(
                "photo edits name a non-empty crop inside the image and a quarter turn from 0 to 3",
            ));
        }
        if self
            .location
            .as_ref()
            .is_some_and(|location| !location.is_valid())
        {
            return Err(Refusal::new(
                "a shared location needs finite WGS-84 coordinates, an explicit shared scope, and safe provenance",
            ));
        }
        if let Some(analysis) = &self.analysis() {
            if !valid_analysis(analysis) {
                return Err(Refusal::new(
                    "image analysis is compact, explicitly shared, and carries safe provenance",
                ));
            }
            if !analysis_content_matches(self, analysis)
                || (analysis.provenance.source.starts_with("local-")
                    && !analysis_names_exact_content(self, analysis))
            {
                return Err(Refusal::new(
                    "analysis provenance does not name this photograph's content",
                ));
            }
        }
        Ok(())
    }

    /// What an import accepts: a photograph kept in the content store,
    /// portable and bounded.
    pub(crate) fn importable(&self) -> Result<(), Refusal> {
        let dimensions = u64::from(self.image.width).saturating_mul(u64::from(self.image.height));
        if !valid_name(&self.image.name)
            || self.image.path.trim().is_empty()
            || self.image.path.chars().count() > MAX_RESOURCE_ID_CHARS
            || self.image.path.chars().any(char::is_control)
            || self.image.width == 0
            || self.image.height == 0
            || dimensions > crate::pixels::MAX_DECODED_PIXELS
            || !self.image.content.as_deref().is_some_and(is_content_hash)
        {
            return Err(Refusal::new(
                "a photograph is content-addressed, portable and bounded",
            ));
        }
        self.valid()
    }

    /// A short, deterministic description for resource trees and assistive
    /// technology.  Every field is optional, so an imported legacy record
    /// still has a useful result without pretending it has metadata.
    #[must_use]
    pub fn metadata_summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(folder) = &self.folder {
            parts.push(format!("Folder: {folder}"));
        }
        if !self.people.is_empty() {
            parts.push(format!("People: {}", self.people().join(", ")));
        }
        if !self.tags.is_empty() {
            parts.push(format!("Tags: {}", self.tags().join(", ")));
        }
        if let Some(captured_at) = &self.captured_at {
            parts.push(format!("Captured: {}", capture_label(captured_at)));
        }
        if let Some(location) = &self.location {
            parts.push(format!("Place: {}", location.display_name()));
        }
        if let Some(vision) = &self.vision
            && !vision.description.is_empty()
        {
            parts.push(format!("Description: {}", vision.description));
        }
        parts.join(". ")
    }

    /// A short provenance line for a gallery card. The details
    /// sheet retains every field and its labels; a shelf should let a person
    /// distinguish photographs before it starts truncating at "People:".
    #[must_use]
    pub fn metadata_card_summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(folder) = &self.folder {
            parts.push(folder.clone());
        }
        if let Some(person) = self.people.iter().next() {
            parts.push(person.clone());
        }
        if let Some(tag) = self.tags.iter().next() {
            parts.push(tag.clone());
        }
        if let Some(captured_at) = &self.captured_at {
            parts.push(captured_at.clone());
        }
        if let Some(location) = &self.location {
            parts.push(location.display_name());
        }
        if let Some(vision) = &self.vision
            && !vision.description.is_empty()
        {
            parts.push(vision.description.clone());
        }
        parts.join(", ")
    }

    /// Strip a content-addressed photograph's machine-local source location
    /// before it enters the shared library. The display name and content hash
    /// are sufficient for every collaborator; an absolute import path is not.
    #[must_use]
    pub fn portable(mut self) -> Self {
        self.image = self.image.portable();
        self
    }
}

/// A named collection of photographs by their stable ids, rather than path
/// strings, so peers refer to the same content wherever it was staged. It
/// shows them in the library's own browse order; filing and unfiling a
/// photograph merges per photograph with a collaborator's.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = Album::valid)]
pub struct Album {
    /// The album's stable id, `photos.album:…`.
    #[model(readonly)]
    pub id: String,
    /// Name
    #[model(label)]
    pub name: String,
    /// Photographs
    pub photo_ids: Set<String>,
}

impl Album {
    /// A named, empty album under `id`.
    #[must_use]
    pub fn named(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            photo_ids: Set::new(),
        }
    }

    /// The album's item id in the library.
    #[must_use]
    pub fn item(&self) -> Id {
        Id::from_raw(self.id.clone())
    }

    fn valid(&self) -> Result<(), Refusal> {
        if !valid_album_id(&self.id) || !valid_name(&self.name) || self.name.trim() != self.name {
            return Err(Refusal::new("an album needs an id and a name"));
        }
        if self.photo_ids.len() > MAX_ALBUM_MEMBERS {
            return Err(Refusal::new("an album holds at most 4096 photographs"));
        }
        Ok(())
    }
}

/// The library: every photograph, in the order it was imported, and the
/// albums that file them.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct PhotoLibrary {
    pub photos: Collection<LibraryPhoto>,
    pub albums: Collection<Album>,
}

/// A set of labels: people or tags.
pub fn labels<S: Into<String>>(labels: impl IntoIterator<Item = S>) -> Set<String> {
    labels.into_iter().map(Into::into).collect()
}

impl PhotoLibrary {
    /// A library holding `photos` and `albums`, in their order.
    #[must_use]
    pub fn of(photos: Vec<LibraryPhoto>, albums: Vec<Album>) -> Self {
        let mut library = Self::default();
        for photo in photos {
            library.add_photo(photo);
        }
        for album in albums {
            library.add_album(album);
        }
        library
    }

    pub fn photo(&self, id: &str) -> Option<&LibraryPhoto> {
        self.photos.get(&Id::from_raw(id))
    }

    pub fn photo_mut(&mut self, id: &str) -> Option<&mut LibraryPhoto> {
        self.photos.get_mut(&Id::from_raw(id))
    }

    pub fn album(&self, id: &str) -> Option<&Album> {
        self.albums.get(&Id::from_raw(id))
    }

    pub fn album_mut(&mut self, id: &str) -> Option<&mut Album> {
        self.albums.get_mut(&Id::from_raw(id))
    }

    /// Every photograph, in import order.
    pub fn all_photos(&self) -> impl DoubleEndedIterator<Item = &LibraryPhoto> + ExactSizeIterator {
        self.photos.values()
    }

    /// Every album, in the order they were made.
    pub fn all_albums(&self) -> impl DoubleEndedIterator<Item = &Album> + ExactSizeIterator {
        self.albums.values()
    }

    /// The photographs of `album` still in the library, in the library's
    /// order: a photograph removed while someone filed it is not shown.
    pub fn album_photos<'a>(&'a self, album: &'a Album) -> impl Iterator<Item = &'a str> + 'a {
        self.all_photos()
            .filter(|photo| album.photo_ids.contains(&photo.id))
            .map(|photo| photo.id.as_str())
    }

    /// Insert exactly once, preserving the existing metadata if a collaborator
    /// has already imported the same content.
    pub fn add_photo(&mut self, photo: LibraryPhoto) -> bool {
        if self.photo(&photo.id).is_some() {
            false
        } else {
            self.photos.push(photo.item(), photo);
            true
        }
    }

    /// Add `album` after the others.
    pub fn add_album(&mut self, album: Album) {
        self.albums.push(album.item(), album);
    }
}

/// A rectangle in normalized `0..1` coordinates of the image it applies to.
///
/// Normalized rather than pixels so a crop survives the source being replaced
/// by a different resolution of the same photograph, and so the value means
/// the same thing to a peer that has not fetched the bytes yet.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rect01 {
    #[schema(range(min = 0, max = 1))]
    pub x: f64,
    #[schema(range(min = 0, max = 1))]
    pub y: f64,
    #[schema(range(min = 0, max = 1))]
    pub w: f64,
    #[schema(range(min = 0, max = 1))]
    pub h: f64,
}

impl Default for Rect01 {
    fn default() -> Self {
        Self::FULL
    }
}

impl Rect01 {
    /// The whole image — the identity crop.
    pub const FULL: Self = Self {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    /// Whether this rect selects the entire image, within a tolerance that
    /// absorbs the float error of composing several crops.
    pub fn is_full(&self) -> bool {
        self.x.abs() < 1e-6
            && self.y.abs() < 1e-6
            && (self.w - 1.0).abs() < 1e-6
            && (self.h - 1.0).abs() < 1e-6
    }

    /// Whether this is a real crop contained in the original image.
    ///
    /// Unlike [`Self::clamped`], this validates an external command instead
    /// of silently changing its meaning. A durable action must either replay
    /// exactly what its author requested or be refused everywhere.
    pub fn is_valid_selection(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.w.is_finite()
            && self.h.is_finite()
            && self.x >= 0.0
            && self.y >= 0.0
            && self.w > 0.0
            && self.h > 0.0
            && self.x <= 1.0
            && self.y <= 1.0
            && self.x + self.w <= 1.0
            && self.y + self.h <= 1.0
    }

    /// Clamp into the unit square, keeping a non-negative extent. A rect
    /// arriving from a peer is untrusted input like any other.
    #[must_use]
    pub fn clamped(&self) -> Self {
        let x = self.x.clamp(0.0, 1.0);
        let y = self.y.clamp(0.0, 1.0);
        let w = self.w.max(0.0).min(1.0 - x);
        let h = self.h.max(0.0).min(1.0 - y);
        Self { x, y, w, h }
    }

    /// Compose `inner` — a rect expressed relative to the region *this* rect
    /// already selects — into a single rect in the original's coordinates.
    ///
    /// This is what makes a second crop mean "crop what I can currently see"
    /// rather than "crop the original again", which is the behaviour anyone
    /// cropping twice expects.
    #[must_use]
    pub fn compose(&self, inner: Rect01) -> Self {
        let outer = self.clamped();
        let inner = inner.clamped();
        Self {
            x: outer.x + inner.x * outer.w,
            y: outer.y + inner.y * outer.h,
            w: inner.w * outer.w,
            h: inner.h * outer.h,
        }
        .clamped()
    }
}

/// What the user has done to a photograph. Ordered `crop` then
/// `rotate`; see [`crate::pixels`] for why that order is fixed.
///
/// The crop and the turn are separate fields: a crop is one coherent
/// rectangle, while rotation is a separate decision, so the two merge
/// independently.
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Edits {
    /// Crop: the region of the *original* kept, normalized.
    pub crop: Rect01,
    /// Turn: clockwise quarter turns applied after the crop, `0..=3`.
    pub rotate_quarters: u8,
}

tilo_resource::json_model!(Rect01, PhotoLocation, OcrComponent, VisionComponent);

impl Edits {
    /// Whether the photograph is untouched — what the Reset affordance and the
    /// "edited" status line key off.
    pub fn is_identity(&self) -> bool {
        self.crop.is_full() && self.rotate_quarters.is_multiple_of(4)
    }

    /// Turn clockwise.
    #[must_use]
    pub fn rotated_right(&self) -> Self {
        Self {
            rotate_quarters: (self.rotate_quarters + 1) % 4,
            ..*self
        }
    }

    /// Turn anticlockwise.
    #[must_use]
    pub fn rotated_left(&self) -> Self {
        Self {
            rotate_quarters: (self.rotate_quarters + 3) % 4,
            ..*self
        }
    }

    /// Apply a crop taken from the currently displayed (rotated) view.
    ///
    /// The rect arrives in the rotated frame, is mapped back through the
    /// rotation, and is then composed onto whatever crop is already in force.
    #[must_use]
    pub fn cropped_from_view(&self, view_rect: Rect01) -> Self {
        let in_original = crate::pixels::unrotate_rect(view_rect, self.rotate_quarters);
        Self {
            crop: self.crop.compose(in_original),
            ..*self
        }
    }
}

/// The photograph currently open: its portable identity and dimensions.
///
/// Deliberately carries no bytes. The document names the original by content
/// hash; the bytes live in the app's content store (`tilo-content`) and the
/// pixels are decoded beside the document, never written into the CRDT: an
/// oversized change is dropped by the sync transport and freezes every peer's
/// replica at the change before it.
#[derive(Model, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpenImage {
    /// File name, as shown.
    pub name: String,
    /// Where the picture was opened from, for a picture with no content
    /// address; a content-addressed picture keeps only its name here, never
    /// an absolute origin path: its bytes resolve by [`Self::content`]
    /// through the content store on every device.
    pub path: String,
    /// Intrinsic pixel size of the *original*, before any edit.
    pub width: u32,
    pub height: u32,
    /// blake3 of the picture's bytes — the one name for it that means the
    /// same thing in every session.
    ///
    /// `#[serde(default)]` so a document written before this field existed
    /// still parses; such a picture has only its path, which is why the
    /// resolution order keeps working from one.
    #[serde(default)]
    pub content: Option<String>,
}

impl OpenImage {
    /// Derive the display name from a path.
    pub fn from_path(path: &str, width: u32, height: u32) -> Self {
        let name = std::path::Path::new(path)
            .file_name()
            .map_or_else(|| path.to_string(), |s| s.to_string_lossy().into_owned());
        Self {
            path: path.to_string(),
            name,
            width,
            height,
            content: None,
        }
    }

    /// Name the bytes this picture is, by their hash.
    ///
    /// Set wherever the picture is opened from bytes this machine has, which
    /// is also where those bytes are put into the content store — the two go
    /// together, because a hash in a document that no participant can resolve
    /// is worth less than the path it replaced.
    #[must_use]
    pub fn with_content(mut self, hash: impl Into<String>) -> Self {
        self.content = Some(hash.into());
        self
    }

    /// Drop the origin path from a content-addressed record before durable
    /// sharing. A legacy record has no portable byte identity, so retain its
    /// path as its explicit compatibility fallback.
    #[must_use]
    pub fn portable(mut self) -> Self {
        if self.content.is_some() {
            self.path.clone_from(&self.name);
        }
        self
    }

    /// Pixel size after `edits` — what the status line reports and what a
    /// caller asking the resource surface for dimensions gets back.
    pub fn edited_size(&self, edits: &Edits) -> (u32, u32) {
        let c = edits.crop.clamped();
        let w = (f64::from(self.width) * c.w).round().max(1.0) as u32;
        let h = (f64::from(self.height) * c.h).round().max(1.0) as u32;
        if edits.rotate_quarters % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// The bytes of the picture `open` names, from wherever this machine has them.
///
/// Content first: it is the portable identity every current import writes.
/// Path-only legacy records retain their old local fallback, but a new shared
/// record never causes this device to probe another participant's filesystem.
/// Resolving the hash is what turns "a photograph a collaborator opened" into
/// pixels here rather than into an empty canvas.
///
/// Missing both is not an error to report: content a collaborator imported may
/// simply not have reached this device's store yet, so the honest answer now is
/// no picture, and the moment after the bytes land there is one.
///
/// Public, and the only answer to this question, because the app is not the
/// only thing that asks it. Anything acting on the open photograph — the
/// editor drawing it, a participant with no window rendering the crop it was
/// asked for — needs the same bytes, and a second copy of "read the path" is
/// how one of those callers ends up reading a path that is true on another
/// machine.
pub fn bytes_of(open: &OpenImage, store: &tilo_content::ContentStore) -> Option<Vec<u8>> {
    if let Some(content) = &open.content {
        return crate::pixels::read_bounded(store.open(content)?, crate::pixels::MAX_ENCODED_BYTES);
    }
    crate::files::read_native(
        std::path::Path::new(&open.path),
        crate::pixels::MAX_ENCODED_BYTES as u64,
    )
}

/// A stored capture time as people read it: the ISO separator between the
/// date and the time becomes a space (`2026-07-12 18:16`). Anything else is
/// shown as stored.
pub(crate) fn capture_label(captured_at: &str) -> String {
    match (
        captured_at.get(..10),
        captured_at.get(10..11),
        captured_at.get(11..),
    ) {
        (Some(day), Some("T"), Some(time))
            if !time.is_empty()
                && crate::derived::timeline_day_from_capture(captured_at).is_some() =>
        {
            format!("{day} {time}")
        }
        _ => captured_at.to_owned(),
    }
}

/// Whether `value` names a photograph: not blank, bounded, no control
/// characters.
pub(crate) fn valid_resource_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_RESOURCE_ID_CHARS
        && !value.chars().any(char::is_control)
}

/// Whether `value` is a content hash: 64 lowercase hex digits.
pub(crate) fn is_content_hash(value: &str) -> bool {
    value.len() == CONTENT_HEX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn valid_name(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_NAME_CHARS
        && !value.chars().any(char::is_control)
}

pub(crate) fn valid_album_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("photos.album:") else {
        return false;
    };
    !suffix.is_empty()
        && value.chars().count() <= MAX_ALBUM_ID_CHARS
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(crate) fn valid_analysis(analysis: &ImageAnalysis) -> bool {
    fn compact_text(value: &str, max: usize) -> bool {
        value.chars().count() <= max && !value.chars().any(char::is_control)
    }
    fn labels(values: &[String]) -> bool {
        values.len() <= MAX_ANALYSIS_LABELS
            && values.iter().all(|value| {
                !value.trim().is_empty() && compact_text(value, MAX_ANALYSIS_LABEL_CHARS)
            })
            && values
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == values.len()
    }
    fn valid_provenance(provenance: &crate::model::AnalysisProvenance) -> bool {
        matches!(provenance.scope.as_str(), "shared")
            && matches!(
                provenance.source.as_str(),
                "local-ocr" | "local-vision" | "local-description" | "user"
            )
            && compact_text(&provenance.engine, 128)
            && compact_text(&provenance.model_id, 128)
            && (provenance.model_revision.is_empty()
                || (matches!(provenance.model_revision.len(), 40 | 64)
                    && provenance
                        .model_revision
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())))
            && (provenance.source != "local-vision"
                || (!provenance.model_id.is_empty()
                    && !provenance.model_revision.is_empty()
                    && provenance.engine == "tilo-vision"))
            && compact_text(&provenance.language, 16)
            && (provenance.content.is_empty()
                || (provenance.content.len() == CONTENT_HEX_LEN
                    && provenance
                        .content
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))))
    }
    let valid_ocr = analysis.ocr_text.chars().count() <= MAX_ANALYSIS_TEXT_CHARS
        && analysis
            .ocr_text
            .chars()
            .all(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'));
    (!analysis.is_empty()
        || analysis.ocr_provenance.is_some()
        || analysis.vision_provenance.is_some())
        && valid_ocr
        && compact_text(&analysis.description, 1_024)
        && labels(&analysis.objects)
        && labels(&analysis.pets)
        && valid_provenance(&analysis.provenance)
        && analysis.ocr_provenance.as_ref().is_none_or(|provenance| {
            valid_provenance(provenance)
                && matches!(
                    provenance.source.as_str(),
                    "local-ocr" | "local-vision" | "user"
                )
        })
        && analysis
            .vision_provenance
            .as_ref()
            .is_none_or(|provenance| {
                valid_provenance(provenance)
                    && matches!(
                        provenance.source.as_str(),
                        // `local-ocr` is retained only for pre-component
                        // combined records that were valid in older Photos.
                        "local-ocr" | "local-vision" | "local-description" | "user"
                    )
            })
}

fn analysis_content_matches(photo: &LibraryPhoto, analysis: &ImageAnalysis) -> bool {
    [
        Some(&analysis.provenance),
        analysis.ocr_provenance.as_ref(),
        analysis.vision_provenance.as_ref(),
    ]
    .into_iter()
    .flatten()
    .all(|provenance| {
        provenance.content.is_empty()
            || photo.image.content.as_deref() == Some(provenance.content.as_str())
    })
}

pub(crate) fn analysis_names_exact_content(photo: &LibraryPhoto, analysis: &ImageAnalysis) -> bool {
    let Some(content) = photo.image.content.as_deref() else {
        return false;
    };
    [
        Some(&analysis.provenance),
        analysis.ocr_provenance.as_ref(),
        analysis.vision_provenance.as_ref(),
    ]
    .into_iter()
    .flatten()
    .all(|provenance| provenance.content == content)
}

pub(crate) fn merge_image_analysis(
    existing: Option<&ImageAnalysis>,
    incoming: ImageAnalysis,
    content: Option<&str>,
) -> ImageAnalysis {
    let mut merged = existing.cloned().unwrap_or_default();
    let component_is_current = |provenance: &crate::model::AnalysisProvenance| {
        provenance.content.is_empty() || Some(provenance.content.as_str()) == content
    };
    let ocr_provenance = merged.ocr_provenance.as_ref().unwrap_or(&merged.provenance);
    if !merged.ocr_text.is_empty() && !component_is_current(ocr_provenance) {
        merged.ocr_text.clear();
        merged.ocr_provenance = None;
    }
    let vision_provenance = merged
        .vision_provenance
        .as_ref()
        .unwrap_or(&merged.provenance);
    if (!merged.description.is_empty() || !merged.objects.is_empty() || !merged.pets.is_empty())
        && !component_is_current(vision_provenance)
    {
        merged.description.clear();
        merged.objects.clear();
        merged.pets.clear();
        merged.vision_provenance = None;
    }
    // Hydrate component provenance for pre-component records before changing
    // anything. This prevents a new OCR result from making an older visual
    // description look as though OCR produced it (and vice versa).
    if merged.ocr_provenance.is_none() && !merged.ocr_text.is_empty() {
        merged.ocr_provenance = Some(merged.provenance.clone());
    }
    if merged.vision_provenance.is_none()
        && (!merged.description.is_empty() || !merged.objects.is_empty() || !merged.pets.is_empty())
        && matches!(
            merged.provenance.source.as_str(),
            "local-vision" | "local-description" | "user"
        )
    {
        merged.vision_provenance = Some(merged.provenance.clone());
    }
    match incoming.provenance.source.as_str() {
        "local-ocr" => {
            merged.ocr_text = incoming.ocr_text;
            merged.ocr_provenance = incoming
                .ocr_provenance
                .or_else(|| Some(incoming.provenance.clone()));
        }
        "local-vision" | "local-description" => {
            merged.description = incoming.description;
            merged.objects = incoming.objects;
            merged.pets = incoming.pets;
            // A dedicated OCR result wins over incidental vision OCR. Vision
            // still refreshes OCR produced by an older vision pass, including
            // clearing text the current model no longer observes.
            let dedicated_ocr = merged.ocr_provenance.as_ref().is_some_and(|provenance| {
                matches!(provenance.source.as_str(), "local-ocr" | "user")
            });
            if !dedicated_ocr && incoming.provenance.source == "local-vision" {
                merged.ocr_text = incoming.ocr_text;
                merged.ocr_provenance = incoming
                    .ocr_provenance
                    .or_else(|| (!merged.ocr_text.is_empty()).then(|| incoming.provenance.clone()));
            }
            merged.vision_provenance = incoming
                .vision_provenance
                .or_else(|| Some(incoming.provenance.clone()));
        }
        _ => return incoming,
    }
    // Keep the legacy primary field meaningful for older peers while the
    // component fields retain the complete, non-lossy audit trail.
    merged.provenance = merged
        .vision_provenance
        .clone()
        .or_else(|| merged.ocr_provenance.clone())
        .unwrap_or(incoming.provenance);
    merged
}

/// Keep user-authored metadata compact enough that a library remains a small
/// CRDT record, and reject (rather than normalize) malformed remote commands
/// so every replica either applies exactly the same labels or refuses them.
pub(crate) fn valid_metadata(
    folder: Option<&str>,
    people: &[String],
    tags: &[String],
    captured_at: Option<&str>,
) -> bool {
    fn text(value: &str) -> bool {
        !value.trim().is_empty() && value.chars().count() <= 128
    }
    fn labels(values: &[String]) -> bool {
        values.len() <= 32
            && values.iter().all(|value| text(value))
            && values
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == values.len()
    }
    folder.is_none_or(text)
        && captured_at.is_none_or(canonical_capture_at)
        && labels(people)
        && labels(tags)
}

pub(crate) fn unique_strings(values: &[String]) -> bool {
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    values.iter().all(|value| seen.insert(value.as_str()))
}

/// Return the one wire spelling Photos accepts for a capture value.
///
/// Date-only values deliberately remain date-only; local date-times remain
/// local (there is no invented timezone); and offset date-times retain both
/// their authored UTC offset and their fractional-second precision.  This is
/// a validation boundary, not a display formatter, so a collaborator cannot
/// turn an unknown local time into a different instant by merely opening it.
pub fn canonical_capture_at(value: &str) -> bool {
    canonical_capture_at_value(value).as_deref() == Some(value)
}

pub(crate) fn canonical_capture_at_value(value: &str) -> Option<String> {
    if value.chars().count() > 128 || value.chars().any(char::is_control) || !value.is_ascii() {
        return None;
    }
    // EXIF's `YYYY:MM:DD HH:MM:SS` and a legacy space separator are accepted
    // once at the action boundary and committed as ISO. All durable values
    // therefore converge to one form.
    let mut canonical = value.to_owned();
    if canonical.as_bytes().get(4) == Some(&b':') && canonical.as_bytes().get(7) == Some(&b':') {
        canonical.replace_range(4..5, "-");
        canonical.replace_range(7..8, "-");
    }
    if canonical.as_bytes().get(10) == Some(&b' ') {
        canonical.replace_range(10..11, "T");
    }
    if canonical.len() == 10 {
        return valid_capture_date(&canonical).then_some(canonical);
    }
    let (body, offset) = if let Some(body) = canonical.strip_suffix('Z') {
        (body, Some("Z"))
    } else if let Some((body, suffix)) = canonical
        .len()
        .checked_sub(6)
        .and_then(|split| canonical.split_at_checked(split))
        && matches!(suffix.as_bytes().first(), Some(b'+' | b'-'))
        && suffix.as_bytes().get(3) == Some(&b':')
    {
        (body, Some(suffix))
    } else {
        (canonical.as_str(), None)
    };
    if body.len() < 16
        || !body.get(..10).is_some_and(valid_capture_date)
        || body.as_bytes().get(10) != Some(&b'T')
    {
        return None;
    }
    let (whole_seconds, _fraction) = match body.split_once('.') {
        Some((whole, fraction))
            if fraction.len() <= 9
                && !fraction.is_empty()
                && fraction.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            (whole, Some(fraction.len()))
        }
        Some(_) => return None,
        None => (body, None),
    };
    let minute_precision = whole_seconds.len() == 16;
    let second_precision = whole_seconds.len() == 19;
    if !minute_precision && !second_precision {
        return None;
    }
    // An offset identifies an instant, so require seconds in its wire format.
    if offset.is_some() && !second_precision {
        return None;
    }
    if !valid_capture_time(whole_seconds, second_precision) {
        return None;
    }
    if let Some(offset) = offset
        && offset != "Z"
        && !valid_capture_offset(offset)
    {
        return None;
    }
    let mut result = body.to_owned();
    if offset == Some("+00:00") || offset == Some("-00:00") || offset == Some("Z") {
        result.push('Z');
    } else if let Some(offset) = offset {
        result.push_str(offset);
    }
    Some(result)
}

fn decimal(value: &[u8]) -> Option<u32> {
    (!value.is_empty() && value.iter().all(u8::is_ascii_digit))
        .then(|| std::str::from_utf8(value).ok()?.parse().ok())
        .flatten()
}

fn valid_capture_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return false;
    }
    let (Some(year), Some(month), Some(day)) = (
        bytes.get(..4).and_then(decimal),
        bytes.get(5..7).and_then(decimal),
        bytes.get(8..).and_then(decimal),
    ) else {
        return false;
    };
    (1..=12).contains(&month)
        && (1..=ui::date::days_in_month(i64::from(year), month)).contains(&day)
}

fn valid_capture_time(value: &str, has_seconds: bool) -> bool {
    let bytes = value.as_bytes();
    let needed = if has_seconds { 19 } else { 16 };
    if bytes.len() != needed
        || bytes.get(13) != Some(&b':')
        || (has_seconds && bytes.get(16) != Some(&b':'))
    {
        return false;
    }
    let hour = bytes.get(11..13).and_then(decimal);
    let minute = bytes.get(14..16).and_then(decimal);
    let second = has_seconds
        .then(|| bytes.get(17..19).and_then(decimal))
        .flatten();
    hour.is_some_and(|value| value <= 23)
        && minute.is_some_and(|value| value <= 59)
        && (!has_seconds || second.is_some_and(|value| value <= 59))
}

fn valid_capture_offset(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 6
        || !matches!(bytes.first(), Some(b'+' | b'-'))
        || bytes.get(3) != Some(&b':')
    {
        return false;
    }
    bytes
        .get(1..3)
        .and_then(decimal)
        .is_some_and(|hour| hour <= 23)
        && bytes
            .get(4..)
            .and_then(decimal)
            .is_some_and(|minute| minute <= 59)
}

/// A person is a compact user-authored label.  This intentionally has the
/// same limit and character rules as the `people` metadata array so a review
/// action can never create a record an ordinary metadata edit would reject.
pub(crate) fn valid_person_label(value: &str) -> bool {
    !value.trim().is_empty()
        && value.trim().chars().count() <= 128
        && !value.chars().any(char::is_control)
}

/// An explicit field-level decision in an atomic multi-photo metadata edit.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MetadataPatch<T> {
    /// Preserve the value already stored on each addressed photograph.
    Keep,
    /// Remove the value (or empty the complete label list).
    Clear,
    /// Replace the value with this explicit final value.
    Set(T),
}

/// A field-level list decision for people and tags.
///
/// `Add` and `Remove` are intentionally separate from `Set`: a batch editor
/// can add one label to hundreds of photographs without replacing labels a
/// collaborator added while the sheet was open.  The action still carries the
/// complete bounded target selection, so a stale selection refuses atomically.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MetadataLabelsPatch {
    Keep,
    Clear,
    Set(Vec<String>),
    Add(Vec<String>),
    Remove(Vec<String>),
}

/// One save of the details editor over a selection of photographs: per
/// field, keep what each has, clear it, or set it; people and tags may also
/// be added to or removed from what each has.
#[derive(Clone, Debug, PartialEq)]
pub struct MetadataBatch {
    pub folder: MetadataPatch<String>,
    pub people: MetadataLabelsPatch,
    pub tags: MetadataLabelsPatch,
    pub captured_at: MetadataPatch<String>,
    pub location: MetadataPatch<PhotoLocation>,
}

/// The paths of the photograph `id` in the library.
#[must_use]
pub fn photo_paths(id: &str) -> <LibraryPhoto as Model>::Paths<PhotoLibrary> {
    PhotoLibrary::paths().photos.item(&Id::from_raw(id))
}

/// The paths of the album `id` in the library.
#[must_use]
pub fn album_paths(id: &str) -> <Album as Model>::Paths<PhotoLibrary> {
    PhotoLibrary::paths().albums.item(&Id::from_raw(id))
}

impl PhotoLibrary {
    /// The field edits that save `batch` on the photographs `ids`: each
    /// label added or removed on its own, so a collaborator's label added
    /// meanwhile stays.
    ///
    /// # Errors
    /// Refuses a selection that is empty, too large or names a photograph
    /// that is gone, and a capture time that is not ISO 8601.
    pub fn metadata_edits(
        &self,
        ids: &[String],
        batch: &MetadataBatch,
    ) -> Result<Vec<tilo_resource::Edit<Self>>, Refusal> {
        if ids.is_empty() || ids.len() > crate::resource::MAX_BATCH_PHOTOS || !unique_strings(ids) {
            return Err(Refusal::new(
                "a details edit names each photograph once, at most 256",
            ));
        }
        let captured_at = match &batch.captured_at {
            MetadataPatch::Set(value) => MetadataPatch::Set(
                canonical_capture_at_value(value)
                    .ok_or("the capture time is not a valid ISO 8601 value")?,
            ),
            other => other.clone(),
        };
        let mut edits = Vec::new();
        for id in ids {
            let photo = self
                .photo(id)
                .ok_or("a selected photograph is no longer in the library")?;
            let paths = photo_paths(id);
            match &batch.folder {
                MetadataPatch::Keep => {}
                MetadataPatch::Clear => edits.push(paths.folder.set(None)),
                MetadataPatch::Set(value) => edits.push(paths.folder.set(Some(value.clone()))),
            }
            for (now, patch, path) in [
                (&photo.people, &batch.people, &paths.people),
                (&photo.tags, &batch.tags, &paths.tags),
            ] {
                let next = label_patch(now, patch);
                edits.extend(
                    now.iter()
                        .filter(|label| !next.contains(label))
                        .map(|label| path.remove(label)),
                );
                edits.extend(
                    next.iter()
                        .filter(|label| !now.contains(label))
                        .map(|label| path.insert(label)),
                );
            }
            match &captured_at {
                MetadataPatch::Keep => {}
                MetadataPatch::Clear => edits.push(paths.captured_at.set(None)),
                MetadataPatch::Set(value) => {
                    edits.push(paths.captured_at.set(Some(value.clone())));
                }
            }
            match &batch.location {
                MetadataPatch::Keep => {}
                MetadataPatch::Clear => edits.push(paths.location.set(None)),
                MetadataPatch::Set(value) => edits.push(paths.location.set(Some(value.clone()))),
            }
        }
        Ok(edits)
    }
}

/// The labels `now` becomes under `patch`.
fn label_patch(now: &Set<String>, patch: &MetadataLabelsPatch) -> Vec<String> {
    match patch {
        MetadataLabelsPatch::Keep => now.iter().cloned().collect(),
        MetadataLabelsPatch::Clear => Vec::new(),
        MetadataLabelsPatch::Set(values) => values.clone(),
        MetadataLabelsPatch::Add(values) => now
            .iter()
            .cloned()
            .chain(values.iter().filter(|value| !now.contains(value)).cloned())
            .collect(),
        MetadataLabelsPatch::Remove(values) => now
            .iter()
            .filter(|value| !values.contains(value))
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn capture_times_read_with_a_space_not_the_iso_separator() {
        assert_eq!(capture_label("2026-07-12T18:16"), "2026-07-12 18:16");
        assert_eq!(capture_label("2026-07-12"), "2026-07-12");
        assert_eq!(capture_label("June 2026"), "June 2026");
    }

    use super::*;

    #[test]
    fn analysis_text_eligibility_and_legacy_provenance() {
        assert_eq!(ImageAnalysis::default().searchable_text(), "");
        let analysis = ImageAnalysis {
            ocr_text: "  MARKET  ".to_owned(),
            objects: vec!["sign".to_owned()],
            pets: vec!["  ".to_owned()],
            ..ImageAnalysis::default()
        };
        assert_eq!(analysis.searchable_text(), "MARKET sign");

        // Vision eligibility requires matching content and model revision.
        let provenance = |source: &str, model_revision: String| AnalysisProvenance {
            source: source.into(),
            model_revision,
            content: "a".repeat(64),
            ..Default::default()
        };
        let analysis = ImageAnalysis {
            ocr_text: "receipt".into(),
            provenance: provenance("local-ocr", String::new()),
            ocr_provenance: Some(provenance("local-ocr", String::new())),
            ..Default::default()
        };
        assert!(!analysis.has_vision_component(Some(&"a".repeat(64)), &"b".repeat(64)));
        let analysis = ImageAnalysis {
            vision_provenance: Some(provenance("local-vision", "b".repeat(64))),
            ..analysis
        };
        // (content, model revision, eligible)
        for (content, revision, eligible) in
            [('a', 'b', true), ('c', 'b', false), ('a', 'd', false)]
        {
            assert_eq!(
                analysis.has_vision_component(
                    Some(&content.to_string().repeat(64)),
                    &revision.to_string().repeat(64)
                ),
                eligible,
                "content {content} revision {revision}"
            );
        }

        let legacy = r#"{
            "source":"local-ocr",
            "engine":"tilo-ocr",
            "language":"eng",
            "content":"abc",
            "scope":"shared"
        }"#;
        let provenance: AnalysisProvenance = serde_json::from_str(legacy).unwrap();
        assert!(provenance.model_id.is_empty());
        assert!(provenance.model_revision.is_empty());
    }

    fn rect(left: f64, top: f64, width: f64, height: f64) -> Rect01 {
        Rect01 {
            x: left,
            y: top,
            w: width,
            h: height,
        }
    }

    #[test]
    fn edits_compose_rotate_crop_and_clamp_into_the_unit_square() {
        assert!(Edits::default().is_identity());
        let mut e = Edits::default();
        for _ in 0..4 {
            e = e.rotated_right();
        }
        assert!(e.is_identity(), "four right turns");
        assert!(
            Edits::default()
                .rotated_right()
                .rotated_left()
                .is_identity(),
            "left and right cancel"
        );

        let half = rect(0.0, 0.0, 0.5, 0.5);
        let out = half.compose(half);
        assert!((out.w - 0.25).abs() < 1e-9, "{out:?}");
        assert!((out.h - 0.25).abs() < 1e-9, "{out:?}");
        let right_half = rect(0.5, 0.0, 0.5, 1.0);
        let out = right_half.compose(right_half);
        assert!((out.x - 0.75).abs() < 1e-9, "{out:?}");
        assert!((out.w - 0.25).abs() < 1e-9, "{out:?}");

        let r = rect(-0.5, 0.9, 4.0, 4.0).clamped();
        assert!(r.x >= 0.0 && r.y >= 0.0);
        assert!(r.x + r.w <= 1.0 + 1e-9, "{r:?}");
        assert!(r.y + r.h <= 1.0 + 1e-9, "{r:?}");
    }

    #[test]
    fn open_images_name_themselves_and_size_by_their_edits() {
        let img = OpenImage::from_path("/var/tmp/tilo/home/quinten/harbour.png", 1, 1);
        assert_eq!(img.name, "harbour.png");

        let img = OpenImage::from_path("/x/harbour.png", 800, 400);
        for (case, edits, size) in [
            ("identity", Edits::default(), (800, 400)),
            (
                "odd turn swaps the axes",
                Edits::default().rotated_right(),
                (400, 800),
            ),
            (
                "crop shrinks",
                Edits {
                    crop: rect(0.0, 0.0, 0.5, 0.5),
                    rotate_quarters: 0,
                },
                (400, 200),
            ),
        ] {
            assert_eq!(img.edited_size(&edits), size, "case {case}");
        }
    }
}
