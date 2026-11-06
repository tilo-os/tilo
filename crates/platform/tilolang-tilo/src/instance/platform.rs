//! What an instance asks of whatever runs it: the platform's answers for loading, words, time
//! and its zone, waking and publishing.
//!
//! An instance runs the same way in a window of the app host, in a browser worker and in a test;
//! what differs is what runs it, and this is the whole of what it may ask.

use std::rc::Rc;

use crate::background::Waker;
use crate::bundle::Bundle;
use crate::host::Host;
use crate::runtime::Words;

use super::{Publication, Update};

/// What an instance asks of whatever runs it.
pub trait Platform {
    /// Load what a window of the package runs into `host`: the installed package, or the latest
    /// later build of it, with the packages it uses. The later build it
    /// loaded, 0 for the installed package.
    fn load(&self, host: &Host) -> u64;

    /// The latest later build of the package, once a development session handed one over.
    fn latest(&self) -> Option<Update> {
        None
    }

    /// Wake this instance when a later build arrives. Called once, when it first follows them.
    fn follow_updates(&self) {}

    /// Whether this process follows later builds at all.
    fn follows_updates(&self) -> bool {
        false
    }

    /// A later build took over: what the package declares on the door is its now.
    fn surface_changed(&self, app: &str, bundle: &Bundle) {
        let _ = (app, bundle);
    }

    /// The platform's phrase `id` with `args`, in the person's language: what the standard
    /// library's frames say.
    fn phrase(&self, id: &str, args: &[(String, String)]) -> String;

    /// Takes in a package's catalogues, `(locale, Fluent text)`, so [`Self::phrase`] says its
    /// messages too (i18n.md). A platform without catalogues of its own shows the source words.
    fn add_words(&self, catalogues: &[(String, String)]) {
        let _ = catalogues;
    }

    /// What the words resolve at now: it moves when the person's language, or a catalogue of
    /// theirs, does, so a package's published labels and nouns are published again in it.
    fn words_revision(&self) -> u64 {
        0
    }

    /// The time now, as `now` reads it: ISO-8601 UTC text.
    fn now(&self) -> String;

    /// The language the person reads and types in (`de`, `en`): what a date or a time typed in
    /// an entry is said in. Empty when the platform names none, which reads as English.
    fn language(&self) -> String {
        String::new()
    }

    /// How many seconds the person's clock is ahead of UTC: the zone `dated` reads "tomorrow
    /// 9am" in. UTC unless the platform knows the person's.
    fn zone(&self) -> i64 {
        0
    }

    /// The person's zone as its IANA name (`Europe/Berlin`): the zone a repeating item keeps its
    /// wall time of day in across a change of clocks, and the one a day begins in. Nothing
    /// unless the platform knows it, which leaves the offset above as the whole answer.
    fn zone_name(&self) -> Option<String> {
        None
    }

    /// The day the person's region starts its week on, as days from Monday (`0`): the day a
    /// day's `start_of_week` goes back to. Monday unless the platform knows the person's region.
    fn week_start(&self) -> i64 {
        0
    }

    /// What wakes the instance when work it waits for is ready (an index read off the frame).
    fn waker(&self) -> Waker;

    /// Publish what `host` holds until the returned hold is dropped; `wake` is called when
    /// records read off the frame are ready to be taken in.
    fn publish(&self, host: &Host, wake: Waker) -> Option<Box<dyn Publication>> {
        let _ = (host, wake);
        None
    }
}

/// The words an instance shows, in the person's language: every package's catalogues go to
/// the platform, and a key says what the platform's phrases say of it, or the source words where
/// no catalogue holds it (a phrase no catalogue holds is its own key) (i18n.md).
pub struct PlatformWords(pub(super) Rc<dyn Platform>);

impl Words for PlatformWords {
    fn add(&self, catalogues: &[(String, String)]) {
        self.0.add_words(catalogues);
    }

    fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String {
        let said = self.0.phrase(key, args);
        if said == key { source.to_owned() } else { said }
    }

    fn language(&self) -> String {
        self.0.language()
    }

    fn revision(&self) -> u64 {
        self.0.words_revision()
    }
}
