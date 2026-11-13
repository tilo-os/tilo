//! Desktop search shared by every Tilo desktop: the native shell's palette
//! and the portable desktop surface rank, group and present results with
//! this one implementation.
//!
//! - [`rank`]: per-provider field scores on the shared ranker; the
//!   cross-provider merge and kind grouping are [`ui::search::system`]'s.
//! - [`model`] and [`providers`]: the portable query model and the sources a
//!   desktop can search without a compositor (applications from the manifest
//!   registry, open windows, application actions and content, recent
//!   choices, host commands).
//! - [`palette`]: the toolkit's command palette over those sources.
//! - [`window`]: one window's own search, over what it publishes.
//! - [`lazy`]: search whose code a program may fetch on first use.
//! - [`answers`]: what a query that is not a name answers: a sum, a unit
//!   conversion, an address, the web searches it can be.
//! - `content_index` and `files_index` (feature `indexes`): the session's
//!   on-disk indexes of what apps publish and of the person's files.
//! - [`availability`]: what the manifest registry knows of each app: where
//!   it runs, its category and the words people know it by.
//! - [`usage`] and [`preferences`]: what the person chose before, and the
//!   sources they switched off; the host keeps both.
//! - [`i18n`]: the palette's own copy, in the desktop's language.
//! - Feature `settings`: Settings' pages and the settings on them, from
//!   Settings' own index — every desktop's search offers them, the page's
//!   too, each listing the pages its build has.
//! - Feature `catalogue` (native): the apps a person can get, from the
//!   catalogue the Store reads.

pub mod answers;
pub mod availability;
#[cfg(feature = "indexes")]
pub mod content_index;
#[cfg(feature = "indexes")]
pub mod files_index;
pub mod i18n;
#[cfg(feature = "indexes")]
pub mod indexed;
pub mod lazy;
pub mod model;
pub mod names;
pub mod palette;
pub mod preferences;
pub mod providers;
pub mod rank;
pub mod usage;
pub mod window;
pub mod workers;

use std::path::Path;

/// The `file://` address of `path`, as a row names a file it opens.
#[must_use]
pub fn file_uri(path: &Path) -> String {
    use std::fmt::Write as _;
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(char::from(byte));
            }
            other => {
                let _ = write!(uri, "%{other:02X}");
            }
        }
    }
    uri
}

pub use lazy::{
    DESKTOP_SEARCH, DesktopSearchApi, LazyDesktopSearch, MakeDesktopSearch, builtin_desktop_search,
};
pub use model::{
    ALL_APPS_COMMAND, ActionTarget, CHROME_RESOURCE, DESKTOP_APP, Held, Hit, InvokeTarget,
    LaunchPlace, OPEN_ACTION, OPEN_ANCHOR, RECORD_LAUNCH, SHOW_COLLABORATORS, SearchStream, Source,
    Target, Thing, WORKSPACE_COMMAND_PREFIX, launched_record, on_the_loop, record_launch, search,
    search_stream, the_loop,
};
pub use palette::{ANSWER_POLL, DesktopSearch, SearchData};
pub use preferences::SearchPreferences;
pub use ui::search::system::ResourceKind;
pub use usage::Usage;
pub use workers::SearchWorkers;

/// What the crate's tests wait for a worker: a source answers on a worker
/// thread, so a test drives frames until it has, rather than assuming the
/// answer is in the frame that asked.
#[cfg(test)]
pub(crate) mod tests {
    use ui::time::Duration;

    pub(crate) const SETTLE: Duration = Duration::from_secs(20);
}
