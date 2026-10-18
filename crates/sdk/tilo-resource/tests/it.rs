// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "apply_cache.rs"]
pub mod apply_cache;
#[cfg(feature = "chrono")]
#[path = "dates.rs"]
pub mod dates;
#[path = "data_enum.rs"]
pub mod data_enum;
#[path = "evolution.rs"]
pub mod evolution;
#[path = "fields.rs"]
pub mod fields;
#[path = "footprint.rs"]
pub mod footprint;
#[path = "golden_path.rs"]
pub mod golden_path;
#[path = "housekeeping_tag.rs"]
pub mod housekeeping_tag;
#[path = "incoming_message.rs"]
pub mod incoming_message;
#[path = "incremental.rs"]
pub mod incremental;
#[path = "keyed.rs"]
pub mod keyed;
#[path = "kinds.rs"]
pub mod kinds;
#[path = "one_pass_reads.rs"]
pub mod one_pass_reads;
#[path = "outbox.rs"]
pub mod outbox;
#[path = "paths.rs"]
pub mod paths;
#[path = "schema.rs"]
pub mod schema;
#[path = "shape.rs"]
pub mod shape;
#[path = "verbs.rs"]
pub mod verbs;
