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
