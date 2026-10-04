//! Pure data model: the edit state a photograph carries, and nothing else.
//!
//! Every type here is plain data plus serde. The document maps durable library
//! records field-by-field (and keeps the open-editor values compact); bytes
//! never enter Automerge — see [`crate::doc`] for the content-addressed
//! boundary.

use serde::{Deserialize, Serialize};

/// Bounded, explainable intelligence derived on a device from a photograph.
///
/// This record contains no pixels, embeddings, face geometry, machine-local
/// file names, or opaque model output.  A local analysis is private until a
/// user or trusted caller explicitly publishes it through `SetImageAnalysis`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Non-biometric, user-visible person labels. These are names only when a
    /// person deliberately supplied a name; recognition never invents one.
    #[serde(default)]
    pub people: Vec<String>,
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

/// The bounded provenance attached to an analysis result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
            &self.people.join(" "),
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
            && self.people.is_empty()
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// A durable entry in Photos' shared library. Pixel bytes remain in the
/// content store; this is intentionally only addressable metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryPhoto {
    pub id: String,
    pub image: OpenImage,
    #[serde(default)]
    pub favorite: bool,
    /// A user-organized folder label. This is deliberately metadata rather
    /// than an on-disk directory: a shared path only has meaning on the
    /// machine that happened to import the picture.
    #[serde(default)]
    pub folder: Option<String>,
    /// Named people visible in the photograph.  These are compact labels, not
    /// biometric data or a second copy of the image.
    #[serde(default)]
    pub people: Vec<String>,
    /// Freeform, durable search labels.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional user-supplied capture date/time text.  Keeping the original
    /// string avoids silently changing a collaborator's timezone or precision.
    #[serde(default)]
    pub captured_at: Option<String>,
    /// Optional explicit location. Absence means no location was shared — it
    /// is not a failed lookup and never triggers a network request.
    #[serde(default)]
    pub location: Option<PhotoLocation>,
    /// Explicitly shared intelligence. Device-local analysis belongs in the
    /// caller's ephemeral cache until this compact record is intentionally
    /// accepted through a canonical action.
    #[serde(default)]
    pub analysis: Option<ImageAnalysis>,
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
            image,
            favorite: false,
            folder: None,
            people: Vec::new(),
            tags: Vec::new(),
            captured_at: None,
            location: None,
            analysis: None,
        }
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
            parts.push(format!("People: {}", self.people.join(", ")));
        }
        if !self.tags.is_empty() {
            parts.push(format!("Tags: {}", self.tags.join(", ")));
        }
        if let Some(captured_at) = &self.captured_at {
            parts.push(format!("Captured: {captured_at}"));
        }
        if let Some(location) = &self.location {
            parts.push(format!("Place: {}", location.display_name()));
        }
        if let Some(analysis) = &self.analysis
            && !analysis.description.is_empty()
        {
            parts.push(format!("Description: {}", analysis.description));
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
        if let Some(person) = self.people.first() {
            parts.push(person.clone());
        }
        if let Some(tag) = self.tags.first() {
            parts.push(tag.clone());
        }
        if let Some(captured_at) = &self.captured_at {
            parts.push(captured_at.clone());
        }
        if let Some(location) = &self.location {
            parts.push(location.display_name());
        }
        if let Some(analysis) = &self.analysis
            && !analysis.description.is_empty()
        {
            parts.push(analysis.description.clone());
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

/// A named collection of stable photo ids. Membership is ordered for a calm,
/// predictable browse order and uses ids rather than path strings so peers
/// refer to the same content wherever it was staged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub photo_ids: Vec<String>,
}

/// The complete small metadata namespace Photos replicates.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PhotoLibrary {
    #[serde(default)]
    pub photos: Vec<LibraryPhoto>,
    #[serde(default)]
    pub albums: Vec<Album>,
}

impl PhotoLibrary {
    pub fn photo(&self, id: &str) -> Option<&LibraryPhoto> {
        self.photos.iter().find(|photo| photo.id == id)
    }

    pub fn photo_mut(&mut self, id: &str) -> Option<&mut LibraryPhoto> {
        self.photos.iter_mut().find(|photo| photo.id == id)
    }

    pub fn album(&self, id: &str) -> Option<&Album> {
        self.albums.iter().find(|album| album.id == id)
    }

    pub fn album_mut(&mut self, id: &str) -> Option<&mut Album> {
        self.albums.iter_mut().find(|album| album.id == id)
    }

    /// Insert exactly once, preserving the existing metadata if a collaborator
    /// has already imported the same content.
    pub fn add_photo(&mut self, photo: LibraryPhoto) -> bool {
        if self.photo(&photo.id).is_some() {
            false
        } else {
            self.photos.push(photo);
            true
        }
    }

    /// The document projection intentionally contains only portable metadata.
    /// Existing path-only records remain readable for backward compatibility,
    /// but every content-addressed record written from this version drops its
    /// origin-machine path.
    #[must_use]
    pub fn portable(&self) -> Self {
        Self {
            photos: self
                .photos
                .iter()
                .cloned()
                .map(LibraryPhoto::portable)
                .collect(),
            albums: self.albums.clone(),
        }
    }
}

/// A rectangle in normalized `0..1` coordinates of the image it applies to.
///
/// Normalized rather than pixels so a crop survives the source being replaced
/// by a different resolution of the same photograph, and so the value means
/// the same thing to a peer that has not fetched the bytes yet.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect01 {
    pub x: f64,
    pub y: f64,
    pub w: f64,
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

/// What the user has done to the open photograph. Ordered `crop` then
/// `rotate`; see [`crate::pixels`] for why that order is fixed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Edits {
    /// Region of the *original* kept, normalized.
    pub crop: Rect01,
    /// Clockwise quarter turns applied after the crop, `0..=3`.
    pub rotate_quarters: u8,
}

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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenImage {
    /// Legacy path or portable display-name hint.
    ///
    /// Path-only records written before content staging keep their existing
    /// fallback path. New content-addressed records write only [`Self::name`]
    /// here, never an absolute origin path; bytes resolve by [`Self::content`]
    /// through the content store on every device.
    pub path: String,
    /// File name, as shown.
    pub name: String,
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

#[cfg(test)]
mod tests {
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
