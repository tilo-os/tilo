//! The canonical rich-text automerge layout (schema v1). Public ABI: codecs
//! and the snapshot depend on these field names.

pub const SCHEMA_ID: &str = "tilo.richtext";
pub const SCHEMA_VERSION: i64 = 1;

pub const KEY_SCHEMA: &str = "schema";
pub const KEY_SCHEMA_VERSION: &str = "schema_version";
pub const KEY_BLOCKS: &str = "blocks"; // Map<block_id, block>
/// Prefix for grow-only structural deletion markers stored beside block maps.
pub const BLOCK_TOMBSTONE_PREFIX: &str = "~deleted:";
pub const KEY_STYLES: &str = "styles"; // Map<style_id, style def>
/// Prefix for grow-only named-style deletion markers stored beside style maps.
/// This gives custom-style deletion deterministic delete-wins semantics when a
/// divergent replica edits a property of the same style.
pub const STYLE_TOMBSTONE_PREFIX: &str = "~deleted:";
// Preservation layer (lossless fidelity store for content a codec doesn't model): `preserved` is
// `Map<part_name, part>`, stored either as a single legacy `Bytes` scalar, or (Tier-4) as a
// `Map<segment_id, {ord, bytes}>` whose per-atom objects let independent atoms merge across replicas.
pub const KEY_PRESERVED: &str = "preserved"; // Map<part_name, part>  (preservation layer)
/// A preserved part's per-atom segment's fractional order key (`Bytes`), under the
/// segment sub-map. Segments sort by this to reconstruct the part in source order.
pub const PRESERVED_SEG_ORD: &str = "ord";
/// A preserved part's per-atom segment's verbatim bytes (`Bytes`); concatenating all segments in `ord` order reproduces the part byte-for-byte.
pub const PRESERVED_SEG_BYTES: &str = "bytes";
pub const KEY_META: &str = "meta"; // Map (actor->user, etc.)
// Content-addressed binary references. `blobs` is a `Map<blake3_hex, descriptor>`
// where a descriptor is a sub-map of *metadata only* — the bytes themselves live
// outside the CRDT: they are binary inputs the caller keeps in its content store (`tilo_content`).
//
// Inline media makes every Automerge replica and durable host log carry the
// full payload, amplifies sync memory, and puts later edits behind a large
// change. A reference is only a few dozen bytes, which is why this is the fix
// rather than a bigger transport message.
pub const KEY_BLOBS: &str = "blobs"; // Map<blake3_hex, descriptor>
/// A blob descriptor's media type (`Str`, e.g. `image/png`). Sniffed from the
/// content when it is attached, so it is present for anything we recognise.
pub const BLOB_MIME: &str = "mime";
/// A blob descriptor's content length in bytes (`Int`).
pub const BLOB_LEN: &str = "len";
/// A blob descriptor's intrinsic pixel width (`Int`), 0 when unknown.
pub const BLOB_W: &str = "w";
/// A blob descriptor's intrinsic pixel height (`Int`), 0 when unknown.
pub const BLOB_H: &str = "h";

// The descriptor fields as `materialize` projects them onto a referencing `image` block's attrs,
// distinct from `width`/`height` (requested display size, often absent): these are the content's
// intrinsic size, letting a not-yet-arrived image reserve its space instead of reflowing on arrival.
pub const ATTR_BLOB_MIME: &str = "blob_mime";
pub const ATTR_BLOB_BYTES: &str = "blob_bytes";
pub const ATTR_BLOB_W: &str = "blob_w";
pub const ATTR_BLOB_H: &str = "blob_h";
// Page header / footer regions: each a `Map<block_id, block>` with the same shape as `blocks`,
// holding the paragraph sequence rendered per-page. Created lazily; older documents open with no header/footer.
pub const KEY_HEADER: &str = "header"; // Map<block_id, block>
pub const KEY_FOOTER: &str = "footer"; // Map<block_id, block>

// Footnotes / endnotes: a `notes` root map of `Map<note_id, note>`, each a sub-map with `kind`
// and a `blocks` child map (same shape as body `blocks`). Referenced from body text by a `note-ref`
// mark over a placeholder character (U+FFFC), valued with the note id. Created/opened lazily.
pub const KEY_NOTES: &str = "notes"; // Map<note_id, note>
pub const NOTE_KIND: &str = "kind"; // Str ("footnote"|"endnote")
pub const NOTE_BLOCKS: &str = "blocks"; // Map<block_id, block> (the note body)
pub const NOTE_KIND_FOOTNOTE: &str = "footnote";
pub const NOTE_KIND_ENDNOTE: &str = "endnote";
/// The inline mark naming a note reference: covers the single placeholder character (U+FFFC), valued with the referenced note id (a `Str`).
pub const MARK_NOTE_REF: &str = "note-ref"; // Str — the referenced note id

// Comments / annotations: a `comments` root map of `Map<comment_id, comment>`.
// Comments have `author`, `date` (ISO-8601), and a `blocks` child map. Replies
// share that already-eager root as `Map<reply_id, reply>` records with a
// `parent` comment id plus author/date/blocks. Keeping sibling reply records in
// an existing shared map means two peers can add their first reply to an older
// comment concurrently without racing to create a nested map. Anchored to body text by a `comment` mark
// valued with the comment id over `[start, end)`. Round-trips through the codecs (ODF
// `office:annotation`/`-end`; OOXML `commentRangeStart`/`-End`+`commentReference`). Created eagerly so concurrent adds converge on one container.
pub const KEY_COMMENTS: &str = "comments"; // Map<comment_id, comment>
pub const COMMENT_AUTHOR: &str = "author"; // Str — the comment author's display name
pub const COMMENT_DATE: &str = "date"; // Str — the comment timestamp (ISO-8601)
pub const COMMENT_BLOCKS: &str = "blocks"; // Map<block_id, block> (the comment body)
/// Parent comment id on a reply record. Its absence distinguishes a root
/// annotation from a reply and keeps old comments fully backward compatible.
pub const COMMENT_REPLY_PARENT: &str = "parent";
pub const COMMENT_REPLY_AUTHOR: &str = "author"; // Str — reply author's display name
pub const COMMENT_REPLY_DATE: &str = "date"; // Str — reply timestamp (ISO-8601)
pub const COMMENT_REPLY_BLOCKS: &str = "blocks"; // Map<block_id, block> (reply body)
/// Prefix for grow-only comment-resolution tombstones stored as boolean values
/// in the comments map. Keeping them beside the entities works for legacy docs
/// without changing the schema genesis while making resolve win over a reply
/// authored on a divergent replica.
pub const COMMENT_TOMBSTONE_PREFIX: &str = "~resolved:";
/// The inline mark anchoring a comment to a text range `[start, end)`, valued
/// with the comment id (a `Str`); unlike `note-ref`, it spans text rather than
/// a placeholder character.
pub const MARK_COMMENT: &str = "comment"; // Str — the anchored comment id

// Tracked changes / revision marks: a `changes` root map of `Map<change_id, change>`, each a
// sub-map with `author`, `date` (ISO-8601), and `kind` ("ins"|"del"). Anchored by an `ins`/`del`
// mark matching the kind, valued with the change id, over `[start, end)`: an insertion wraps
// proposed-added text; a deletion marks retained-but-struck text until accept removes it. Both
// round-trip through the codecs (ODF `text:change-*`; OOXML `<w:ins>`/`<w:del>`). Created eagerly so concurrent tracking converges.
pub const KEY_CHANGES: &str = "changes"; // Map<change_id, change>
pub const CHANGE_AUTHOR: &str = "author"; // Str — the change author's display name
pub const CHANGE_DATE: &str = "date"; // Str — the change timestamp (ISO-8601)
pub const CHANGE_KIND: &str = "kind"; // Str ("ins"|"del")
pub const CHANGE_KIND_INS: &str = "ins";
pub const CHANGE_KIND_DEL: &str = "del";
/// The inline mark anchoring a tracked **insertion** to a text range. Covers the
/// inserted range `[start, end)`; its value is the change id (a `Str`).
pub const MARK_INS: &str = "ins"; // Str — the anchored (insertion) change id
/// The inline mark anchoring a tracked **deletion** to a retained-but-struck text
/// range. Covers the deleted range `[start, end)`; its value is the change id (a `Str`).
pub const MARK_DEL: &str = "del"; // Str — the anchored (deletion) change id

// Multi-column section layout: `columns` (Uint) and optional `column_gap` (F64, px) stored as
// scalar registers under `meta`, so replicas changing the count field-level merge. Absent → single
// column. Shared contract with `.odt` `style:columns` / `.docx` `w:sectPr`→`w:cols`.
pub const META_COLUMNS: &str = "columns"; // Uint (under meta) — body column count
pub const META_COLUMN_GAP: &str = "column_gap"; // F64 (under meta) — inter-column gap (px)
/// The default body column count when `meta.columns` is unset (a single column).
pub const DEFAULT_COLUMNS: u32 = 1;

// Document default language: a BCP-47 tag (`en-US`, `de-DE`, `fr`) as a `Str` register under
// `meta`, so replicas changing it field-level merge. Absent → consumer's locale default. Shared
// contract with `.odt` `fo:language`/`fo:country` / `.docx` `w:rPrDefault`→`w:lang`.
pub const META_LANGUAGE: &str = "language"; // Str (under meta) — document default BCP-47 tag
/// The inline mark naming a per-run language override (a `Str` BCP-47 tag, e.g. `fr`/`de-DE`).
/// Round-trips through `.odt` `fo:language`/`fo:country` and `.docx` `w:rPr`→`w:lang`; mirrors the other valued run marks.
pub const MARK_LANG: &str = "lang"; // Str — a per-run BCP-47 language override

// Durable authorship: the `author` mark names who wrote a run of body text (a `Str` display
// name), following the characters through edits/splits/merges like `lang`/`comment` do. Distinct
// from `CHANGE_AUTHOR` (a tracked-change proposal's author); absent means unattributed.
pub const MARK_AUTHOR: &str = "author"; // Str — the display name of whoever wrote the run

// Document properties: a `props` sub-map under `meta` holding well-known fields (title/author/…)
// as plain strings, created lazily. Shared contract with `.odt` `meta.xml` / `.docx` `docProps/*.xml`.
pub const KEY_PROPS: &str = "props"; // Map<prop_name, Str>  (under meta)
pub const PROP_TITLE: &str = "title"; // Str — dc:title
pub const PROP_AUTHOR: &str = "author"; // Str — dc:creator
pub const PROP_SUBJECT: &str = "subject"; // Str — dc:subject
pub const PROP_KEYWORDS: &str = "keywords"; // Str — meta:keyword / cp:keywords
pub const PROP_CREATED: &str = "created"; // Str — meta:creation-date / dcterms:created (ISO-8601)
pub const PROP_MODIFIED: &str = "modified"; // Str — dc:date / dcterms:modified (ISO-8601)
pub const PROP_GENERATOR: &str = "generator"; // Str — meta:generator / app.xml Application

/// The canonical well-known document-property names, in a stable order.
pub const DOC_PROP_KEYS: [&str; 7] = [
    PROP_TITLE,
    PROP_AUTHOR,
    PROP_SUBJECT,
    PROP_KEYWORDS,
    PROP_CREATED,
    PROP_MODIFIED,
    PROP_GENERATOR,
];

// Bookmarks (named anchors) + cross-references (internal links). A bookmark is a `bookmark` mark
// valued with its name over `[start, end)` (a point bookmark widens to one character on import);
// round-trips through `.odt` `text:bookmark-*` / `.docx` `w:bookmarkStart`/`-End`. A cross-reference
// reuses the external-hyperlink `link` mark with a `#<target>` value (leading `#` marks it internal);
// target is a bookmark name or a heading's derived id. The mark is the source of truth; the snapshot exposes a derived `bookmarks` registry.
pub const MARK_BOOKMARK: &str = "bookmark"; // Str — the bookmark name
/// The inline `link` mark (external hyperlinks *and* internal cross-references): a value starting with `#` is internal, anything else an external URL. Shared with the `.md`/`.html` codecs.
pub const MARK_LINK: &str = "link"; // Str — an href (external URL) or `#target` (internal ref)
/// The leading character of an internal cross-reference `link` value (`#bookmarkName`).
pub const CROSS_REF_PREFIX: char = '#';
/// The prefix of a heading's auto/derived reference id (`_heading_<block_id>`), letting a cross-reference target a heading with no explicit bookmark; never used as a user bookmark name.
pub const HEADING_REF_PREFIX: &str = "_heading_";

// Inline field markers: a placeholder whose rendered text is computed at layout time (e.g. page
// number), modeled as one placeholder character covered by a `field` mark naming the field kind.
pub const MARK_FIELD: &str = "field"; // Str — the field kind (see FIELD_PAGE_NUMBER)
pub const FIELD_PAGE_NUMBER: &str = "page-number"; // a `field` mark value
/// The single placeholder character a field occupies (U+FFFC OBJECT REPLACEMENT CHARACTER); the `field` mark covers exactly this character and renderers substitute the computed value.
pub const FIELD_PLACEHOLDER: char = '\u{FFFC}';

// Inline elements an office codec does not model but keeps verbatim to write back: the mark's
// value is the element's source XML, and the text it covers is the element's visible text (a
// field's result, a content control's content), which displays and edits like any other text.
// An element without visible text (a drawing, a formula) covers one FIELD_PLACEHOLDER, which
// displays as nothing. Only the codec that wrote the mark re-emits its XML.
pub const MARK_DOCX_XML: &str = "docx-xml"; // Str — a WordprocessingML paragraph-content fragment
pub const MARK_ODT_XML: &str = "odt-xml"; // Str — an ODF paragraph-content element
/// Every mark naming an inline element kept verbatim.
pub const KEPT_INLINE_MARKS: [&str; 2] = [MARK_DOCX_XML, MARK_ODT_XML];

pub const BLOCK_ORDER: &str = "order"; // Bytes (fractional key)
pub const BLOCK_KIND: &str = "kind"; // Str ("paragraph"|"heading"|"list_item"|"table"|"cell"|"image"|"fidelity")
pub const BLOCK_TEXT: &str = "text"; // Text (present for text blocks)
pub const BLOCK_ATTRS: &str = "attrs"; // Map (style_id, level, ...)
pub const BLOCK_CHILDREN: &str = "children"; // Map<block_id, block> (containers)

// Style fields: each style under `styles.<id>` is a sub-map of these scalar properties
// (field-level mergeable). Shared contract with the editor's `StyleProps`; maps onto ODF `style:style`.
pub const STYLE_NAME: &str = "name"; // Str (display name, e.g. "Heading 1")
pub const STYLE_FONT: &str = "font"; // Str ("sans"|"serif"|"mono")
pub const STYLE_SIZE: &str = "size"; // F64 (pt)
pub const STYLE_BOLD: &str = "bold"; // Bool
pub const STYLE_ITALIC: &str = "italic"; // Bool
pub const STYLE_COLOR: &str = "color"; // Str ("#rrggbb")
pub const STYLE_ALIGN: &str = "align"; // Str ("left"|"center"|"right")
pub const STYLE_SPACE_BEFORE: &str = "space_before"; // F64 (px)
pub const STYLE_SPACE_AFTER: &str = "space_after"; // F64 (px)
pub const STYLE_INDENT: &str = "indent"; // F64 (px, left indent)
