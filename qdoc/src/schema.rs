//! The canonical rich-text automerge layout (schema v1). Public ABI: codecs
//! and the snapshot depend on these field names.

pub const SCHEMA_ID: &str = "qos.richtext";
pub const SCHEMA_VERSION: i64 = 1;

// Root keys
pub const KEY_SCHEMA: &str = "schema";
pub const KEY_SCHEMA_VERSION: &str = "schema_version";
pub const KEY_BLOCKS: &str = "blocks"; // Map<block_id, block>
pub const KEY_STYLES: &str = "styles"; // Map<style_id, style def>
pub const KEY_PRESERVED: &str = "preserved"; // Map<part_name, Bytes>  (preservation layer)
pub const KEY_META: &str = "meta"; // Map (actor->user, etc.)
pub const KEY_BLOBS: &str = "blobs"; // Map<blake3_hex, Bytes>  (content-addressed image/binary store)
// Page header / footer regions. Each is a `Map<block_id, block>` with the exact
// same shape as `blocks` (fractional order keys, same block fields), holding the
// ordered paragraph sequence rendered at the top/bottom of every page. Created
// lazily on first write and opened optionally, so documents authored before the
// regions existed still open (they then read as having no header/footer).
pub const KEY_HEADER: &str = "header"; // Map<block_id, block>
pub const KEY_FOOTER: &str = "footer"; // Map<block_id, block>

// Footnotes / endnotes. A `notes` root map holds `Map<note_id, note>`, where each
// note is a sub-map carrying its `kind` ("footnote"|"endnote") and a `blocks`
// child map (the note's content — same shape as the body `blocks`, with
// fractional order keys). The note is *referenced* from body text by an inline
// `note-ref` mark over a single placeholder character (U+FFFC), whose value is the
// note id; the mark merges with concurrent text edits and round-trips through the
// codecs. Created lazily on first write and opened optionally so documents
// authored before notes existed still open (they then read as having no notes).
pub const KEY_NOTES: &str = "notes"; // Map<note_id, note>
pub const NOTE_KIND: &str = "kind"; // Str ("footnote"|"endnote")
pub const NOTE_BLOCKS: &str = "blocks"; // Map<block_id, block> (the note body)
pub const NOTE_KIND_FOOTNOTE: &str = "footnote";
pub const NOTE_KIND_ENDNOTE: &str = "endnote";
/// The inline mark naming a note reference. Covers exactly the single placeholder
/// character (U+FFFC) standing in for the citation; its value is the referenced
/// note id (a `Str`). Renderers substitute the computed citation number.
pub const MARK_NOTE_REF: &str = "note-ref"; // Str — the referenced note id

// Comments / annotations. A `comments` root map holds `Map<comment_id, comment>`,
// where each comment is a sub-map carrying its `author`, `date` (a free-form string
// — ISO-8601 in practice) and a `blocks` child map (the comment's content — same
// shape as the body `blocks`, with fractional order keys, so a comment can hold
// multiple paragraphs). A comment is *anchored* to body text by an inline `comment`
// mark whose value is the comment id, covering the commented range `[start, end)`;
// the mark merges with concurrent text edits and round-trips through the codecs
// (`office:annotation`/`office:annotation-end` in `.odt`, `commentRangeStart`/
// `commentRangeEnd` + `commentReference` in `.docx`). Created eagerly (like
// `notes`) so two replicas that concurrently add a comment share ONE container
// object and converge; opened optionally so documents authored before comments
// existed still open (they then read as having no comments).
pub const KEY_COMMENTS: &str = "comments"; // Map<comment_id, comment>
pub const COMMENT_AUTHOR: &str = "author"; // Str — the comment author's display name
pub const COMMENT_DATE: &str = "date"; // Str — the comment timestamp (ISO-8601)
pub const COMMENT_BLOCKS: &str = "blocks"; // Map<block_id, block> (the comment body)
/// The inline mark anchoring a comment to a text range. Covers the commented range
/// `[start, end)`; its value is the comment id (a `Str`). Unlike `note-ref`, this
/// spans real text rather than a placeholder character.
pub const MARK_COMMENT: &str = "comment"; // Str — the anchored comment id

// Tracked changes / revision marks. A `changes` root map holds `Map<change_id,
// change>`, where each change is a sub-map carrying its `author`, `date` (a free-form
// string — ISO-8601 in practice) and a `kind` ("ins"|"del"). A change is *anchored* to
// body text by an inline `ins`/`del` mark (matching the change's kind) whose value is
// the change id, covering the changed range `[start, end)`:
//   * a tracked **insertion** wraps inserted body text with an `ins` mark — the text
//     is real content, flagged as proposed-added until accepted;
//   * a tracked **deletion** marks retained-but-struck body text with a `del` mark —
//     the text stays in the document (so reviewers can see it) until accept removes it.
// Both marks merge with concurrent text edits and round-trip through the codecs
// (`text:tracked-changes`/`text:changed-region` + `text:change-start`/`text:change-end`/
// `text:change` in `.odt`; `<w:ins>`/`<w:del>`(+`<w:delText>`) run wrappers in `.docx`).
// Created eagerly (like `comments`) so two replicas that concurrently track a change
// share ONE container object and converge; opened optionally so documents authored
// before tracked changes existed still open (they then read as having no changes).
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

// Multi-column section layout. A document-wide column count (and optional gap)
// stored directly under `meta` as scalar registers, so two replicas changing the
// column count field-level merge rather than clobbering each other. `columns` is a
// `Uint` (the number of newspaper-style columns the body text flows across);
// `column_gap` is an optional `F64` (the gutter between columns, in px). Absent →
// a single column (the default). This is the shared contract between the qdoc model
// and the `.odt` page-layout `style:columns` / `.docx` `w:sectPr`→`w:cols` markup.
pub const META_COLUMNS: &str = "columns"; // Uint (under meta) — body column count
pub const META_COLUMN_GAP: &str = "column_gap"; // F64 (under meta) — inter-column gap (px)
/// The default body column count when `meta.columns` is unset (a single column).
pub const DEFAULT_COLUMNS: u32 = 1;

// Document properties / metadata. A `props` sub-map under `meta` holding the
// well-known document-level fields (title/author/…) as plain strings. Isolated
// from other `meta` keys (page margins, stylesheet) and created lazily so
// documents authored before it existed still open. These keys are the shared
// contract between the qdoc model and the `dc:`/`cp:`/`dcterms:` metadata fields
// the `.odt` (`meta.xml`) and `.docx` (`docProps/*.xml`) codecs map onto.
pub const KEY_PROPS: &str = "props"; // Map<prop_name, Str>  (under meta)
pub const PROP_TITLE: &str = "title"; // Str — dc:title
pub const PROP_AUTHOR: &str = "author"; // Str — dc:creator
pub const PROP_SUBJECT: &str = "subject"; // Str — dc:subject
pub const PROP_KEYWORDS: &str = "keywords"; // Str — meta:keyword / cp:keywords
pub const PROP_CREATED: &str = "created"; // Str — meta:creation-date / dcterms:created (ISO-8601)
pub const PROP_MODIFIED: &str = "modified"; // Str — dc:date / dcterms:modified (ISO-8601)
pub const PROP_GENERATOR: &str = "generator"; // Str — meta:generator / app.xml Application

/// The canonical well-known document-property names, in a stable order.
pub const DOC_PROP_KEYS: [&str; 7] =
    [PROP_TITLE, PROP_AUTHOR, PROP_SUBJECT, PROP_KEYWORDS, PROP_CREATED, PROP_MODIFIED, PROP_GENERATOR];

// Inline field markers. A "field" is an inline placeholder whose rendered text is
// computed at layout time (e.g. the current page number). It is modeled as a
// single placeholder character in the block text covered by a `field` mark whose
// value names the field kind. This rides the existing inline-mark machinery, so a
// field merges with concurrent text edits and round-trips through the codecs.
pub const MARK_FIELD: &str = "field"; // Str — the field kind (see FIELD_PAGE_NUMBER)
pub const FIELD_PAGE_NUMBER: &str = "page-number"; // a `field` mark value
/// The single placeholder character a field occupies in block text (Unicode
/// OBJECT REPLACEMENT CHARACTER, U+FFFC). The `field` mark covers exactly this
/// character; renderers substitute the computed value (e.g. the page number).
pub const FIELD_PLACEHOLDER: char = '\u{FFFC}';

// Block fields
pub const BLOCK_ORDER: &str = "order"; // Bytes (fractional key)
pub const BLOCK_KIND: &str = "kind"; // Str ("paragraph"|"heading"|"list_item"|"table"|"cell"|"image"|"fidelity")
pub const BLOCK_TEXT: &str = "text"; // Text (present for text blocks)
pub const BLOCK_ATTRS: &str = "attrs"; // Map (style_id, level, ...)
pub const BLOCK_CHILDREN: &str = "children"; // Map<block_id, block> (containers)

// Style fields — each style under `styles.<id>` is a sub-map of these scalar
// properties (CRDT-native, so two replicas editing different properties of the
// same style field-level-merge). These key names are the shared contract between
// the qdoc styles container and the editor's `StyleProps`, and they map onto ODF
// `style:style` named-style definitions on export/import.
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
