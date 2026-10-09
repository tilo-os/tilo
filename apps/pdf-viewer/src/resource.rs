//! The markup's verbs, and the app.
//!
//! Plain fields change through the built-in field edits every path offers
//! (`PdfMarkup::paths().annotations.item(&id).color.set(..)`): recolouring
//! and moving an annotation, a free text's box and words, resolving and
//! rewording a comment, form values, a page's turn, moving a page. The
//! methods below are the operations that are more than setting a field:
//! adding what someone writes (signed by them), deleting what takes other
//! things with it, the page plan's duplicates and imports, and what the
//! reader records when it first meets a file.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{
    EffectAnswer, EffectCx, Fixture, LaunchInput, LiveRow, ResourceApp, ResourceSurface, RunMode,
    SurfaceCx, ViewCx,
};
use ui::record::RecordId;
use ui::tilo_resource::{
    self, ActionCx, Bytes, Id, Invoke, Json, JsonSchema, Refusal, SearchEntry, resource,
};
use ui::widget::Element;

use crate::actions::PdfEffect;
use crate::app::PdfApp;
use crate::markup::{
    ImportedPage, MAX_IMPORTED_PAGES, PageOccurrence, PagePlanEntry, PdfMarkup, StoredAnnotation,
    StoredComment, StoredReply, StoredSignature,
};
use crate::model::{
    AnnoKind, FormEntry, GeomSpace, MARGIN, PAGE_H, PAGE_W, PtRect, SignatureInk, Swatch,
};
use crate::scenarios::SampleDocument;

/// The most text one comment or reply holds.
const MAX_COMMENT_CHARS: usize = 100_000;

/// An annotation someone makes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NewAnnotation {
    /// The id it is addressed by.
    pub id: String,
    /// The page occurrence it is on.
    pub page_id: String,
    /// What it marks, and where.
    pub mark: AnnoKind,
    pub color: Swatch,
    /// The engine character range `[start, end)` text markup covers.
    pub chars: Option<[u32; 2]>,
    /// The space its geometry is in.
    pub space: GeomSpace,
}

/// A sticky note someone makes: a note on the page and the comment thread
/// pinned to it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NewNote {
    /// The id of the note on the page.
    pub annotation: String,
    /// The id of its comment thread.
    pub comment: String,
    /// The page occurrence it is on.
    pub page_id: String,
    pub x: f64,
    pub y: f64,
    pub color: Swatch,
    /// What it says.
    pub text: String,
    /// The space `x` and `y` are in.
    pub space: GeomSpace,
}

/// A comment someone writes: free on a page, or on an annotation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NewComment {
    /// The id it is addressed by.
    pub id: String,
    /// The annotation it is about; it is on that annotation's page.
    pub anno: Option<String>,
    /// The page occurrence it is on, when it is about no annotation.
    pub page_id: Option<String>,
    /// What it says, as plain text.
    pub text: String,
    /// Its paragraphs as written; none for plain text.
    #[serde(default)]
    pub rich: Vec<tilo_document::ClipBlock>,
}

/// A signature someone places.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NewSignature {
    /// The id it is addressed by.
    pub id: String,
    /// The page occurrence it is on.
    pub page_id: String,
    pub rect: PtRect,
    /// The name signed; the signer's own when empty.
    pub signer: String,
    /// When it was signed, as written beside it.
    pub date: String,
    pub ink: SignatureInk,
    pub space: GeomSpace,
}

/// An annotation the PDF already carried, as taken over.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AdoptedAnnotation {
    pub id: String,
    pub page_id: String,
    pub mark: AnnoKind,
    pub color: Swatch,
    pub author: String,
    pub created_ms: i64,
}

/// A reply the PDF already carried.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AdoptedReply {
    pub id: String,
    pub author: String,
    pub text: String,
}

/// A comment thread the PDF already carried.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AdoptedThread {
    pub id: String,
    pub anno: Option<String>,
    pub page_id: String,
    pub author: String,
    pub text: String,
    pub created: i64,
    pub replies: Vec<AdoptedReply>,
}

/// The markup a PDF already carries, as a record takes it over.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExistingMarkup {
    pub annotations: Vec<AdoptedAnnotation>,
    pub comments: Vec<AdoptedThread>,
    pub form_values: std::collections::BTreeMap<String, FormEntry>,
    pub adopted: Vec<crate::adopt::AdoptedEntry>,
}

/// The reader's Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[("en-US", include_str!("../locales/en-US.ftl"))];

static CATALOG: ui::i18n::EmbeddedCatalog =
    ui::i18n::EmbeddedCatalog::new("en-US", LOCALES, "pdf-viewer-history-scope")
        .with_system_locale();

#[resource(
    kind = "pdf.document",
    label = "PDF",
    search = PdfMarkup::entries
)]
impl PdfMarkup {
    /// Make the page plan.
    ///
    /// The pages of the opened file, in their order, the first time a
    /// record meets it. Page ids are deterministic, so two people opening
    /// the same file agree.
    #[action(external)]
    pub fn start_page_plan(
        &mut self,
        #[param("How many pages the PDF has")] pages: usize,
    ) -> Result<(), Refusal> {
        if self.page_plan_seeded {
            return Ok(());
        }
        // Pages imported before the plan was made keep their place.
        let imported = std::mem::take(&mut self.pages);
        for source_page in 1..=pages.max(1) {
            let id = Id::from_raw(format!("source-{source_page}"));
            self.pages.push(
                id,
                PageOccurrence {
                    source_page,
                    ..PageOccurrence::default()
                },
            );
        }
        for (id, page) in imported.iter() {
            self.pages.push(id.clone(), page.clone());
        }
        self.page_plan_seeded = true;
        Ok(())
    }

    /// Pin the source.
    ///
    /// Names the PDF bytes the markup is made against by their content. The
    /// first pin of a record wins: it never re-targets its markup.
    #[action(external)]
    pub fn pin_source(
        &mut self,
        #[param("The content hash of the PDF bytes")] hash: String,
        #[param("How many bytes they are")] len: u64,
        #[param("The file's name")] name: String,
    ) -> Result<(), Refusal> {
        if !self.source.is_empty() {
            return Ok(());
        }
        let hash = crate::markup::unhex(&hash.to_ascii_lowercase())
            .ok_or("a source is named by its 64-digit content hash")?;
        self.source = Bytes::Blob { hash, len };
        self.source_name = name;
        Ok(())
    }

    /// Take over the PDF's own markup.
    ///
    /// The highlights, notes and their replies, free text, ink and form
    /// values the source bytes already carry become records like any other,
    /// once per source. Their ids derive from the source, so two people
    /// taking it over at once agree.
    #[action(external, writes = comments)]
    pub fn adopt_existing(
        &mut self,
        #[param("The content hash of the source the markup is in")] source: String,
        #[param("The markup it carries")] markup: ExistingMarkup,
    ) -> Result<(), Refusal> {
        if self.imported_source() == Some(source.as_str()) {
            return Ok(());
        }
        if markup.annotations.len() > crate::adopt::MAX_IMPORTED_ANNOTATIONS
            || markup.adopted.len() > 4 * crate::adopt::MAX_IMPORTED_ANNOTATIONS
        {
            return Err(Refusal::new(
                "the PDF carries more markup than one import takes",
            ));
        }
        for annotation in markup.annotations {
            let id = Id::from_raw(annotation.id.clone());
            if self.annotations.find(&id).is_none() {
                self.annotations.push(StoredAnnotation {
                    id: annotation.id,
                    page_id: annotation.page_id,
                    mark: annotation.mark,
                    color: annotation.color,
                    author: annotation.author,
                    chars: None,
                    created_ms: annotation.created_ms,
                    space: GeomSpace::User,
                });
            }
        }
        for thread in markup.comments {
            if self
                .comments
                .find(&Id::from_raw(thread.id.clone()))
                .is_some()
            {
                continue;
            }
            let mut replies = tilo_resource::Collection::new();
            for reply in thread.replies {
                replies.push(
                    Id::from_raw(reply.id),
                    StoredReply {
                        author: reply.author,
                        text: reply.text,
                        rich: Json(Vec::new()),
                    },
                );
            }
            self.comments.push(StoredComment {
                id: thread.id,
                anno: thread.anno.unwrap_or_default(),
                page_id: thread.page_id,
                author: thread.author,
                text: thread.text,
                rich: Json(Vec::new()),
                created: thread.created,
                resolved: false,
                replies,
            });
        }
        for (name, value) in markup.form_values {
            self.form_values.entry(name).or_insert(value);
        }
        for entry in markup.adopted {
            self.adopted.insert(entry.id.clone(), entry);
        }
        self.imported_source = source;
        Ok(())
    }

    /// Import pages.
    ///
    /// Pages of other PDFs or pictures, by content, after a page (first when
    /// none is named). Their bytes are in the content store, never here.
    #[action(writes = comments)]
    pub fn import_pages(
        &mut self,
        #[param("The page they go after; the start when none")] after: Option<String>,
        #[param("The pages, each with the id it is addressed by")] pages: Vec<PagePlanEntry>,
    ) -> Result<(), Refusal> {
        if pages.is_empty() || pages.len() > MAX_IMPORTED_PAGES {
            return Err(Refusal::new("an import brings 1 to 512 pages"));
        }
        let at = match &after {
            Some(after) => {
                self.pages
                    .position(&Id::from_raw(after.clone()))
                    .ok_or("there is no such page to import after")?
                    + 1
            }
            None => self.pages.len(),
        };
        for (at, page) in (at..).zip(pages) {
            let id = Id::from_raw(page.id.clone());
            if page.id.is_empty() || page.id.len() > 160 || self.pages.contains(&id) {
                return Err(Refusal::new("each imported page has a new id"));
            }
            let imported: Option<ImportedPage> = page.imported;
            if imported.is_none() {
                return Err(Refusal::new(
                    "an imported page names the bytes it comes from",
                ));
            }
            self.pages.insert(
                at,
                id,
                PageOccurrence {
                    source_page: page.source_page,
                    imported,
                    rotation: 0,
                },
            );
        }
        Ok(())
    }

    /// Duplicate page.
    ///
    /// Another occurrence of the page right after it. Markup stays with the
    /// page it was made on.
    #[action(item = pages)]
    pub fn duplicate_page(
        &mut self,
        cx: &mut ActionCx,
        page: &Id,
        #[param("The id the copy is addressed by; a new one when none")] copy: Option<String>,
    ) -> Result<(), Refusal> {
        let at = self.pages.position(page).ok_or(NO_PAGE)?;
        let original = self.pages.get(page).ok_or(NO_PAGE)?.clone();
        let copy = Id::from_raw(copy.unwrap_or_else(|| format!("page-{}", cx.uuid())));
        if copy.as_str().is_empty() || self.pages.contains(&copy) {
            return Err(Refusal::new("the copy needs an id of its own"));
        }
        self.pages.insert(
            at + 1,
            copy,
            PageOccurrence {
                rotation: 0,
                ..original
            },
        );
        Ok(())
    }

    /// Delete page.
    ///
    /// Its markup stays in the record, hidden, and comes back with the page.
    /// The last page is never deleted.
    #[action(item = pages)]
    pub fn delete_page(&mut self, page: &Id) -> Result<(), Refusal> {
        if self.pages.len() <= 1 {
            return Err(Refusal::new("a document keeps at least one page"));
        }
        self.pages
            .remove(page)
            .map(drop)
            .ok_or_else(|| Refusal::new(NO_PAGE))
    }

    /// Move page.
    ///
    /// After another page, or to the start.
    #[action(item = pages)]
    pub fn move_page(
        &mut self,
        page: &Id,
        #[param("The page it goes after; the start when none")] after: Option<String>,
    ) -> Result<(), Refusal> {
        let from = self.pages.position(page).ok_or(NO_PAGE)?;
        let to = match after {
            Some(after) => {
                let after = self
                    .pages
                    .position(&Id::from_raw(after))
                    .ok_or("there is no such page to move after")?;
                if after < from { after + 1 } else { after }
            }
            None => 0,
        };
        self.pages.move_to(page, to);
        Ok(())
    }

    /// Rotate page.
    ///
    /// A quarter turn clockwise.
    #[action(item = pages)]
    pub fn rotate_page(&mut self, page: &Id) -> Result<(), Refusal> {
        let page = self.pages.get_mut(page).ok_or(NO_PAGE)?;
        page.rotation = (page.rotation + 90).rem_euclid(360);
        Ok(())
    }

    /// Add annotation.
    ///
    /// Text markup, ink, a note or free text on a page, signed by whoever
    /// adds it.
    #[action(writes = comments)]
    pub fn add_annotation(
        &mut self,
        cx: &mut ActionCx,
        #[param("The annotation")] annotation: NewAnnotation,
    ) -> Result<(), Refusal> {
        let id = Id::from_raw(annotation.id.clone());
        if self.annotations.find(&id).is_some() {
            return Ok(());
        }
        self.on_a_page(&annotation.page_id)?;
        self.annotations.push(StoredAnnotation {
            id: annotation.id,
            page_id: annotation.page_id,
            mark: annotation.mark,
            color: annotation.color,
            author: cx.author().unwrap_or_default().to_owned(),
            chars: annotation.chars,
            created_ms: cx.now_ms(),
            space: annotation.space,
        });
        Ok(())
    }

    /// Delete annotation.
    ///
    /// The comments pinned to it go with it, so a deleted note leaves no
    /// thread behind.
    #[action(item = annotations, writes = comments)]
    pub fn delete_annotation(&mut self, annotation: &Id) -> Result<(), Refusal> {
        self.annotations
            .take(annotation)
            .ok_or("there is no such annotation")?;
        self.comments
            .retain(|comment| comment.anno != annotation.as_str());
        Ok(())
    }

    /// Add note.
    ///
    /// A sticky note on a page and the comment thread pinned to it, signed
    /// by whoever adds it.
    #[action(writes = comments)]
    pub fn create_note(
        &mut self,
        cx: &mut ActionCx,
        #[param("The note")] note: NewNote,
    ) -> Result<(), Refusal> {
        self.on_a_page(&note.page_id)?;
        let author = cx.author().unwrap_or_default().to_owned();
        let now = cx.now_ms();
        check_text(&note.text, &[])?;
        if self
            .annotations
            .find(&Id::from_raw(note.annotation.clone()))
            .is_some()
        {
            return Ok(());
        }
        self.annotations.push(StoredAnnotation {
            id: note.annotation.clone(),
            page_id: note.page_id.clone(),
            mark: AnnoKind::Note {
                x: note.x,
                y: note.y,
            },
            color: note.color,
            author: author.clone(),
            chars: None,
            created_ms: now,
            space: note.space,
        });
        self.comments.push(StoredComment {
            id: note.comment,
            anno: note.annotation,
            page_id: note.page_id,
            author,
            text: note.text,
            rich: Json(Vec::new()),
            created: now,
            resolved: false,
            replies: tilo_resource::Collection::new(),
        });
        Ok(())
    }

    /// Add a note at the top of a page.
    ///
    /// A sticky note with this text, where a reader looks first; for the
    /// palette, keyboard users and agents.
    #[action(writes = comments)]
    pub fn add_note(
        &mut self,
        cx: &mut ActionCx,
        #[param("What the note says")] text: String,
        #[param("Page (1 when omitted)")] page: Option<usize>,
    ) -> Result<(), Refusal> {
        let page_id = self.page_id(page.unwrap_or(1)).ok_or(NO_PAGE)?;
        let annotation = cx.uuid().to_string();
        let comment = cx.uuid().to_string();
        self.create_note(
            cx,
            NewNote {
                annotation,
                comment,
                page_id,
                x: MARGIN,
                y: MARGIN,
                color: Swatch::Yellow,
                text,
                space: GeomSpace::Page,
            },
        )
    }

    /// Add comment.
    ///
    /// Free on a page, or about an annotation, signed by whoever writes it.
    #[action(field = "comments", writes = comments)]
    pub fn add_comment(
        &mut self,
        cx: &mut ActionCx,
        #[param("The comment")] comment: NewComment,
    ) -> Result<(), Refusal> {
        check_text(&comment.text, &comment.rich)?;
        if self
            .comments
            .find(&Id::from_raw(comment.id.clone()))
            .is_some()
        {
            return Ok(());
        }
        let page_id = match (&comment.anno, &comment.page_id) {
            (Some(anno), _) => self
                .annotation_page_id(anno)
                .ok_or("there is no such annotation")?,
            (None, Some(page_id)) => {
                self.on_a_page(page_id)?;
                page_id.clone()
            }
            (None, None) => return Err(Refusal::new("a comment is on a page or an annotation")),
        };
        self.comments.push(StoredComment {
            id: comment.id,
            anno: comment.anno.unwrap_or_default(),
            page_id,
            author: cx.author().unwrap_or_default().to_owned(),
            text: comment.text,
            rich: Json(comment.rich),
            created: cx.now_ms(),
            resolved: false,
            replies: tilo_resource::Collection::new(),
        });
        Ok(())
    }

    /// Reply.
    ///
    /// Adds an answer to the thread, signed by whoever writes it.
    #[action(item = comments, field = "comments", writes = comments)]
    pub fn reply(
        &mut self,
        cx: &mut ActionCx,
        comment: &Id,
        #[param("What it says")] text: String,
        #[param("The id the reply is addressed by; a new one when none")] id: Option<String>,
        #[param("Its paragraphs as written; none for plain text")] rich: Option<
            Vec<tilo_document::ClipBlock>,
        >,
    ) -> Result<(), Refusal> {
        let rich = rich.unwrap_or_default();
        check_text(&text, &rich)?;
        let id = id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| cx.uuid().to_string());
        let author = cx.author().unwrap_or_default().to_owned();
        let thread = self
            .comments
            .find_mut(comment)
            .ok_or("there is no such comment")?;
        let id = Id::from_raw(id);
        if !thread.replies.contains(&id) {
            thread.replies.push(
                id,
                StoredReply {
                    author,
                    text,
                    rich: Json(rich),
                },
            );
        }
        Ok(())
    }

    /// Delete comment.
    ///
    /// A comment pinning a sticky note takes the note with it.
    #[action(item = comments, writes = comments)]
    pub fn delete_comment(&mut self, comment: &Id) -> Result<(), Refusal> {
        let thread = self
            .comments
            .take(comment)
            .ok_or("there is no such comment")?;
        if !thread.anno.is_empty() {
            self.delete_annotation(&Id::from_raw(thread.anno)).ok();
        }
        Ok(())
    }

    /// Sign.
    ///
    /// Places a signature on a page.
    #[action]
    pub fn add_signature(
        &mut self,
        cx: &mut ActionCx,
        #[param("The signature")] signature: NewSignature,
    ) -> Result<(), Refusal> {
        self.on_a_page(&signature.page_id)?;
        if self
            .signatures
            .find(&Id::from_raw(signature.id.clone()))
            .is_some()
        {
            return Ok(());
        }
        let signer = if signature.signer.trim().is_empty() {
            cx.author().unwrap_or_default().to_owned()
        } else {
            signature.signer
        };
        self.signatures.push(StoredSignature {
            id: signature.id,
            page_id: signature.page_id,
            rect: signature.rect,
            signer,
            date: signature.date,
            ink: signature.ink,
            space: signature.space,
        });
        Ok(())
    }

    /// Sign with a typed name.
    ///
    /// A typed signature dated today, where a page is signed: for the
    /// palette, keyboard users and agents.
    #[action]
    pub fn add_typed_signature(
        &mut self,
        cx: &mut ActionCx,
        #[param("The name signed; your own when omitted")] signer: Option<String>,
        #[param("Page (1 when omitted)")] page: Option<usize>,
    ) -> Result<(), Refusal> {
        let page_id = self.page_id(page.unwrap_or(1)).ok_or(NO_PAGE)?;
        let id = cx.uuid().to_string();
        let date = crate::app::ymd_from_unix_ms(cx.now_ms());
        self.add_signature(
            cx,
            NewSignature {
                id,
                page_id,
                // A familiar signing place, clear of the opening paragraph.
                rect: PtRect::new(PAGE_W - MARGIN - 218.0, PAGE_H - MARGIN - 74.0, 218.0, 74.0),
                signer: signer.unwrap_or_default().trim().to_owned(),
                date,
                ink: SignatureInk::Typed,
                space: GeomSpace::Page,
            },
        )
    }

    /// Delete signature.
    #[action(item = signatures, writes = comments)]
    pub fn delete_signature(&mut self, signature: &Id) -> Result<(), Refusal> {
        self.signatures
            .take(signature)
            .map(drop)
            .ok_or_else(|| Refusal::new("there is no such signature"))
    }
}

/// Why a verb on a page is refused when it is gone.
const NO_PAGE: &str = "there is no such page";

impl PdfMarkup {
    /// Refuse a page that is not part of the document.
    fn on_a_page(&self, page_id: &str) -> Result<(), Refusal> {
        if self.pages.contains(&Id::from_raw(page_id)) {
            Ok(())
        } else {
            Err(Refusal::new(NO_PAGE))
        }
    }

    /// The markup people made, each entry anchored at the item it is.
    fn entries(&self) -> Vec<SearchEntry> {
        let anchor = |field: &str, id: &str| {
            tilo_resource::items::item_resource_id::<PdfMarkup>(field, &Id::from_raw(id))
        };
        let mut entries: Vec<SearchEntry> = self
            .list_annotations()
            .into_iter()
            .map(|annotation| {
                SearchEntry::new(
                    format!("{} annotation", annotation.kind.type_key()),
                    format!(
                        "Page {} annotated by {}",
                        annotation.page, annotation.author
                    ),
                )
                .anchored(anchor("annotations", &annotation.id))
            })
            .collect();
        for comment in self.list_comments() {
            let replies: Vec<&str> = comment
                .replies
                .iter()
                .map(|reply| reply.body.as_str())
                .collect();
            entries.push(
                SearchEntry::new(
                    format!("Comment — {}", comment.author),
                    std::iter::once(comment.body.as_str())
                        .chain(replies)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                .anchored(anchor("comments", &comment.id)),
            );
        }
        for signature in self.list_signatures() {
            entries.push(
                SearchEntry::new(
                    format!("Signature by {}", signature.signer),
                    format!("Page {} signed on {}", signature.page, signature.date),
                )
                .anchored(anchor("signatures", &signature.id)),
            );
        }
        entries
    }
}

/// A comment or reply's text: `rich`, when given, matches its plain text;
/// bounded, with paragraph kinds and marks a document can hold.
fn check_text(text: &str, rich: &[tilo_document::ClipBlock]) -> Result<(), Refusal> {
    if text.chars().count() > MAX_COMMENT_CHARS {
        return Err(Refusal::new("the comment is too long"));
    }
    if rich.is_empty() {
        return Ok(());
    }
    if rich.len() > 1_000 || crate::actions::plain_of(rich) != text {
        return Err(Refusal::new(
            "the comment's paragraphs do not match its text",
        ));
    }
    let safe_name = |name: &str| {
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    };
    let valid = rich.iter().all(|clip| {
        let len = clip.text.chars().count();
        safe_name(&clip.kind)
            && clip.level.is_none_or(|level| (0..=9).contains(&level))
            && clip.attrs.len() <= 16
            && clip.attrs.keys().all(|key| safe_name(key))
            && clip
                .marks
                .iter()
                .all(|mark| safe_name(&mark.name) && mark.start < mark.end && mark.end <= len)
    });
    if valid {
        Ok(())
    } else {
        Err(Refusal::new("the comment's paragraphs are not valid"))
    }
}

/// The PDF reader: one record per document holds its markup, and the
/// reader is the surface over it.
pub struct PdfViewerShell;

impl ResourceApp for PdfViewerShell {
    type Resource = PdfMarkup;
    const ID: &'static str = "so.tilo.pdf-viewer";
    const TITLE: &'static str = "PDF Viewer";
    const ICON: Option<&'static str> = Some("pdf-viewer");
    const OPENS: &'static [&'static str] = &["application/pdf"];
    /// The PDF to open (`pdf-viewer paper.pdf`).
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<PdfMarkup>) -> Element {
        let content = cx.content().unwrap_or_else(crate::app::default_content);
        let app = if cx.mode() == RunMode::Past {
            // A past version shows its markup's record, never a document:
            // History builds a view for every version it previews.
            PdfApp::past()
        } else if let Some(path) = cx.launch_arguments().first() {
            PdfApp::open_with_content(path, content)
        } else if cx.fixture::<SampleDocument>().is_some() {
            PdfApp::sample_with_content(content)
        } else {
            PdfApp::empty_with_content(content)
        };
        // A search hit, or an agent's `open`, shows the page, markup or
        // thread it names.
        cx.on_open_item(app.opener());
        cx.surface(app)
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn label(state: &PdfMarkup) -> String {
        if !state.source_name.is_empty() {
            state.source_name.clone()
        } else if state.pages.is_empty() {
            "PDF Viewer".into()
        } else {
            format!("{} pages", state.pages.len())
        }
    }

    /// A document's markup is a record of its own, derived from the file's
    /// durable identity, so renaming or moving the file finds the same
    /// annotations, a copy starts clean, and another app's record of the
    /// same file is left alone.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        input.argument().map(|path| {
            RecordId::derived(
                MARKUP_ROLE,
                &RecordId::identify_file(std::path::absolute(path).unwrap_or_else(|_| path.into())),
            )
        })
    }

    /// A handed file is kept in the content store and pinned as the record's
    /// source, so the record reopens it wherever the bytes are.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<PdfMarkup>> {
        let (Some(file), Some(content)) = (input.file(), input.content()) else {
            return Vec::new();
        };
        let Ok(hash) = content.put(file.bytes()) else {
            return Vec::new();
        };
        let len = u64::try_from(file.bytes().len()).unwrap_or(u64::MAX);
        vec![crate::actions::pin_source_action(&hash, len, file.name())]
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn fixture(fixture: &mut Fixture<PdfMarkup>) {
        crate::scenarios::fixture(fixture);
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<PdfMarkup>> {
        crate::scenarios::scenario(name)
    }
}

/// The role of a document's markup record beside the document's own.
const MARKUP_ROLE: &str = "annotations";

impl ResourceSurface<PdfMarkup, PdfEffect> for PdfApp {
    fn present(&mut self, state: &Rc<PdfMarkup>) {
        self.adopt_resource_state(state);
    }

    /// What the open document says — its pages' text, its outline, the
    /// current find hits — beside the markup the resource holds.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.shown_search_entries()
    }

    fn attach(&mut self, cx: SurfaceCx<PdfMarkup>) {
        self.attach_framework(cx.dispatcher());
        self.attach_people(cx.people());
        self.attach_open_here(cx.opener());
        // A capture shows the scenario's signature, never the person's.
        if cx.mode().is_live()
            && let Some(signature) = cx.person::<crate::signature::SignatureProfile>()
        {
            self.attach_signature(signature);
        }
    }

    fn read_only_changed(&mut self, read_only: bool) {
        self.set_read_only(read_only);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        PdfApp::stage(self, scenario);
    }

    fn effect(&mut self, effect: PdfEffect, cx: &mut EffectCx<PdfMarkup, Self>) -> EffectAnswer {
        match effect {
            PdfEffect::Open { path } => self.open_effect(path, cx),
            effect => self.answer_effect(&effect, || cx.reply()),
        }
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.document_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.document_row(id)
    }
}

#[cfg(test)]
mod tests;
