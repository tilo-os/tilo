//! Pure data model: the edit state a photograph carries, and nothing else.
//!
//! Every type here is plain data plus serde, so the whole of what the user has
//! decided round-trips through Automerge scalars. The bytes never do — see
//! [`crate::doc`] for why the document holds a path and a pair of edit fields
//! rather than an image.

use serde::{Deserialize, Serialize};

/// A durable entry in Photos' shared library. Pixel bytes remain in the
/// content store; this is intentionally only addressable metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryPhoto {
    pub id: String,
    pub image: OpenImage,
    #[serde(default)]
    pub favorite: bool,
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
        }
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

    pub fn album(&self, id: &str) -> Option<&Album> {
        self.albums.iter().find(|album| album.id == id)
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

/// The photograph currently open: where it came from and how big it is.
///
/// Deliberately carries no bytes. The document names the file; the pixels are
/// loaded beside it and are never written into the CRDT — the same separation
/// `crates/doc/qdoc/src/blobstore.rs` enforces for a document's images, and for
/// the same reason: an oversized change is dropped by the sync transport and
/// freezes every peer's replica at the change before it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenImage {
    /// Absolute path the bytes were read from, on the machine that opened it.
    ///
    /// A hint, not the picture's identity. This document is shared, and the
    /// path in it is true of exactly one participant: the same photograph
    /// handed to a room is staged under a different absolute path in every
    /// session that receives it, so whichever session's path ends up in the
    /// document is a path the others cannot read. Resolve [`Self::content`]
    /// first and treat this as the local shortcut it is.
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
    fn a_fresh_edit_set_is_the_identity() {
        assert!(Edits::default().is_identity());
    }

    #[test]
    fn rotating_four_times_returns_to_the_identity() {
        let mut e = Edits::default();
        for _ in 0..4 {
            e = e.rotated_right();
        }
        assert!(e.is_identity());
    }

    #[test]
    fn left_and_right_cancel() {
        let e = Edits::default().rotated_right().rotated_left();
        assert!(e.is_identity());
    }

    #[test]
    fn composing_two_half_crops_takes_a_quarter() {
        let half = Rect01 {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        };
        let out = half.compose(half);
        assert!((out.w - 0.25).abs() < 1e-9, "{out:?}");
        assert!((out.h - 0.25).abs() < 1e-9, "{out:?}");
    }

    #[test]
    fn composing_offsets_into_the_outer_rect() {
        let outer = Rect01 {
            x: 0.5,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        };
        let inner = Rect01 {
            x: 0.5,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        };
        let out = outer.compose(inner);
        assert!((out.x - 0.75).abs() < 1e-9, "{out:?}");
        assert!((out.w - 0.25).abs() < 1e-9, "{out:?}");
    }

    #[test]
    fn clamping_pulls_an_out_of_range_rect_inside_the_unit_square() {
        let r = Rect01 {
            x: -0.5,
            y: 0.9,
            w: 4.0,
            h: 4.0,
        }
        .clamped();
        assert!(r.x >= 0.0 && r.y >= 0.0);
        assert!(r.x + r.w <= 1.0 + 1e-9, "{r:?}");
        assert!(r.y + r.h <= 1.0 + 1e-9, "{r:?}");
    }

    #[test]
    fn edited_size_swaps_the_axes_on_an_odd_turn() {
        let img = OpenImage::from_path("/x/harbour.png", 800, 400);
        assert_eq!(img.edited_size(&Edits::default()), (800, 400));
        assert_eq!(
            img.edited_size(&Edits::default().rotated_right()),
            (400, 800)
        );
    }

    #[test]
    fn edited_size_shrinks_with_the_crop() {
        let img = OpenImage::from_path("/x/harbour.png", 800, 400);
        let e = Edits {
            crop: Rect01 {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 0.5,
            },
            rotate_quarters: 0,
        };
        assert_eq!(img.edited_size(&e), (400, 200));
    }

    #[test]
    fn a_name_is_derived_from_the_path() {
        let img = OpenImage::from_path("/var/tmp/qos/home/quinten/harbour.png", 1, 1);
        assert_eq!(img.name, "harbour.png");
    }
}
