//! The markup of one PDF: what is stored, and what the reader reads of it.
//!
//! [`PdfMarkup`] is the record — the markup a launch derives from the file
//! being read — so page numbers, which only mean anything within one file,
//! are only ever read against the file they came from. What is
//! *collaborative* lives here: the page plan, annotations, threaded
//! comments, signatures and form values. The PDF itself is an immutable blob
//! the record names by content ([`PdfMarkup::source`]); every annotation is
//! relative to those bytes.
//!
//! **Anchors are stable.** Markup names its page by durable occurrence id,
//! never by position, so reordering pages, deleting them and annotating
//! concurrently compose. Geometry is stored in the page's PDF user space,
//! which does not change however the page is displayed or rotated; text
//! markup also keeps the engine character range it covers.
//!
//! **Concurrency.** Pages are items with stable ids and order keys: a page
//! deleted while someone moves it stays deleted. Annotations, comments and
//! signatures are items under their own ids; their fields merge one by one.
//! Comment threads are a storage unit of their own, so the right to comment
//! is the right to write them.
//!
//! **Markup already in the PDF is taken over.** The first time a record
//! meets its source bytes, the file's highlights, notes and their reply
//! threads, free text, ink and form values become records like any other;
//! [`PdfMarkup::adopted`] remembers where each came from, so an export
//! keeps, replaces or removes the originals instead of adding copies — see
//! [`crate::adopt`].
//!
//! The reading position is here as a resume hint — the page occurrence,
//! how far down it the reader was, and the zoom — so reopening a document
//! returns to where it was left. It is a hint, not live view state: each
//! window scrolls on its own, and a peer's last position is simply the one
//! a fresh window starts from. The active tool, selection and panels remain
//! per-peer and are never written here.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{
    self, Bytes, Collection, Id, Identified, JsonSchema, Keyed, Model, Refusal,
};

use crate::adopt::AdoptedEntry;
use crate::model::{
    AnnoKind, Annotation, Comment, FormEntry, GeomSpace, PtRect, Signature, SignatureInk, Thread,
};

/// The most pages one import brings in.
pub const MAX_IMPORTED_PAGES: usize = 512;

/// A page occurrence from an imported payload. Dimensions are captured at
/// authoring time so a peer can reserve an honest page-shaped placeholder
/// before it has downloaded the blob.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportedPage {
    pub hash: String,
    pub kind: String,
    pub page: usize,
    pub width_milli: u32,
    pub height_milli: u32,
}

impl ImportedPage {
    pub fn valid(&self) -> bool {
        self.hash.len() == 64
            && self.hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            && matches!(self.kind.as_str(), "pdf" | "image")
            && self.page > 0
            && self.width_milli > 0
            && self.height_milli > 0
            && self.width_milli <= 100_000_000
            && self.height_milli <= 100_000_000
    }
}

tilo_resource::json_model!(
    ImportedPage,
    AnnoKind,
    PtRect,
    SignatureInk,
    FormEntry,
    AdoptedEntry
);

/// A durable page-plan entry, as the reader works with it: the occurrence
/// `id`, the immutable source page it shows, and its turn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PagePlanEntry {
    pub id: String,
    pub source_page: usize,
    /// `None` denotes a page of the file which was opened; a populated value
    /// names bytes in the app's content store by hash.
    pub imported: Option<ImportedPage>,
    /// Clockwise rotation (0, 90, 180 or 270) added to the source page's own.
    #[serde(default)]
    pub rotation: i32,
}

/// One page of the plan, as stored: an item of [`PdfMarkup::pages`] under
/// its occurrence id.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = PageOccurrence::valid)]
pub struct PageOccurrence {
    /// The page of its PDF it shows, 1-based.
    #[model(readonly)]
    pub source_page: usize,
    /// The bytes it comes from when it is not the opened file's.
    #[model(readonly)]
    pub imported: Option<ImportedPage>,
    /// Turn: clockwise, a multiple of 90 degrees.
    pub rotation: i32,
}

impl PageOccurrence {
    fn valid(&self) -> Result<(), Refusal> {
        if self.source_page == 0
            || self.imported.as_ref().is_some_and(|page| !page.valid())
            || !matches!(self.rotation, 0 | 90 | 180 | 270)
        {
            return Err(Refusal::new(
                "a page shows a page of its PDF, turned by a multiple of 90 degrees",
            ));
        }
        Ok(())
    }
}

/// One placed signature as stored.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = StoredSignature::valid)]
pub struct StoredSignature {
    #[model(atom, readonly)]
    pub id: String,
    /// The page occurrence it is on.
    #[model(atom)]
    pub page_id: String,
    /// Where it is on the page.
    pub rect: PtRect,
    /// Who signed.
    #[model(readonly)]
    pub signer: String,
    /// When they signed, as they wrote it.
    #[model(readonly)]
    pub date: String,
    /// What was signed: the typed name or the strokes drawn.
    #[model(readonly)]
    pub ink: SignatureInk,
    /// Which space `rect` is in.
    pub space: GeomSpace,
}

impl Identified for StoredSignature {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

impl StoredSignature {
    fn valid(&self) -> Result<(), Refusal> {
        let r = self.rect;
        if ![r.x, r.y, r.w, r.h].iter().all(|n| n.is_finite()) || self.ink.is_empty() {
            return Err(Refusal::new("a signature has a place and some ink"));
        }
        Ok(())
    }
}

/// The markup of one PDF. See the module docs.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = PdfMarkup::valid)]
pub struct PdfMarkup {
    /// The PDF bytes the markup is made against. The first pin wins: a
    /// record never silently re-targets its markup at different bytes.
    #[model(readonly)]
    pub source: Bytes,
    /// The file name the source was opened under; empty when unnamed.
    #[model(readonly)]
    pub source_name: String,
    /// Whether the page plan was made from the source.
    #[model(readonly)]
    pub page_plan_seeded: bool,
    /// The source whose own markup this record has taken over; empty
    /// until it has.
    #[model(atom, readonly)]
    pub imported_source: String,
    /// The pages, in reading order.
    pub pages: Collection<PageOccurrence>,
    /// Annotations: text markup, ink, notes and free text.
    pub annotations: Keyed<Annotation>,
    /// Comment threads, in the order they were written.
    #[model(unit)]
    pub comments: Collection<Comment>,
    /// Signatures
    pub signatures: Keyed<StoredSignature>,
    /// Form values, by the field's fully qualified name.
    pub form_values: BTreeMap<String, FormEntry>,
    /// Where taken-over annotations came from, by record id.
    #[model(readonly)]
    pub adopted: BTreeMap<String, AdoptedEntry>,
    /// The page occurrence the reader last left off on; empty before the
    /// first position is recorded.
    #[model(atom)]
    pub reading_page: String,
    /// How far down that page the reader was, from 0 (top) to 1 (foot).
    pub reading_fraction: f64,
    /// The zoom factor the reader had there.
    pub reading_zoom: f64,
}

/// The PDF bytes a record's markup is made against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedSource {
    pub hash: String,
    pub len: u64,
    /// The file name it was opened under; empty when it was not named.
    pub name: String,
}

impl PdfMarkup {
    /// The markup `doc` holds; none when it holds none that reads.
    #[must_use]
    pub fn of(doc: &tilo_resource::Doc) -> Self {
        <Self as tilo_resource::Resource>::read(doc).unwrap_or_default()
    }

    /// Apply `invoke` to `doc` through the door, as a replica of it does.
    #[cfg(test)]
    pub(crate) fn apply_to(
        doc: &mut tilo_resource::Doc,
        invoke: impl Into<tilo_resource::Invoke<Self>>,
    ) {
        let mut replica = tilo_resource::Replica::<Self>::from_doc(std::mem::take(doc));
        replica.apply(invoke).expect("the change applies");
        *doc = std::mem::take(replica.doc_mut());
    }

    /// A record holding a plan of `pages` source pages, as bytes every test
    /// replica loads: the shared genesis two peers start from.
    #[cfg(test)]
    pub(crate) fn planned(pages: usize) -> Vec<u8> {
        let mut replica = tilo_resource::Replica::<Self>::new();
        replica
            .apply(Self::START_PAGE_PLAN.with(pages))
            .expect("the plan");
        replica.save()
    }

    /// The quarter turn page occurrence `id` shows at.
    #[must_use]
    pub fn page_rotation(&self, id: &str) -> i32 {
        self.pages
            .get(&Id::from_raw(id))
            .map_or(0, |page| page.rotation)
    }

    fn valid(&self) -> Result<(), Refusal> {
        if self.page_plan_seeded && self.pages.is_empty() {
            return Err(Refusal::new("a document keeps at least one page"));
        }
        if !self.reading_page.is_empty()
            && (!self.reading_fraction.is_finite()
                || !(0.0..=1.0).contains(&self.reading_fraction)
                || !self.reading_zoom.is_finite()
                || self.reading_zoom <= 0.0)
        {
            return Err(Refusal::new(
                "a reading position is a page, a fraction of it and a positive zoom",
            ));
        }
        Ok(())
    }

    /// The PDF bytes the markup is made against, once pinned.
    #[must_use]
    pub fn pinned_source(&self) -> Option<PinnedSource> {
        match &self.source {
            Bytes::Blob { hash, len } => Some(PinnedSource {
                hash: hex::encode(hash),
                len: *len,
                name: self.source_name.clone(),
            }),
            _ => None,
        }
    }

    /// The source whose markup has been taken over.
    #[must_use]
    pub fn imported_source(&self) -> Option<&str> {
        (!self.imported_source.is_empty()).then_some(self.imported_source.as_str())
    }

    /// The page plan, in reading order.
    #[must_use]
    pub fn page_plan(&self) -> Vec<PagePlanEntry> {
        self.pages
            .iter()
            .map(|(id, page)| PagePlanEntry {
                id: id.as_str().to_owned(),
                source_page: page.source_page,
                imported: page.imported.clone(),
                rotation: page.rotation,
            })
            .collect()
    }

    /// Where the reader last left off, when a position was recorded.
    #[must_use]
    pub fn reading_position(&self) -> Option<(String, f64, f64)> {
        (!self.reading_page.is_empty()).then(|| {
            (
                self.reading_page.clone(),
                self.reading_fraction,
                self.reading_zoom,
            )
        })
    }

    /// Page occurrence id → 1-based displayed page number.
    fn page_numbers(&self) -> HashMap<&str, usize> {
        self.pages
            .ids()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index + 1))
            .collect()
    }

    /// The occurrence shown as page `page` (1-based).
    #[must_use]
    pub fn page_id(&self, page: usize) -> Option<String> {
        self.pages
            .at(page.checked_sub(1)?)
            .map(|(id, _)| id.as_str().to_owned())
    }

    /// The occurrence annotation `id` is on.
    #[must_use]
    pub fn annotation_page_id(&self, id: &str) -> Option<String> {
        self.annotations
            .find(&Id::from_raw(id))
            .map(|annotation| annotation.page_id.clone())
    }

    /// Annotations on pages that are part of the document now. Those on a
    /// deleted page stay in the record but are not listed until the page
    /// comes back.
    #[must_use]
    pub fn list_annotations(&self) -> Vec<Annotation> {
        self.annotations_with_hidden()
            .into_iter()
            .filter(|annotation| annotation.page > 0)
            .collect()
    }

    /// Every annotation in the record, those on a deleted page with
    /// `page == 0`.
    #[must_use]
    pub fn annotations_with_hidden(&self) -> Vec<Annotation> {
        let numbers = self.page_numbers();
        self.annotations
            .iter()
            .map(|stored| Annotation {
                page: numbers.get(stored.page_id.as_str()).copied().unwrap_or(0),
                ..stored.clone()
            })
            .collect()
    }

    /// The comment threads on pages that are part of the document now, in
    /// the order they were written.
    #[must_use]
    pub fn list_comments(&self) -> Vec<Thread> {
        let numbers = self.page_numbers();
        self.comments
            .iter()
            .filter_map(|(id, comment)| {
                Some(Thread {
                    id: id.as_str().to_owned(),
                    page: *numbers.get(comment.anchor.page_id.as_str())?,
                    comment: comment.clone(),
                })
            })
            .collect()
    }

    /// The signatures on pages that are part of the document now.
    #[must_use]
    pub fn list_signatures(&self) -> Vec<Signature> {
        let numbers = self.page_numbers();
        self.signatures
            .iter()
            .filter_map(|stored| {
                Some(Signature {
                    id: stored.id.clone(),
                    page: *numbers.get(stored.page_id.as_str())?,
                    rect: stored.rect,
                    signer: stored.signer.clone(),
                    date: stored.date.clone(),
                    ink: stored.ink.clone(),
                    space: stored.space,
                })
            })
            .collect()
    }

    /// Every taken-over annotation's origin.
    #[must_use]
    pub fn adopted_entries(&self) -> Vec<AdoptedEntry> {
        self.adopted.values().cloned().collect()
    }
}

/// The 32 bytes a 64-digit hex hash names.
pub(crate) fn unhex(hash: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(hash, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests;
