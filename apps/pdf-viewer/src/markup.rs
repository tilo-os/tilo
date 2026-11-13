//! The markup of one PDF: the page plan, annotations, comment threads, signatures and
//! form values, against the immutable bytes the record names. Markup names its page by
//! durable occurrence id, never by position, and stores geometry in the page's PDF user
//! space, so reordering, deleting and annotating concurrently compose. The reading
//! position is a resume hint only; the tool, selection and panels stay per window.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{
    self, Bytes, Collection, Id, Identified, JsonSchema, Keyed, Model, Refusal,
};

use crate::adopt::AdoptedEntry;
use crate::model::{
    AnnoKind, Annotation, Comment, FormEntry, GeomSpace, PtRect, Signature, SignatureInk, Thread,
};

pub const MAX_IMPORTED_PAGES: usize = 512;

/// A page occurrence from an imported payload, with its dimensions captured at
/// authoring time so a peer can reserve a page-shaped placeholder before the blob arrives.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportedPage {
    /// The payload's content reference: the address it is kept under, and the
    /// key that opens it when the workspace keeps content sealed.
    pub hash: String,
    pub kind: String,
    pub page: usize,
    pub width_milli: u32,
    pub height_milli: u32,
}

impl ImportedPage {
    pub fn valid(&self) -> bool {
        tilo_content::valid_reference(&self.hash)
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
    /// The bytes, kept where this window keeps content: its hash and the
    /// reference that opens it.
    pub source: Bytes,
    /// The file name it was opened under; empty when it was not named.
    pub name: String,
}

impl PinnedSource {
    /// The bytes as `store` holds them; `None` while they are not here.
    #[must_use]
    pub fn bytes(&self, store: &tilo_content::ContentStore) -> Option<Vec<u8>> {
        self.source.read(store)
    }

    /// The bytes' content hash, which names them the same everywhere.
    #[must_use]
    pub fn hash(&self) -> String {
        hex::encode(self.source.hash())
    }
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
        (!self.source.is_empty()).then(|| PinnedSource {
            source: self.source.clone(),
            name: self.source_name.clone(),
        })
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

#[cfg(test)]
mod tests;
