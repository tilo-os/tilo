//! The workspace's published tree as a caller names it: an address of nested
//! resources ending in an action, the names a resource goes by among the
//! others under its parent, and which resource or action a name picks.
//!
//! Every way in reads the tree through these: MCP, the REST API, the command
//! line and tilolang. A resource's type is a property of it, not a level of
//! an address: an address is resources inside resources, as deep as they nest.

mod address;
mod cursor;
mod names;
mod node;
mod pick;

pub use address::{Address, Segment};
pub use cursor::LsCursor;
pub use names::{
    BOUND_PREFIX, app_meant, bind_choices, bound_key, close_matches, folded, path_name,
};
pub use node::{
    Level, LevelPages, Names, Node, PageMark, distinct_children, level_of, row_offset,
    shared_names, spanned,
};
pub use pick::{
    TypeNames, action_rank, is_program_form, named_beyond_its_start, only_by_id, pick, pick_action,
    rank, reads_action,
};

/// How many names an error lists before it stops.
pub const NAMES_LISTED: usize = 12;

/// The close matches an action no resource offers is told with.
pub const CLOSE_MATCHES: usize = 5;
