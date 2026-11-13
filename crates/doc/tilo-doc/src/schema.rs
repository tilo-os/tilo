//! The canonical rich-text automerge layout (schema v1). Public ABI: codecs and the
//! snapshot depend on these field names.

pub const SCHEMA_ID: &str = "tilo.richtext";
pub const SCHEMA_VERSION: u64 = 1;

pub const KEY_SCHEMA: &str = "schema";
pub const KEY_SCHEMA_VERSION: &str = tilo_resource::evolution::VERSION_KEY;
pub const KEY_BLOCKS: &str = "blocks"; // Map<block_id, block>
pub const BLOCK_TOMBSTONE_PREFIX: &str = "~deleted:";
pub const KEY_STYLES: &str = "styles"; // Map<style_id, style def>
/// Prefix for grow-only named-style deletion markers stored beside style maps, which gives
/// custom-style deletion delete-wins semantics when a divergent replica edits a property of the
/// same style.
pub const STYLE_TOMBSTONE_PREFIX: &str = "~deleted:";
// Preservation layer (lossless store for content a codec does not model): `preserved` is
// `Map<part_name, part>`, stored either as a single `Bytes` scalar or as a `Map<segment_id, {ord,
// bytes}>` whose per-atom objects let independent atoms merge across replicas.
pub const KEY_PRESERVED: &str = "preserved"; // Map<part_name, part>  (preservation layer)
pub const PRESERVED_SEG_ORD: &str = "ord";
pub const PRESERVED_SEG_BYTES: &str = "bytes";
pub const KEY_META: &str = "meta"; // Map (actor->user, etc.)
// Content-addressed binary references: `blobs` is a `Map<blake3_hex, descriptor>` of metadata only
// — the bytes are binary inputs the caller keeps in its content store ([`tilo_content`]). Inline
// media would make every replica and host log carry the payload and put later edits behind a large
// change; a reference is a few dozen bytes.
pub const KEY_BLOBS: &str = "blobs"; // Map<blake3_hex, descriptor>
pub const BLOB_MIME: &str = "mime";
pub const BLOB_LEN: &str = "len";
pub const BLOB_W: &str = "w";
pub const BLOB_H: &str = "h";

// The descriptor fields as `materialize` projects them onto an `image` block's attrs, distinct from
// `width`/`height` (requested display size): these are the content's intrinsic size, so an image
// can reserve its space before it arrives.
pub const ATTR_BLOB_MIME: &str = "blob_mime";
pub const ATTR_BLOB_BYTES: &str = "blob_bytes";
pub const ATTR_BLOB_W: &str = "blob_w";
pub const ATTR_BLOB_H: &str = "blob_h";
// Page header and footer regions, each a `Map<block_id, block>` shaped like `blocks`. Created
// lazily; older documents open with none.
pub const KEY_HEADER: &str = "header"; // Map<block_id, block>
pub const KEY_FOOTER: &str = "footer"; // Map<block_id, block>

// Footnotes and endnotes: a `notes` map of `Map<note_id, note>` with `kind` and a `blocks` child
// map. Referenced from body text by a `note-ref` mark over a placeholder character (U+FFFC), valued
// with the note id.
pub const KEY_NOTES: &str = "notes"; // Map<note_id, note>
pub const NOTE_KIND: &str = "kind"; // Str ("footnote"|"endnote")
pub const NOTE_BLOCKS: &str = "blocks"; // Map<block_id, block> (the note body)
pub const NOTE_KIND_FOOTNOTE: &str = "footnote";
pub const NOTE_KIND_ENDNOTE: &str = "endnote";
pub const MARK_NOTE_REF: &str = "note-ref"; // Str — the referenced note id

// Comments and annotations: a `comments` map of `Map<comment_id, comment>` with `author`, `date`
// (ISO-8601) and a `blocks` child map. Replies share that root as records with a `parent` id, so
// two peers can add their first reply to an older comment concurrently without racing to create a
// nested map. Anchored by a `comment` mark over `[start, end)`. Created eagerly so concurrent adds
// converge on one container.
pub const KEY_COMMENTS: &str = "comments"; // Map<comment_id, comment>
pub const COMMENT_AUTHOR: &str = "author"; // Str — the comment author's display name
pub const COMMENT_DATE: &str = "date"; // Str — the comment timestamp (ISO-8601)
pub const COMMENT_BLOCKS: &str = "blocks"; // Map<block_id, block> (the comment body)
pub const COMMENT_REPLY_PARENT: &str = "parent";
pub const COMMENT_REPLY_AUTHOR: &str = "author"; // Str — reply author's display name
pub const COMMENT_REPLY_DATE: &str = "date"; // Str — reply timestamp (ISO-8601)
pub const COMMENT_REPLY_BLOCKS: &str = "blocks"; // Map<block_id, block> (reply body)
/// Prefix for grow-only comment-resolution tombstones stored as boolean values in the comments map:
/// keeping them beside the entities makes resolve win over a reply authored on a divergent replica,
/// without changing the schema genesis.
pub const COMMENT_TOMBSTONE_PREFIX: &str = "~resolved:";
pub const MARK_COMMENT: &str = "comment"; // Str — the anchored comment id

// Tracked changes: a `changes` map of `Map<change_id, change>` with `author`, `date` and `kind`
// ("ins"|"del"), anchored by an `ins`/`del` mark over `[start, end)` valued with the change id.
// Created eagerly so concurrent tracking converges.
pub const KEY_CHANGES: &str = "changes"; // Map<change_id, change>
pub const CHANGE_AUTHOR: &str = "author"; // Str — the change author's display name
pub const CHANGE_DATE: &str = "date"; // Str — the change timestamp (ISO-8601)
pub const CHANGE_KIND: &str = "kind"; // Str ("ins"|"del")
pub const CHANGE_KIND_INS: &str = "ins";
pub const CHANGE_KIND_DEL: &str = "del";
pub const MARK_INS: &str = "ins"; // Str — the anchored (insertion) change id
pub const MARK_DEL: &str = "del"; // Str — the anchored (deletion) change id

// Multi-column section layout: `columns` (Uint) and optional `column_gap` (F64, px) as scalar
// registers under `meta`, so replicas changing the count field-level merge. Absent → single column.
pub const META_COLUMNS: &str = "columns"; // Uint (under meta) — body column count
pub const META_COLUMN_GAP: &str = "column_gap"; // F64 (under meta) — inter-column gap (px)
pub const DEFAULT_COLUMNS: u32 = 1;

// Document default language: a BCP-47 tag (`en-US`, `de-DE`) as a `Str` register under `meta`, so
// replicas changing it field-level merge. Absent → the consumer's locale. Shared contract with
// `.odt` `fo:language`/`fo:country` and `.docx` `w:rPrDefault`→`w:lang`.
pub const META_LANGUAGE: &str = "language"; // Str (under meta) — document default BCP-47 tag
/// The inline mark naming a per-run language override, round-tripping through `.odt`
/// `fo:language`/`fo:country` and `.docx` `w:rPr`→`w:lang`.
pub const MARK_LANG: &str = "lang"; // Str — a per-run BCP-47 language override

// Durable authorship: the `author` mark names who wrote a run, following the characters through
// edits like `lang` and `comment`. Distinct from `CHANGE_AUTHOR`, a tracked change's author; absent
// means unattributed.
pub const MARK_AUTHOR: &str = "author"; // Str — the display name of whoever wrote the run

// Document properties: a `props` sub-map under `meta` of well-known fields as plain strings,
// created lazily. Shared contract with `.odt` `meta.xml` and `.docx` `docProps/*.xml`.
pub const KEY_PROPS: &str = "props"; // Map<prop_name, Str>  (under meta)
pub const PROP_TITLE: &str = "title"; // Str — dc:title
pub const PROP_AUTHOR: &str = "author"; // Str — dc:creator
pub const PROP_SUBJECT: &str = "subject"; // Str — dc:subject
pub const PROP_KEYWORDS: &str = "keywords"; // Str — meta:keyword / cp:keywords
pub const PROP_CREATED: &str = "created"; // Str — meta:creation-date / dcterms:created (ISO-8601)
pub const PROP_MODIFIED: &str = "modified"; // Str — dc:date / dcterms:modified (ISO-8601)
pub const PROP_GENERATOR: &str = "generator"; // Str — meta:generator / app.xml Application

pub const DOC_PROP_KEYS: [&str; 7] = [
    PROP_TITLE,
    PROP_AUTHOR,
    PROP_SUBJECT,
    PROP_KEYWORDS,
    PROP_CREATED,
    PROP_MODIFIED,
    PROP_GENERATOR,
];

// Bookmarks are a `bookmark` mark valued with its name over `[start, end)`, round-tripping through
// `.odt` `text:bookmark-*` and `.docx` `w:bookmarkStart`/`-End`; a cross-reference reuses the
// `link` mark with a `#<target>` value. The mark is the source of truth, and the snapshot exposes a
// derived `bookmarks` registry.
pub const MARK_BOOKMARK: &str = "bookmark"; // Str — the bookmark name
pub const MARK_LINK: &str = "link"; // Str — an href (external URL) or `#target` (internal ref)
/// A mark over a whole paragraph projecting another resource live: the run says the title the
/// resource had when written, and the value names it — a host's own page (`#page-<id>`) or a
/// catalogue reference. What the editor draws as a card, one level deep.
pub const MARK_PROJECTION: &str = "transclusion"; // Str — what it projects
pub const CROSS_REF_PREFIX: char = '#';
/// The prefix of a heading's derived reference id (`_heading_<block_id>`), so a cross-reference can
/// target a heading with no bookmark; never a user bookmark name.
pub const HEADING_REF_PREFIX: &str = "_heading_";

// Inline field markers: one placeholder character covered by a `field` mark naming the field kind,
// whose rendered text is computed at layout time (e.g. page number).
pub const MARK_FIELD: &str = "field"; // Str — the field kind (see FIELD_PAGE_NUMBER)
pub const FIELD_PAGE_NUMBER: &str = "page-number"; // a `field` mark value
pub const FIELD_PLACEHOLDER: char = '\u{FFFC}';

// Inline elements a codec does not model but keeps verbatim: the mark's value is the element's
// source XML and the text it covers is its visible text, which displays and edits like any other.
// An element without visible text (a drawing, a formula) covers one placeholder. Only the codec
// that wrote the mark re-emits its XML.
pub const MARK_DOCX_XML: &str = "docx-xml"; // Str — a WordprocessingML paragraph-content fragment
pub const MARK_ODT_XML: &str = "odt-xml"; // Str — an ODF paragraph-content element
pub const KEPT_INLINE_MARKS: [&str; 2] = [MARK_DOCX_XML, MARK_ODT_XML];

pub const BLOCK_ORDER: &str = "order"; // Bytes (fractional key)
pub const BLOCK_KIND: &str = "kind"; // Str ("paragraph"|"heading"|"list_item"|"table"|"cell"|"image"|"fidelity")
pub const BLOCK_TEXT: &str = "text"; // Text (present for text blocks)
pub const BLOCK_ATTRS: &str = "attrs"; // Map (style_id, level, ...)
pub const BLOCK_CHILDREN: &str = "children"; // Map<block_id, block> (containers)

// Style fields: each style under `styles.<id>` is a sub-map of these scalar properties, field-level
// mergeable. Shared contract with the editor's `StyleProps`; maps onto ODF `style:style`.
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
