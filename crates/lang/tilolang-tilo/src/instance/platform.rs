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

use super::Publication;

pub use jiff::Zoned;

/// The device or page's current wall clock as ISO-8601 UTC text: the shared [`Platform::now`]
/// reading for every live host (one clock read, no zone lookup).
#[must_use]
pub fn wall_now() -> String {
    format!("{:.0}", tilo_clock::local::now_instant())
}

/// The device or page's current local moment: the shared [`Platform::moment`] reading for every
/// live host (one clock read, one zone lookup, no text built or parsed).
#[must_use]
pub fn wall_moment() -> Zoned {
    tilo_clock::local::now()
}

/// Parse `now` (ISO-8601 UTC text from [`Platform::now`]), falling back to the wall clock when a
/// synthetic test string is not a valid timestamp.
fn parse_now(now: &str) -> jiff::Timestamp {
    match now.parse() {
        Ok(timestamp) => timestamp,
        Err(error) => {
            tracing::warn!(
                now,
                %error,
                "Platform::now returned an unparseable timestamp; falling back to local wall clock"
            );
            tilo_clock::local::now_instant()
        }
    }
}

/// How the stored `value` of a day, moment or duration (`kind` is `date`, `time` or `duration`)
/// reads as a brief fact (`Today`, `Oct 29`, `2:05 PM`), computed against `moment` in `formats`
/// and `words`. Shared by [`Platform::shown`] and non-window hosts such as agent task sessions.
#[must_use]
pub fn shown_at(
    kind: &str,
    value: &str,
    moment: &Zoned,
    formats: tilo_formats::RegionFormats,
    words: &dyn tilo_formats::Words,
) -> String {
    tilo_formats::shown(
        kind,
        value,
        formats,
        moment.date(),
        moment.datetime(),
        i64::from(moment.offset().seconds()),
        words,
    )
}

/// How the stored `value` of a day or moment (`kind` is `date` or `time`) reads in an
/// interpolated sentence (`"{d}"`), computed against `moment` in `formats` and `words`. Shared
/// by [`Platform::sentence`] and non-window hosts such as agent task sessions.
#[must_use]
pub fn sentence_at(
    kind: &str,
    value: &str,
    moment: &Zoned,
    formats: tilo_formats::RegionFormats,
    words: &dyn tilo_formats::Words,
) -> String {
    tilo_formats::sentence(
        kind,
        value,
        formats,
        moment.date(),
        i64::from(moment.offset().seconds()),
        words,
    )
}

/// Where a platform's catalogues, person language and region formats live: every live host
/// supplies one ([`Platform::locale`]), so catalogue intake, revisions, language, zones, week
/// starts, dates and clocks are implemented once on [`Platform`] instead of retyped — and
/// drifting — on each host.
///
/// Every method is required so a live host cannot silently omit one and fall back to raw dates,
/// Monday-only weeks, English-only date parsing or a frozen revision.
pub trait Locale {
    /// Take in a package's catalogues (`(locale, Fluent text)`). `window` is true for the package
    /// the window shows and false for a package it uses.
    fn add_words(&self, window: bool, catalogues: &[(String, String)]);

    /// Moves when the active display language or catalogues change, so published labels and nouns
    /// are published again.
    fn words_revision(&self) -> u64;

    /// The language the person reads and types in (`de`, `en`), even when the window's own
    /// package only ships another language.
    fn language(&self) -> String;

    /// The person's region formats (date style, 12/24-hour clock, first day of the week).
    fn formats(&self) -> tilo_formats::RegionFormats;

    /// The localized date and clock words (`day-today`, `format-month-short`, …) for
    /// [`tilo_formats`].
    fn format_words(&self) -> &dyn tilo_formats::Words;
}

/// A [`Locale`] backed by functions over thread- or process-global state (such as the toolkit's
/// `I18nManager` and region formats). Hosts that share those globals construct one once as a
/// `static` and override only fields that genuinely differ for them.
#[derive(Clone, Copy)]
pub struct GlobalLocale {
    pub add_words: fn(bool, &[(String, String)]),
    pub words_revision: fn() -> u64,
    pub language: fn() -> String,
    pub formats: fn() -> tilo_formats::RegionFormats,
    pub format_words: fn() -> &'static dyn tilo_formats::Words,
}

impl Locale for GlobalLocale {
    fn add_words(&self, window: bool, catalogues: &[(String, String)]) {
        (self.add_words)(window, catalogues);
    }

    fn words_revision(&self) -> u64 {
        (self.words_revision)()
    }

    fn language(&self) -> String {
        (self.language)()
    }

    fn formats(&self) -> tilo_formats::RegionFormats {
        (self.formats)()
    }

    fn format_words(&self) -> &dyn tilo_formats::Words {
        (self.format_words)()
    }
}

/// What an instance asks of whatever runs it.
pub trait Platform {
    /// Load what a window of the package runs into `host`: the installed package, or the latest
    /// later build of it, with the packages it uses. The later build it
    /// loaded, 0 for the installed package.
    fn load(&self, host: &Host) -> u64;

    /// The later builds of the package the instance takes over as they arrive, when it follows
    /// them.
    fn builds(&self) -> Option<std::sync::Arc<super::LaterBuilds>> {
        None
    }

    /// A later build took over: what the package declares on the API is its now.
    fn surface_changed(&self, app: &str, bundle: &Bundle) {
        let _ = (app, bundle);
    }

    /// The person's catalogues, language and region formats for this host. Every live host
    /// supplies `Some(...)`, implementing `add_words`, `words_revision`, `language`, `zone`,
    /// `zone_name`, `week_start`, `shown`, `sentence` and `clock` through one shared path. Only
    /// headless test stubs that pin a synthetic UTC clock leave this `None`.
    fn locale(&self) -> Option<&dyn Locale> {
        None
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
        if let Some(locale) = self.locale() {
            locale.add_words(window, catalogues);
        }
    }

    /// What the words resolve at now: it moves when the person's language, or a catalogue of
    /// theirs, does, so a package's published labels and nouns are published again in it.
    fn words_revision(&self) -> u64 {
        self.locale().map_or(0, Locale::words_revision)
    }

    /// The time now, as `now` reads it: ISO-8601 UTC text. Live hosts delegate to [`wall_now`].
    fn now(&self) -> String;

    /// The current moment in the person's local zone, used by [`Self::zone`], [`Self::shown`]
    /// and [`Self::sentence`].
    ///
    /// Live hosts override this with [`wall_moment`] so each call makes one clock read and one
    /// zone lookup without building or parsing text. The default parses [`Self::now`] once in the
    /// thread's or device's local zone, so test and benchmark platforms that pin `now()` stay
    /// consistent and warn at most once per call if `now()` is not ISO-8601.
    fn moment(&self) -> Zoned {
        tilo_clock::local::at(parse_now(&self.now()))
    }

    /// The language the person reads and types in (`de`, `en`): what a date or a time typed in
    /// an entry is said in. Empty when the platform names none, which reads as English.
    fn language(&self) -> String {
        self.locale().map_or_else(String::new, Locale::language)
    }

    /// How many seconds the person's clock is ahead of UTC at [`Self::moment`]: the zone `dated`
    /// reads "tomorrow 9am" in. UTC unless the platform knows the person's.
    fn zone(&self) -> i64 {
        if self.locale().is_some() {
            i64::from(self.moment().offset().seconds())
        } else {
            0
        }
    }

    /// The person's zone as its IANA name (`Europe/Berlin`): the zone a repeating item keeps its
    /// wall time of day in across a change of clocks, and the one a day begins in. Nothing
    /// unless the platform knows it, which leaves the offset above as the whole answer.
    fn zone_name(&self) -> Option<String> {
        self.locale().and_then(|_| {
            tilo_clock::local::Zone::here()
                .name()
                .or_else(tilo_clock::local_zone_name)
        })
    }

    /// The day the person's region starts its week on, as days from Monday (`0`): the day a
    /// day's `start_of_week` goes back to. Monday unless the platform knows the person's region.
    fn week_start(&self) -> i64 {
        self.locale().map_or(0, |locale| {
            i64::from(locale.formats().first_day.weekday().to_monday_zero_offset())
        })
    }

    /// How the stored `value` of a day or a time of day (`kind` is `date` or `time`) reads in a
    /// sentence — a row's line, a notice — where a view cannot stand: a day as the day fact
    /// (`Today`, `Friday`, `Jun 29`), a time of day as a list shows when something came. Computed
    /// against [`Self::moment`]; stored text unless the platform formats.
    fn shown(&self, kind: &str, value: &str) -> String {
        let Some(locale) = self.locale() else {
            return value.to_owned();
        };
        let moment = self.moment();
        shown_at(
            kind,
            value,
            &moment,
            locale.formats(),
            locale.format_words(),
        )
    }

    /// How a day or a time of day named in a sentence (`"{d}"`) reads, in the person's formats: a
    /// day as `Jun 29`, or `Jun 29, 2025` another year, a moment as `Jun 29, 6:00 PM`. Computed
    /// against [`Self::moment`]; stored text unless the platform formats.
    fn sentence(&self, kind: &str, value: &str) -> String {
        let Some(locale) = self.locale() else {
            return value.to_owned();
        };
        let moment = self.moment();
        sentence_at(
            kind,
            value,
            &moment,
            locale.formats(),
            locale.format_words(),
        )
    }

    /// The time of day `hour`:`minute` as the person reads a clock (`14:05`, `2:05 PM`), in the
    /// style they chose in Settings: what a view says when it names the time of a moment
    /// (`a.fetched.clock()`). A 24-hour clock unless the platform knows the person's style, which
    /// a host with the toolkit does.
    fn clock(&self, hour: u32, minute: u32) -> String {
        let Some(locale) = self.locale() else {
            return format!("{hour:02}:{minute:02}");
        };
        locale
            .formats()
            .time
            .format_hm(hour, minute, locale.format_words())
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

    /// Ask the person for files, taking what they pick into the content store: one, several,
    /// or the files of a folder, as `request` asks. An empty list when they dismiss the
    /// chooser; the error says why the files could not be taken in (a size, a media type, the
    /// store's failure). A platform without a chooser answers the error saying so.
    ///
    /// # Errors
    ///
    /// Why there are no files: there is no chooser, or the one there is refused it.
    fn choose_files(&self, request: &FileRequest) -> Result<Vec<FileValue>, String> {
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

    /// A preview of `file` no larger than `size` pixels a side, kept as a file of its own: what
    /// `File.thumbnail` answers. `None` when the file has no preview (it is no image); a
    /// platform that keeps no preview cache answers the error saying so.
    ///
    /// # Errors
    ///
    /// Why there is no preview.
    fn thumbnail(&self, file: &FileValue, size: u32) -> Result<Option<FileValue>, String> {
        let _ = (file, size);
        Err("this host keeps no previews".to_owned())
    }

    /// Offer `bytes` of `mime` (`text/plain` or an image type) as what the person copied: what
    /// `clipboard.copy` and `clipboard.copy_image` reach. A platform that serves no clipboard
    /// answers false.
    fn copy_clipboard(&self, mime: &str, bytes: &[u8]) -> bool {
        let _ = (mime, bytes);
        false
    }

    /// What the person's selection holds of the first of `types` it offers, as its media type and
    /// bytes: what `clipboard.paste` and `clipboard.paste_image` read. A platform that serves no
    /// clipboard answers none.
    fn paste_clipboard(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        let _ = types;
        None
    }

    /// What wakes the instance when work it waits for is ready (an index read off the frame).
    fn waker(&self) -> Waker;

    /// The device's PDF engine (`Runtime::set_pdf`), when it can read PDFs: what a package's
    /// `pdf.*` asks. A platform with none leaves the runtime's own answer, which says this
    /// device reads no PDFs.
    fn pdf(&self) -> Option<Rc<dyn tilolang_runtime::pdf::PdfHost>> {
        None
    }

    /// Publish what `host` holds until the returned hold is dropped.
    fn publish(&self, host: &Host) -> Option<Box<dyn Publication>> {
        let _ = host;
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

    fn choose(&self, request: &FileRequest) -> Result<Vec<FileValue>, String> {
        self.0.choose_files(request)
    }

    fn save(&self, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
        self.0.save_file(request, content)
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        self.0.keep_content(name, media, bytes)
    }

    fn thumbnail(&self, file: &FileValue, size: u32) -> Result<Option<FileValue>, String> {
        self.0.thumbnail(file, size)
    }
}

/// The person's clipboard, as the platform running the instance answers it: what a copy offers
/// the device, and what a paste reads off it.
pub struct PlatformClipboard(pub(super) Rc<dyn Platform>);

impl tilolang_runtime::clipboard::ClipboardHost for PlatformClipboard {
    fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
        self.0.copy_clipboard(mime, bytes)
    }

    fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        self.0.paste_clipboard(types)
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

/// The device's PDF engine, as the platform handed it over: one `Rc` per instance, so every
/// `pdf.*` call of the instance reaches the same engine.
pub struct SharedPdf(pub(super) Rc<dyn tilolang_runtime::pdf::PdfHost>);

impl tilolang_runtime::pdf::PdfHost for SharedPdf {
    fn pages(&self, bytes: &[u8]) -> Result<String, String> {
        self.0.pages(bytes)
    }

    fn outline(&self, bytes: &[u8]) -> Result<String, String> {
        self.0.outline(bytes)
    }

    fn text(&self, bytes: &[u8], page: i64) -> Result<String, String> {
        self.0.text(bytes, page)
    }

    fn links(&self, bytes: &[u8], page: i64) -> Result<String, String> {
        self.0.links(bytes, page)
    }

    fn fields(&self, bytes: &[u8]) -> Result<String, String> {
        self.0.fields(bytes)
    }

    fn markup(&self, bytes: &[u8]) -> Result<String, String> {
        self.0.markup(bytes)
    }

    fn picture(&self, bytes: &[u8], page: i64, width: i64) -> Result<Vec<u8>, String> {
        self.0.picture(bytes, page, width)
    }

    fn import(&self, bytes: &[u8]) -> Result<String, String> {
        self.0.import(bytes)
    }

    fn save(&self, sources: &[&[u8]], request: &str) -> Result<Vec<u8>, String> {
        self.0.save(sources, request)
    }
}
