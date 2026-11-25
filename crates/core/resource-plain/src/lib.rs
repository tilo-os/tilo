//! The parts of Tilo's resource vocabulary that need no store, document or CRDT: the smallest
//! splice between two texts, the scope a search looks within, the kind an item's address names,
//! and the name a workspace emoji is written by. `tilo-resource` publishes them where they have
//! always been; the toolkit reads them here, with or without that model.

mod emoji;
mod item;
mod scope;
mod text;

pub use emoji::{MAX_CUSTOM_NAME, custom_emoji_name, valid_custom_emoji_name};
pub use item::item_kind;
pub use scope::{ScopeDecl, scope_contains};
pub use text::{Splice, common_ends, common_text_ends, splice_between};
