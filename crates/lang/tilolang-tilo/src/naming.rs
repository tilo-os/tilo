//! What a published app and its resources are called: the ids its resources are published
//! under, and the app's name in the reader's language.

use tilolang_runtime::meta::Meta;
use tilolang_runtime::words::Words;

/// The id of the published resource for the tilolang resource `id` of `app`.
#[must_use]
pub fn resource_id(app: &str, id: &str) -> String {
    format!("tl:{app}:{id}")
}

/// The id of a package's own resource, for its document-less actions.
#[must_use]
pub fn package_resource_id(app: &str) -> String {
    format!("tl:{app}")
}

/// The id of the host resource.
pub const HOST_RESOURCE: &str = "tl:host";

/// What the app `meta` is called in the reader's language: its title, in the words the package's
/// own catalogue writes for it.
#[must_use]
pub fn app_name(words: &dyn Words, meta: &Meta) -> String {
    words.word(&tilolang::messages::key_of(&meta.app), &[], &meta.title)
}
