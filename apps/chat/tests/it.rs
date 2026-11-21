//! One test binary instead of one per file; #[path] keeps the files in place.

#[path = "record_search.rs"]
mod record_search;

#[path = "native_chat_fixture.rs"]
mod native_chat_fixture;
