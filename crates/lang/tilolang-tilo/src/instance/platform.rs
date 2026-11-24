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

    /// The families this device sets text in: the toolkit's catalog, which a platform with the
    /// toolkit has. One without it answers none, so a picker offers the face the text already
    /// sets.
    fn fonts(&self) -> Vec<String> {
        Vec::new()
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

    /// Offer `text` as a secret copy that histories and device sync do not keep, taken off the
    /// clipboard after `clear_after` unless something else was copied: what
    /// `clipboard.copy_secret` reaches. A platform that cannot copy a secret apart answers false.
    fn copy_secret_clipboard(&self, text: &str, clear_after: std::time::Duration) -> bool {
        let _ = (text, clear_after);
        false
    }

    /// What the person's selection holds of the first of `types` it offers, as its media type and
    /// bytes: what `clipboard.paste` and `clipboard.paste_image` read. A platform that serves no
    /// clipboard answers none.
    fn paste_clipboard(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        let _ = types;
        None
    }

    /// The entries of the device's folder `path`, within the paths this app was granted: what
    /// `File.list` reads. A platform that holds no device files answers the same refusal for
    /// each call below.
    ///
    /// # Errors
    ///
    /// Why the folder cannot be read (not a folder, not granted, gone).
    fn list_files(&self, path: &str) -> Result<Vec<tilolang_runtime::files::Entry>, String> {
        let _ = path;
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// The entry at `path`, or none when nothing is there: what `File.stat` reads.
    ///
    /// # Errors
    ///
    /// Why the disk could not be asked.
    fn stat_file(&self, path: &str) -> Result<Option<tilolang_runtime::files::Entry>, String> {
        let _ = path;
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// The bytes `path` holds: what `File.read` reads.
    ///
    /// # Errors
    ///
    /// Why they cannot be read.
    fn read_file_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        let _ = path;
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Keep the folder `path` watched, running the package's action `run` when it changes: what
    /// `File.watch` registers, within the paths this app was granted. The action is invoked on
    /// the app's root, as the person the app runs for.
    ///
    /// # Errors
    ///
    /// Why the folder cannot be watched.
    fn watch_folder(&self, path: &str, run: &str) -> Result<String, String> {
        let _ = (path, run);
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Stop the watch `token` names.
    fn unwatch_folder(&self, token: &str) {
        let _ = token;
    }

    /// Make the folder `path`: what `File.mkdir` writes.
    ///
    /// # Errors
    ///
    /// Why it cannot be made (it exists, it is not granted).
    fn make_folder(&self, path: &str) -> Result<tilolang_runtime::files::Entry, String> {
        let _ = path;
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Write `bytes` as the new file `path`: what `File.write` writes.
    ///
    /// # Errors
    ///
    /// Why it cannot be written (it exists, it is not granted).
    fn write_file(
        &self,
        path: &str,
        bytes: &[u8],
    ) -> Result<tilolang_runtime::files::Entry, String> {
        let _ = (path, bytes);
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Rename the entry at `path` to `name` in its own folder: what `File.rename` does.
    ///
    /// # Errors
    ///
    /// Why it cannot be renamed.
    fn rename_file(
        &self,
        path: &str,
        name: &str,
    ) -> Result<tilolang_runtime::files::Entry, String> {
        let _ = (path, name);
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Move the entry at `path` into `folder`: what `File.move` does.
    ///
    /// # Errors
    ///
    /// Why it cannot be moved.
    fn move_file(
        &self,
        path: &str,
        folder: &str,
    ) -> Result<tilolang_runtime::files::Entry, String> {
        let _ = (path, folder);
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Copy the entry at `path` into `folder`: what `File.copy` does.
    ///
    /// # Errors
    ///
    /// Why it cannot be copied.
    fn copy_file(
        &self,
        path: &str,
        folder: &str,
    ) -> Result<tilolang_runtime::files::Entry, String> {
        let _ = (path, folder);
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Remove the entry at `path` for good: what `File.remove` does.
    ///
    /// # Errors
    ///
    /// Why it cannot be removed.
    fn remove_file(&self, path: &str) -> Result<bool, String> {
        let _ = path;
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Put the entry at `path` in this device's trash: what `File.trash` does.
    ///
    /// # Errors
    ///
    /// Why it cannot be trashed.
    fn trash_file(&self, path: &str) -> Result<tilolang_runtime::files::TrashEntry, String> {
        let _ = path;
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// What this device's trash holds: what `File.trash_entries` reads.
    ///
    /// # Errors
    ///
    /// Why it cannot be read.
    fn trash_entries(&self) -> Result<Vec<tilolang_runtime::files::TrashEntry>, String> {
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Restore the trashed entry `name` to where it came from: what `File.restore` does.
    ///
    /// # Errors
    ///
    /// Why it cannot be restored.
    fn restore_file(&self, name: &str) -> Result<tilolang_runtime::files::Entry, String> {
        let _ = name;
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Pack the entries at `paths` into the new archive `to`: what `File.compress` does.
    ///
    /// # Errors
    ///
    /// Why it cannot be packed (not granted, gone, the name is taken).
    fn compress_files(&self, paths: &[String], to: &str) -> Result<i64, String> {
        let _ = (paths, to);
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Extract the archive `from` into the new folder `to`: what `File.extract` does.
    ///
    /// # Errors
    ///
    /// Why it cannot be extracted (not granted, gone, the folder is taken).
    fn extract_file(&self, from: &str, to: &str) -> Result<i64, String> {
        let _ = (from, to);
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// The apps that open the file at `path`, the one it opens with today first: what
    /// `tool.document_apps` lists.
    ///
    /// # Errors
    ///
    /// Why the shell could not answer.
    fn document_apps(
        &self,
        path: &str,
    ) -> Result<Vec<tilolang_runtime::files::DocumentApp>, String> {
        let _ = path;
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Open the file at `path` in `app`: what `tool.open_with` asks.
    ///
    /// # Errors
    ///
    /// Why nothing opened it.
    fn open_with_app(&self, path: &str, app: &str) -> Result<(), String> {
        let _ = (path, app);
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// What the shell opened this app with, as JSON, or empty text: what `tool.context` asks.
    fn launch_context(&self) -> String {
        String::new()
    }

    /// Open a terminal in the folder `folder`: what `tool.terminal` asks.
    ///
    /// # Errors
    ///
    /// Why no terminal opened.
    fn open_terminal_at(&self, folder: &str) -> Result<(), String> {
        let _ = folder;
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Open a shell in `container`, run by `engine`: what `tool.container_shell` asks.
    ///
    /// # Errors
    ///
    /// Why no shell opened.
    fn open_container_shell(&self, container: &str, engine: &str) -> Result<(), String> {
        let _ = (container, engine);
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// Open the address `url` with the app that opens links of its scheme: what
    /// `tool.open_link` asks.
    ///
    /// # Errors
    ///
    /// Why nothing opened it.
    fn open_link(&self, url: &str) -> Result<(), String> {
        let _ = url;
        Err(tilolang_runtime::files::NO_LINKS.to_owned())
    }

    /// Open `file` in the app that opens its kind: what `File.open` asks. The shell picks the app
    /// by the file's first bytes and its name and starts it, or says nothing here opens it or
    /// that this app may not open files.
    ///
    /// # Errors
    ///
    /// Why it did not open.
    fn open_file(&self, file: &tilolang_runtime::files::FileTarget<'_>) -> Result<(), String> {
        let _ = file;
        Err(tilolang_runtime::files::NO_FILES.to_owned())
    }

    /// The code of this moment from the vault item `reference` names, where this platform holds
    /// the workspace's vault: a reading, so a view may show it. `None` for a platform that keeps
    /// no vault, and the view shows its waiting state.
    fn vault_code(&self, reference: &str) -> Option<tilolang_runtime::host_work::VaultCode> {
        let _ = reference;
        None
    }

    /// The device's own work for `app`: an op answered now. The engines this session runs for its
    /// apps answer it ([`crate::engine`]); one it does not run is refused.
    ///
    /// # Errors
    ///
    /// The engine's refusal.
    fn engine_call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        crate::engine::answer(app, op, input)
    }

    /// The device's own work for `app`, started as a job that runs `handlers` as it moves.
    ///
    /// # Errors
    ///
    /// The engine's refusal.
    fn engine_start(
        &self,
        app: &str,
        op: &str,
        input: &str,
        handlers: &tilolang_runtime::host_work::JobHandlers,
    ) -> Result<String, String> {
        let _ = handlers;
        crate::engine::begin(app, op, input)
    }

    /// The device's own work for `app`, started as a stream whose frames reach `push`.
    ///
    /// # Errors
    ///
    /// The engine's refusal.
    fn engine_stream(
        &self,
        app: &str,
        op: &str,
        input: &str,
        push: std::sync::Arc<dyn tilolang_runtime::host_work::Push>,
    ) -> Result<String, String> {
        crate::engine::stream(app, op, input, push)
    }

    /// The job `path` as the device that holds it records it, as its record's JSON; `None` for a
    /// job no device records.
    ///
    /// # Errors
    ///
    /// The read was refused.
    fn engine_job(&self, path: &str) -> Result<Option<serde_json::Value>, String> {
        crate::engine::record(path)
    }

    /// Stop the job `job`: false when no engine runs it or it had finished.
    fn engine_cancel(&self, job: &str) -> bool {
        crate::engine::stop(job)
    }

    /// One check the project's host approved, started by its id for `app` as a job
    /// [`Self::engine_status`] and [`Self::engine_cancel`] follow: run for the invocation the
    /// workspace dispatched to the action asking, through the broker connection the instance
    /// joined. A platform with no such connection runs none.
    ///
    /// # Errors
    ///
    /// Why no check starts.
    fn approved_check(&self, app: &str, check_id: &str) -> Result<String, String> {
        let _ = (app, check_id);
        Err(tilolang_runtime::host_work::NO_APPROVED_CHECKS.to_owned())
    }

    /// What wakes the instance when work it waits for is ready (an index read off the frame).
    fn waker(&self) -> Waker;

    /// The device's PDF engine (`Runtime::set_pdf`), when it can read PDFs: what a package's
    /// `pdf.*` asks. A platform with none leaves the runtime's own answer, which says this
    /// device reads no PDFs.
    fn pdf(&self) -> Option<Rc<dyn tilolang_runtime::pdf::PdfHost>> {
        None
    }

    /// The device's drawing engine (`Runtime::set_canvas`), when it can draw an artwork: what
    /// a package's `canvas.*` asks. A platform with none leaves the runtime's own answer, which
    /// says this device cannot draw.
    fn canvas(&self) -> Option<Rc<dyn tilolang_runtime::canvas::CanvasHost>> {
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

/// This device's text-setting families, as the platform running the instance answers them.
pub struct PlatformFonts(pub(super) Rc<dyn Platform>);

impl tilolang_runtime::fonts::FontHost for PlatformFonts {
    fn families(&self) -> Vec<String> {
        self.0.fonts()
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

/// The vault's answers, as the platform running the instance gives them: what `vault.code`
/// reads, from where the workspace's vault is.
pub struct PlatformVault(pub(super) Rc<dyn Platform>);

impl tilolang_runtime::host_work::VaultHost for PlatformVault {
    fn code(&self, reference: &str) -> Option<tilolang_runtime::host_work::VaultCode> {
        self.0.vault_code(reference)
    }
}

/// The device's own engine work, as the platform answers an app that asks for it
/// (`engine.call`, `engine.start`, `engine.status`, `engine.cancel`).
pub struct PlatformEngine(pub(super) Rc<dyn Platform>);

impl tilolang_runtime::host_work::EngineHost for PlatformEngine {
    fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        self.0.engine_call(app, op, input)
    }

    fn start(
        &self,
        app: &str,
        op: &str,
        input: &str,
        handlers: &tilolang_runtime::host_work::JobHandlers,
    ) -> Result<String, String> {
        self.0.engine_start(app, op, input, handlers)
    }

    fn stream(
        &self,
        app: &str,
        op: &str,
        input: &str,
        push: std::sync::Arc<dyn tilolang_runtime::host_work::Push>,
    ) -> Result<String, String> {
        self.0.engine_stream(app, op, input, push)
    }

    fn job(&self, path: &str) -> Result<Option<serde_json::Value>, String> {
        self.0.engine_job(path)
    }

    fn cancel(&self, job: &str) -> bool {
        self.0.engine_cancel(job)
    }

    fn check(&self, app: &str, check_id: &str) -> Result<String, String> {
        self.0.approved_check(app, check_id)
    }
}

/// The person's clipboard, as the platform running the instance answers it: what a copy offers
/// the device, and what a paste reads off it.
pub struct PlatformClipboard(pub(super) Rc<dyn Platform>);

impl tilolang_runtime::clipboard::ClipboardHost for PlatformClipboard {
    fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
        self.0.copy_clipboard(mime, bytes)
    }

    fn copy_secret(&self, text: &str, clear_after: std::time::Duration) -> bool {
        self.0.copy_secret_clipboard(text, clear_after)
    }

    fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        self.0.paste_clipboard(types)
    }
}

/// The device's own folders, as the platform running the instance answers them: the paths the
/// app was granted, and nothing else. What `File.list`, `File.stat`, `File.read`, `File.watch` and
/// `File.open` reach.
pub struct PlatformFiles(pub(super) Rc<dyn Platform>);

impl tilolang_runtime::files::FilesHost for PlatformFiles {
    fn list(&self, path: &str) -> Result<Vec<tilolang_runtime::files::Entry>, String> {
        self.0.list_files(path)
    }

    fn stat(&self, path: &str) -> Result<Option<tilolang_runtime::files::Entry>, String> {
        self.0.stat_file(path)
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        self.0.read_file_bytes(path)
    }

    fn watch(&self, path: &str, run: &str) -> Result<String, String> {
        self.0.watch_folder(path, run)
    }

    fn unwatch(&self, token: &str) {
        self.0.unwatch_folder(token);
    }

    fn mkdir(&self, path: &str) -> Result<tilolang_runtime::files::Entry, String> {
        self.0.make_folder(path)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<tilolang_runtime::files::Entry, String> {
        self.0.write_file(path, bytes)
    }

    fn rename(&self, path: &str, name: &str) -> Result<tilolang_runtime::files::Entry, String> {
        self.0.rename_file(path, name)
    }

    fn move_entry(
        &self,
        path: &str,
        folder: &str,
    ) -> Result<tilolang_runtime::files::Entry, String> {
        self.0.move_file(path, folder)
    }

    fn copy(&self, path: &str, folder: &str) -> Result<tilolang_runtime::files::Entry, String> {
        self.0.copy_file(path, folder)
    }

    fn remove(&self, path: &str) -> Result<bool, String> {
        self.0.remove_file(path)
    }

    fn trash(&self, path: &str) -> Result<tilolang_runtime::files::TrashEntry, String> {
        self.0.trash_file(path)
    }

    fn trash_entries(&self) -> Result<Vec<tilolang_runtime::files::TrashEntry>, String> {
        self.0.trash_entries()
    }

    fn restore(&self, name: &str) -> Result<tilolang_runtime::files::Entry, String> {
        self.0.restore_file(name)
    }

    fn compress(&self, paths: &[String], to: &str) -> Result<i64, String> {
        self.0.compress_files(paths, to)
    }

    fn document_apps(
        &self,
        path: &str,
    ) -> Result<Vec<tilolang_runtime::files::DocumentApp>, String> {
        self.0.document_apps(path)
    }

    fn open_with(&self, path: &str, app: &str) -> Result<(), String> {
        self.0.open_with_app(path, app)
    }

    fn terminal_at(&self, folder: &str) -> Result<(), String> {
        self.0.open_terminal_at(folder)
    }

    fn open_link(&self, url: &str) -> Result<(), String> {
        self.0.open_link(url)
    }

    fn launch_context(&self) -> String {
        self.0.launch_context()
    }

    fn container_shell(&self, container: &str, engine: &str) -> Result<(), String> {
        self.0.open_container_shell(container, engine)
    }

    fn extract(&self, from: &str, to: &str) -> Result<i64, String> {
        self.0.extract_file(from, to)
    }

    fn open(&self, file: tilolang_runtime::files::FileTarget<'_>) -> Result<(), String> {
        self.0.open_file(&file)
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

/// The device's drawing engine, as the platform handed it over: one `Rc` per instance, so
/// every `canvas.*` call of the instance reaches the same engine.
pub struct SharedCanvas(pub(super) Rc<dyn tilolang_runtime::canvas::CanvasHost>);

impl tilolang_runtime::canvas::CanvasHost for SharedCanvas {
    fn look(
        &self,
        artwork: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String> {
        self.0.look(artwork, request, pictures)
    }

    fn export(
        &self,
        artwork: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String> {
        self.0.export(artwork, request, pictures)
    }

    fn grade(&self, picture: &[u8], adjust: &str) -> Result<Vec<u8>, String> {
        self.0.grade(picture, adjust)
    }

    fn wand(&self, picture: &[u8], request: &str) -> Result<String, String> {
        self.0.wand(picture, request)
    }

    fn find(
        &self,
        artwork: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(String, Option<(String, Vec<u8>, [f64; 4])>), String> {
        self.0.find(artwork, request, pictures)
    }

    fn combine(&self, artwork: &str, request: &str) -> Result<String, String> {
        self.0.combine(artwork, request)
    }

    fn measure(
        &self,
        artwork: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<String, String> {
        self.0.measure(artwork, request, pictures)
    }

    fn compare(
        &self,
        artwork: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String> {
        self.0.compare(artwork, request, pictures)
    }

    fn diff(
        &self,
        before: &str,
        after: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(Option<Vec<u8>>, String), String> {
        self.0.diff(before, after, request, pictures)
    }

    fn select(&self, artwork: &str, request: &str) -> Result<String, String> {
        self.0.select(artwork, request)
    }

    fn inspect(&self, artwork: &str, request: &str) -> Result<String, String> {
        self.0.inspect(artwork, request)
    }

    fn remove_along(&self, artwork: &str, request: &str) -> Result<String, String> {
        self.0.remove_along(artwork, request)
    }

    fn svg(&self, artwork: &str, request: &str) -> Result<String, String> {
        self.0.svg(artwork, request)
    }

    fn svg_import(&self, markup: &str, request: &str) -> Result<String, String> {
        self.0.svg_import(markup, request)
    }

    fn save(&self, artwork: &str, pictures: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
        self.0.save(artwork, pictures)
    }

    fn open(&self, archive: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
        self.0.open(archive)
    }
}
