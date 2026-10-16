//! Books' application definition, the one every host mounts: the native
//! launcher, the web desktop and an agent's participant.
use ui::application::ApplicationDefinition;

/// What Books opens when a host hands it a file.
pub const EPUB_MEDIA_TYPE: &str = "application/epub+zip";

/// The resource a book is addressed by in Books' action surface: its place
/// in the library.
pub fn book_resource(book_id: &str) -> String {
    crate::resource::book_place_resource(book_id)
}

pub fn definition() -> &'static ApplicationDefinition {
    ui::built_in_definition!(crate::EbookShell)
}

#[cfg(test)]
#[path = "component_tests.rs"]
mod tests;
