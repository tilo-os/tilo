// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "evolution.rs"]
pub mod evolution;
#[path = "fields.rs"]
pub mod fields;
#[path = "golden_path.rs"]
pub mod golden_path;
#[path = "incremental.rs"]
pub mod incremental;
#[path = "keyed.rs"]
pub mod keyed;
#[path = "kinds.rs"]
pub mod kinds;
#[path = "paths.rs"]
pub mod paths;
#[path = "schema.rs"]
pub mod schema;
#[path = "shape.rs"]
pub mod shape;
#[path = "verbs.rs"]
pub mod verbs;
