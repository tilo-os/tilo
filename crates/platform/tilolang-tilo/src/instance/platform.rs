//! What an instance asks of whatever runs it: the platform's answers for loading, words, time
//! and its zone, waking and publishing.
//!
//! An instance runs the same way in a window of the app host, in a browser worker and in a test;
//! what differs is what runs it, and this is the whole of what it may ask.

use std::rc::Rc;

use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
use tilolang_runtime::run::WebRequest;
use tilolang_runtime::web::WebReply;

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
    /// messages too (i18n.md). `window` is the package the window shows, whose languages choose
    /// the language it speaks; without it, a package the window uses, whose words ride along in
    /// that language and never choose it. A platform without catalogues of its own shows the
    /// source words.
    fn add_words(&self, window: bool, catalogues: &[(String, String)]) {
        let _ = (window, catalogues);
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

    /// How the stored `value` of a day or a time of day (`kind` is `date` or `time`) reads in a
    /// sentence — a row's line, a notice — where a view cannot stand: a day as the day fact
    /// (`Today`, `Friday`, `Jun 29`), a time of day as a list shows when something came. Stored
    /// text unless the platform formats, which a window's host does.
    fn shown(&self, kind: &str, value: &str) -> String {
        let _ = kind;
        value.to_owned()
    }

    /// The time of day `hour`:`minute` as the person reads a clock (`14:05`, `2:05 PM`), in the
    /// style they chose in Settings: what a view says when it names the time of a moment
    /// (`a.fetched.clock()`). A 24-hour clock unless the platform knows the person's style, which
    /// a host with the toolkit does.
    fn clock(&self, hour: u32, minute: u32) -> String {
        format!("{hour:02}:{minute:02}")
    }

    /// The answer to a `web.get` or `web.post` the instance's code makes: this device's
    /// network, which runs each request once, and the platform's limits for it. A platform that
    /// reaches no services answers [`WebReply::failed`], saying so; so does one whose device is
    /// offline. The call may block for as long as [`tilolang_runtime::web::TIMEOUT`].
    fn web(&self, request: &WebRequest) -> WebReply {
        let _ = request;
        WebReply::failed("this host reaches no services")
    }

    /// The content at the content address `hash`, when this device's content store holds it: a
    /// file an action reads (`.text()`), and the one `file.save` writes out. A platform that
    /// holds no store answers none.
    fn read_content(&self, hash: &str) -> Option<Vec<u8>> {
        let _ = hash;
        None
    }

    /// Ask the person for a file, taking what they pick into the content store. `None` when
    /// they dismiss the chooser; the error says why a file could not be taken in (its size,
    /// its media type, the store's failure). A platform without a chooser answers the error
    /// saying so.
    ///
    /// # Errors
    ///
    /// Why there is no file: there is no chooser, or the one there is refused it.
    fn choose_file(&self, request: &FileRequest) -> Result<Option<FileValue>, String> {
        let _ = request;
        Err("this host chooses no files".to_owned())
    }

    /// Put `content` where the person says, offering `request`'s name. A platform without a
    /// chooser answers the error saying so.
    ///
    /// # Errors
    ///
    /// Why the content was not written.
    fn save_file(&self, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
        let _ = (request, content);
        Err("this host saves no files".to_owned())
    }

    /// Keep `bytes` in the device's content store as a file called `name` of media type
    /// `media`: what a program that holds bytes makes of them (the files an archive holds,
    /// `.files()`). A platform that holds no store answers the error saying so.
    ///
    /// # Errors
    ///
    /// Why the bytes were not kept.
    fn keep_content(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        let _ = (name, media, bytes);
        Err("this host keeps no files".to_owned())
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

/// Where an instance's `web.get` and `web.post` reach: this device's network, as the platform
/// running the instance answers it.
pub struct PlatformWeb(pub(super) Rc<dyn Platform>);

impl tilolang_runtime::web::WebTransport for PlatformWeb {
    fn fetch(&self, request: &WebRequest) -> WebReply {
        self.0.web(request)
    }
}

/// The device's files, as the platform running the instance answers them: the content store a
/// chosen file's bytes go to, what it holds, and the chooser the person picks with.
pub struct PlatformBlobs(pub(super) Rc<dyn Platform>);

impl BlobHost for PlatformBlobs {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.0.read_content(hash)
    }

    fn choose(&self, request: &FileRequest) -> Result<Option<FileValue>, String> {
        self.0.choose_file(request)
    }

    fn save(&self, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
        self.0.save_file(request, content)
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        self.0.keep_content(name, media, bytes)
    }
}

/// The words an instance shows, in the person's language: every package's catalogues go to
/// the platform, and a key says what the platform's phrases say of it, or the source words where
/// no catalogue holds it (a phrase no catalogue holds is its own key) (i18n.md).
pub struct PlatformWords(pub(super) Rc<dyn Platform>);

impl Words for PlatformWords {
    fn add(&self, catalogues: &[(String, String)]) {
        self.0.add_words(true, catalogues);
    }

    fn add_used(&self, catalogues: &[(String, String)]) {
        self.0.add_words(false, catalogues);
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
