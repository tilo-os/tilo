// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "fd_boundary.rs"]
pub mod fd_boundary;
#[path = "native_images.rs"]
pub mod native_images;
#[path = "native_session.rs"]
pub mod native_session;
