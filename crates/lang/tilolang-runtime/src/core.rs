//! The host core: the state every import and every invocation shares.
//!
//! `Core` implements [`Host`], so the engine calls straight into it. Everything is behind
//! `RefCell`s and no borrow is held across a call into guest code, so a run's `invoke` can
//! run an action (which pushes a frame, runs the guest and pops it) while the run's own
//! frame is live.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::{Rc, Weak};

use serde_json::Value as Json;

use crate::abi::{self, Imp};
use crate::authority::Authority;
use crate::engine::{Engine, EngineLimits, Host, HostFail, Wasm};
use crate::handles::Frames;
use crate::registry::Registry;
use crate::store::Store;
use crate::value::Value;

use Imp::{
    Abort, ActCall, ActChoose, ActChooseFolder, ActMadeFrom, ActMany, ActOpen, ActRef, ActRunLine,
    ActSave, ActSet, ActThen, ActUndo, Actions, ActorApp, ActorEq, ActorId, ActorKind, ActorName,
    ActorRole, AgentNamed, ArgAt, Base64Decode, BytesLength, CallFn, CallMembers, CanvasExport,
    CanvasGrade, CanvasLook, CanvasOpen, CanvasSave, CanvasSvg, CanvasSvgImport, CanvasWand,
    ChangeView, ClipboardCopyImage, ClipboardCopyText, ClipboardPasteImage, ClipboardPasteText,
    CollCount, CollCounted, CollEmpty, CollInsert, CollInsertAs, CollInsertKeyed, CollMove,
    CollMoveAmong, CollMoveTo, CollOrdered, CollPlace, CollRelatives, CollRemove, CollWhere,
    CollWhereWith, CountBy, DateShow, DayParse, DayPart, DayShift, DayStartOfWeek, DayWeekday,
    Describe, DescribeRef, DurationText, Earlier, EditField, EngineCall, EngineCancel,
    EnginePicture, EngineStart, EngineState, EngineStatus, EngineStream, EntryParts, EnumGetF,
    EnumGetH, EnumGetI, EnumIs, EnumNew, EnumSetF, EnumSetH, EnumSetI, EventPart, EventValue,
    FieldChanged, FileAt, FileBook, FileBookCover, FileBookInfo, FileCompress, FileCopy, FileCrop,
    FileEncode, FileExtract, FileFiles, FileImageInfo, FileList, FileMarkdown, FileMkdir, FileMove,
    FileOfBytes, FileOfDocument, FileOfText, FileOpen, FileOpenPath, FilePart, FileReadPath,
    FileRemove, FileRename, FileResize, FileRestore, FileSize, FileStat, FileText, FileThumbnail,
    FileTrash, FileTrashEntries, FileTurn, FileWatch, FileWrite, FileZip, FilterLit, Filters, Find,
    FirstUnseen, FloatAbs, FloatAcos, FloatAsin, FloatAtan, FloatCbrt, FloatCeil, FloatCeilTo,
    FloatClamp, FloatCos, FloatCosh, FloatExp, FloatFloor, FloatFloorTo, FloatFract, FloatGamma,
    FloatIsFinite, FloatIsNan, FloatLn, FloatLog2, FloatLog10, FloatMax, FloatMin, FloatPow,
    FloatRound, FloatRoundTo, FloatSign, FloatSin, FloatSinh, FloatSqrt, FloatTan, FloatTanh,
    FloatToDegrees, FloatToRadians, FloatToStr, FloatTrunc, FontFamilies, ForeignRef, Grep,
    GroupBy, IdentPart, IntAbs, IntClamp, IntMax, IntMin, IntToStr, InvocationPop, InvocationPush,
    Invoke, Invoked, Invoker, IsError, IsSome, JsonDecode, JsonEncode, JsonParse, Keep, Lenses,
    LinesRun, Links, ListAvg, ListConcat, ListEnd, ListGetF, ListGetH, ListGetI, ListIndexOf,
    ListLen, ListNew, ListPrevious, ListPushF, ListPushH, ListPushI, ListReverse, ListSortByKey,
    ListSumF, ListSumI, MadeFrom, MapGetH, MapGetValue, MapKeys, MapKeysValue, MapNew, MapPut,
    MapPutValue, MapRemove, MapRemoveValue, MapSetAdd, MapSetRemove, MatchGroup, MayRead, Me,
    Members, Mentions, MetaOf, MissingV, ModelCall, NoneV, Notify, Now, Occurrences, OfType,
    Outside, OutsideAt, PageCall, PdfFields, PdfImport, PdfLinks, PdfMarkup, PdfOutline, PdfPages,
    PdfPicture, PdfSave, PdfText, PersonNamed, Phrase, Print, PrintRows, ProposalNote, Query,
    QueueBlock, RandomChoice, RandomFloat, RandomId, RandomInt, RandomShuffle, Reach, ReadText,
    ReadWith, RecNew, RecSetF, RecSetH, RecSetI, RefEq, RefId, RefIs, RefLit, ResAddF, ResAddI,
    ResGetF, ResGetH, ResGetI, ResSetF, ResSetH, ResSetI, ResTextSet, ResourceRef, RichAppended,
    RichEmbed, RichEmpty, RichFromMarkdown, RichMarkdown, RichMarked, RichNew, RichPart,
    RichRelinked, RichTime, RowOpen, RunChanges, SeenBy, SetAdd, SetRemove, SheetEdit, SheetEntry,
    SheetFile, SheetNumbers, SheetOfFile, SheetRanges, SheetRecords, SheetValues, Single, SomeF,
    SomeH, SomeI, SortLit, SpaceF, SpaceI, Speakers, StageReading, StateAt, StateGet, Steps,
    StrCmp, StrConcat, StrContains, StrEndsWith, StrEq, StrFind, StrJoin, StrLen, StrLit, StrLower,
    StrMatches, StrMisspelled, StrReplace, StrSlice, StrSplit, StrStartsWith, StrSuggestions,
    StrToFloat, StrToInt, StrTrim, StrUpper, StrWords, Suggest, Take, TimeClock, TimeDay,
    TimeDayIn, TimeFromWall, TimeOfDay, TimeParse, TimeShift, TimeSpan, TimeWall, Today,
    ToolContainerShell, ToolContext, ToolDocumentApps, ToolOpenWith, ToolTerminal, TryInvoke,
    TupleGetF, TupleGetH, TupleGetI, TupleNew, TupleSetF, TupleSetH, TupleSetI, UiAction,
    UiActions, UiAnchor, UiBind, UiBindBegin, UiBindState, UiBindStateAt, UiChild, UiNode, UiOn,
    UiPropF, UiPropH, UiPropI, UiPropPic, Unseen, UnwrapF, UnwrapH, UnwrapI, ValueEq, VersionEnd,
    Versions, WallClock, WebBody, WebGet, WebHeaders, WebOk, WebPost, WebStatus, WebStatusText,
    WebText, Withdraw, Word, Zones,
};

pub type HResult<T> = Result<T, HostFail>;

pub fn trap<T>(msg: impl Into<String>) -> HResult<T> {
    Err(HostFail::Trap(msg.into()))
}

impl From<String> for HostFail {
    fn from(msg: String) -> Self {
        Self::Trap(msg)
    }
}

/// Answers `model_call`. Calls are journaled, so an implementation may be nondeterministic.
pub trait ModelHost {
    /// # Errors
    ///
    /// A message when the model cannot answer.
    fn call(&self, kind: &str, input: &Value) -> Result<Value, String>;
}

/// What became of an outside effect handed to a sink (convergence.md, "What people see").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The service took it: it is done.
    Delivered,
    /// The service refused it, for this reason. It is never tried again.
    Rejected(String),
    /// Handed over; the service has not answered yet. Asked again on the next settle.
    ///
    /// Resolved by the sink that says it: it is obliged to have its owner settle again once
    /// the service answered (an origin reports `Polled::settle` from the poll that took the
    /// answer in), and to answer every effect it took, its service going away included.
    Pending,
    /// The service could not be reached, for this reason, so it never got it: the effect waits
    /// to go online and is tried again when the network comes back.
    Offline(String),
}

/// Where outside effects are delivered once settled.
pub trait OutsideSink {
    /// Hand the effect to the service, or say what became of it since it was handed over.
    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery;
}

/// How an actor is called where a view shows it (`actor.name()`).
pub trait NameHost {
    /// The name to show for `who`.
    fn name(&self, who: &crate::ids::Actor) -> String;

    /// The colour the host paints `who` in, as a slot of its presence palette: `None` leaves it
    /// to whatever draws the view.
    fn hue(&self, who: &crate::ids::Actor) -> Option<u8> {
        let _ = who;
        None
    }

    /// The picture the host shows for `who`, as the address a face reads its bytes from (a
    /// `data:` or `tilo-blob://` URL): what every view showing them as an author draws.
    /// `None` shows their initials.
    fn picture(&self, who: &crate::ids::Actor) -> Option<String> {
        let _ = who;
        None
    }
}

/// How a value a change shows reads as text, where a sentence must carry it rather than a view
/// node (a row's line, a notice): a day or a time of day in the person's format. Until it is set,
/// the value's stored text.
pub trait ShownHost {
    /// `value` of the shown kind `kind` (`date` or `time`), as the person reads it.
    fn shown(&self, kind: &str, value: &str) -> String;

    /// The time of day `hour`:`minute` as the person reads a clock (`14:05`, `2:05 PM`), in the
    /// style they chose in Settings: what `t.clock()` says of a moment. A 24-hour clock until the
    /// host says.
    fn clock(&self, hour: u32, minute: u32) -> String {
        format!("{hour:02}:{minute:02}")
    }
}

/// The host's answer for the person's zone name, asked each time a rule is read.
type NamedZone = Box<dyn Fn() -> Option<String>>;

/// How a day or a time of day reads, as a plain function (`Runtime::set_shown_by`).
pub type ShownBy = dyn Fn(&str, &str) -> String;

/// How a day or a time of day named in a sentence reads, as a plain function
/// (`Runtime::set_sentence_by`).
pub type SentenceBy = dyn Fn(&str, &str) -> String;

pub struct Core {
    me: Weak<Core>,
    pub engine: Engine,
    pub registry: RefCell<Registry>,
    pub frames: RefCell<Frames>,
    pub store: RefCell<Box<dyn Store>>,
    pub authority: Box<dyn Authority>,
    pub models: RefCell<Option<Box<dyn ModelHost>>>,
    /// The device's PDF engine (`Runtime::set_pdf`): what `pdf.*` asks of a PDF's bytes. Until
    /// the host sets one, every call says this device reads no PDFs.
    pub pdf: RefCell<Box<dyn crate::pdf::PdfHost>>,
    /// The device's drawing engine (`Runtime::set_canvas`): what `canvas.*` asks of an
    /// artwork's records. Until the host sets one, every call says this device cannot draw.
    pub canvas: RefCell<Box<dyn crate::canvas::CanvasHost>>,
    /// Where `web.get` and `web.post` reach (`Runtime::set_web`): the network of the device
    /// whose code makes the request. Until the host sets one, every request answers with a
    /// failure saying the host reaches no services.
    pub web: RefCell<Option<Box<dyn crate::web::WebTransport>>>,
    /// The page the app's window shows (`Runtime::set_page`): what `page.call` asks, and what a
    /// replay answers from its journal instead of asking the page again. Until the host sets
    /// one, a call says this app's window shows no page.
    pub page: RefCell<Option<Box<dyn crate::page::PageHost>>>,
    /// The device's files (`Runtime::set_blobs`): the content store a file's bytes are kept in,
    /// and the chooser the person picks with. Until the host sets one, it holds no files.
    pub blobs: RefCell<Option<Box<dyn crate::blob::BlobHost>>>,
    /// The families text is set in on this device (`Runtime::set_fonts`): what a reading face's
    /// picker offers. Until the host sets one, it offers none.
    pub fonts: RefCell<Option<Box<dyn crate::fonts::FontHost>>>,
    /// The device's own folders, within the paths the app was granted, and the app that opens a
    /// file's kind (`Runtime::set_device_files`). Until the host sets one, it reads none and
    /// opens none.
    pub files: RefCell<Option<Box<dyn crate::files::FilesHost>>>,
    /// The person's clipboard (`Runtime::set_clipboard`): what a copy offers and what a paste
    /// reads. Until the host sets one, nothing copies and nothing pastes.
    pub clipboard: RefCell<Option<Box<dyn crate::clipboard::ClipboardHost>>>,
    /// The host a notice is raised to (`Runtime::set_notices`): the desktop's notifications, or
    /// a test's record. Until the host sets one, a notice is only the record the run wrote.
    pub notices: RefCell<Option<Box<dyn crate::notify::Notices>>>,
    /// The device's own work (`Runtime::set_engine`): what it answers an app that asks for a
    /// decode, a render or a raster it cannot do itself.
    pub engine_work: RefCell<Option<Box<dyn crate::host_work::EngineHost>>>,
    /// The newest frame of each stream the device's engines ran (`engine.stream`): what the app's
    /// window paints by the stream's address.
    pub live_frames: std::sync::Arc<crate::host_work::LiveFrames>,
    /// What names actors; the id itself until the host says.
    pub names: RefCell<Option<Box<dyn NameHost>>>,
    /// How a day or a time of day reads in a sentence; its stored text until the host says
    /// (`Runtime::set_shown_host`).
    pub shown: RefCell<Option<Box<dyn ShownHost>>>,
    /// [`Self::shown`] as a plain function, when a caller set one (`Runtime::set_shown_by`): a
    /// day or a time of day in the person's formats, without a trait of this crate.
    pub shown_by: RefCell<Option<Box<ShownBy>>>,
    /// How a day or a time of day named in a sentence reads, in the person's formats
    /// (`Runtime::set_sentence_by`).
    pub sentence_by: RefCell<Option<Box<SentenceBy>>>,
    /// The workspace's members, as the host last gave them (`members(r)`).
    pub members: RefCell<crate::members::Members>,
    /// The workspace package's `members` root, when the host serves its reading from these
    /// members (`Runtime::serve_members`): every change to them is that reading's too.
    pub members_root: RefCell<Option<crate::value::ResourceRef>>,
    pub sink: RefCell<Option<Box<dyn OutsideSink>>>,
    /// The network came back: entries waiting to go online are tried again on the next settle.
    pub retry_waiting: std::cell::Cell<bool>,
    /// The words packages and the platform show, in the reader's language: the source words
    /// until the host says (`Runtime::set_words`).
    pub words: RefCell<Box<dyn crate::words::Words>>,
    /// Every loaded package's catalogues and which package of the window they belong to,
    /// handed to the words the host sets later too.
    pub catalogues: RefCell<Vec<crate::words::LoadedCatalogues>>,
    /// Compiled embedded blocks.
    pub embeds: RefCell<crate::embed::EmbedState>,
    /// The view state of every view, and the platform's own readings.
    pub view_states: RefCell<crate::view_state::ViewStates>,
    /// Whether a view read who is in a call (`call_members(r)`): the host follows the system's
    /// calls only for an app that shows them.
    pub calls_read: std::cell::Cell<bool>,
    /// The invocations views read (`invoked(id)`), by id: the host reads each one's record and
    /// keeps reading it while it moves.
    pub invoked_read: RefCell<std::collections::BTreeSet<String>>,
    /// What `now` reads: the current time as ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`. Read
    /// once per outermost invocation; until the host sets one, it is the epoch.
    pub clock: RefCell<Option<Box<dyn Fn() -> String>>>,
    /// How many seconds the person's clock is ahead of UTC, which `dated` reads a day and a
    /// time typed against; UTC until the host says.
    pub zone: RefCell<Option<Box<dyn Fn() -> i64>>>,
    /// The person's zone as its IANA name (`Europe/Berlin`), which keeps a repeating item's
    /// wall time of day across a change of clocks; no zone until the host names one.
    pub zone_name: RefCell<Option<NamedZone>>,
    /// The day the person's region starts its week on, as days from Monday (`0`): Monday until
    /// the host says.
    pub week_start: RefCell<Option<Box<dyn Fn() -> i64>>>,
    /// References to collection items built so far: what a view materialized of the
    /// collections it read.
    pub items_built: std::cell::Cell<u64>,
    /// What the program being run printed and kept, while one runs.
    pub program: RefCell<Option<crate::program::ProgramIo>>,
    /// What finds resources by text for programs' `find`.
    pub text_search: RefCell<Option<Box<crate::program::TextSearch>>>,
    /// Where a task's programs send what they change, in a host that runs no app's code
    /// ([`crate::delegate`]); `None` where the runtime runs actions itself.
    pub invoker: RefCell<Option<Box<dyn crate::delegate::Invoker>>>,
    /// Whether [`Self::invoker`] takes this runtime's own actions too, as a host that holds no
    /// records needs, or only other apps'.
    pub delegates_own: std::cell::Cell<bool>,
    /// The verbs the host's resources publish that no loaded package declares, by resource and
    /// verb ([`crate::published`]): what a view names on the resource it shows (`send` on a
    /// channel) and the runtime sends to the app that publishes it, as it sends any other app's
    /// action to that app's action API. Empty until the host hands it in.
    pub published: RefCell<crate::published::Published>,
    /// Where a task's programs ask each app what they read of it (`grep`, `find`, `read`,
    /// `describe`), while they run: a host that holds no records; `None` where the runtime
    /// answers what it holds itself.
    pub reader: RefCell<Option<Box<crate::program::ReadThrough>>>,
    /// Whether the invocation being made is a `try`'s, whose failure the program handles.
    pub trying: std::cell::Cell<bool>,
    /// Whether one write the code asked for is being made ([`Core::journaled_write`]): the reads
    /// and changes it makes on its way are its own, and the journal records the write whole.
    pub writing: std::cell::Cell<bool>,
    /// Whether a run is being driven: its first change opens it and its last closes it, and the
    /// calls it makes are steps of it rather than runs of their own.
    pub running: std::cell::Cell<bool>,
    /// The resources of apps this runtime does not run: how they print, and their readings, as
    /// the host gives them.
    pub foreign: RefCell<crate::foreign::Foreign>,
    /// The apps a view read a resource of before their package was loaded here: what the host
    /// loads ([`crate::Runtime::take_wanted_packages`]).
    pub wanted_packages: RefCell<crate::foreign::WantedPackages>,
    /// The view calls that could not be shown: their package did not load here, or their view
    /// failed when it ran ([`crate::foreign::FailedViews`]). What each call draws as its failed
    /// state, and what the host is told once so the session can say it (shell.md §6).
    pub failed_views: RefCell<crate::foreign::FailedViews>,
    /// The seen marks the person set here, for the host to keep in their own record
    /// ([`crate::seen`]).
    pub seen_writes: RefCell<Vec<crate::seen::SeenWrite>>,
    /// What the last catch-up took away, so that undoing it can give it back
    /// ([`crate::seen::SeenUndo`]).
    pub seen_undo: RefCell<Option<crate::seen::SeenUndo>>,
    /// What each unit costs while a profile is on, shared with the engine that measures fuel.
    pub(crate) costs: Rc<crate::cost::Recorder>,
    /// The row being rendered, while a view renders one: the rows of the list it is a row
    /// of, and where in them it stands. A row reads the rows around it by that place, without
    /// walking the list.
    pub row_place: RefCell<Option<(Rc<crate::value::CollRef>, usize)>>,
    /// The Rust apps' views the host told the runtime, by app: what a resource of a Rust app's
    /// type is drawn with, where a tilolang view's export would be called
    /// ([`crate::rust_views`]).
    pub rust_views: crate::rust_views::Shared,
    /// Whether a view showed something only the window knows while it built one of its nodes,
    /// when the view's own state was already borrowed for the build: taken onto the view when
    /// the node is built ([`Self::with_view`], [`Self::read_person`]).
    pub(crate) person_shown: std::cell::Cell<bool>,
    /// What an earlier build of each app kept in documents of its own, by app
    /// ([`crate::earlier`]): what that app's migrations read with `earlier(r)`. Empty until the
    /// host hands it in (`Runtime::set_earlier`).
    pub earlier: RefCell<std::collections::BTreeMap<String, Rc<[crate::earlier::EarlierItem]>>>,
}

/// What `now` reads while the host has set no clock.
pub const EPOCH: &str = "1970-01-01T00:00:00Z";

/// Typed access to the wasm arguments of one import call.
pub struct Args<'a>(&'a [Wasm]);

impl Args<'_> {
    pub fn h(&self, i: usize) -> HResult<i32> {
        match self.0.get(i) {
            Some(Wasm::I32(v)) => Ok(*v),
            _ => trap(format!("argument {i} is not an i32")),
        }
    }

    pub fn long(&self, i: usize) -> HResult<i64> {
        match self.0.get(i) {
            Some(Wasm::I64(v)) => Ok(*v),
            _ => trap(format!("argument {i} is not an i64")),
        }
    }

    pub fn float(&self, i: usize) -> HResult<f64> {
        match self.0.get(i) {
            Some(Wasm::F64(v)) => Ok(*v),
            _ => trap(format!("argument {i} is not an f64")),
        }
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "every import returns through the same result shape"
)]
pub fn ret_i32(v: i32) -> HResult<Option<Wasm>> {
    Ok(Some(Wasm::I32(v)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "every import returns through the same result shape"
)]
pub fn ret_i64(v: i64) -> HResult<Option<Wasm>> {
    Ok(Some(Wasm::I64(v)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "every import returns through the same result shape"
)]
pub fn ret_f64(v: f64) -> HResult<Option<Wasm>> {
    Ok(Some(Wasm::F64(v)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "every import returns through the same result shape"
)]
pub fn ret_void() -> HResult<Option<Wasm>> {
    Ok(None)
}

pub fn ordering_to_i32(o: Ordering) -> i32 {
    match o {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

impl Core {
    /// # Errors
    ///
    /// A message when the engine cannot be built.
    pub fn new(
        store: Box<dyn Store>,
        authority: Box<dyn Authority>,
        limits: EngineLimits,
    ) -> Result<Rc<Self>, String> {
        let engine = Engine::new(limits)?;
        let costs = Rc::new(crate::cost::Recorder::default());
        engine.watch_costs(Rc::clone(&costs));
        let core = Rc::new_cyclic(|me| Self {
            me: me.clone(),
            engine,
            registry: RefCell::new(Registry::default()),
            frames: RefCell::new(Frames::default()),
            store: RefCell::new(store),
            authority,
            models: RefCell::new(None),
            pdf: RefCell::new(Box::new(crate::pdf::NoPdf)),
            canvas: RefCell::new(Box::new(crate::canvas::NoCanvas)),
            web: RefCell::new(None),
            page: RefCell::new(None),
            blobs: RefCell::new(None),
            fonts: RefCell::new(None),
            files: RefCell::new(None),
            clipboard: RefCell::new(None),
            notices: RefCell::new(None),
            engine_work: RefCell::new(None),
            live_frames: std::sync::Arc::new(crate::host_work::LiveFrames::default()),
            names: RefCell::new(None),
            shown: RefCell::new(None),
            shown_by: RefCell::new(None),
            sentence_by: RefCell::new(None),
            members: RefCell::new(crate::members::Members::default()),
            members_root: RefCell::new(None),
            sink: RefCell::new(None),
            retry_waiting: std::cell::Cell::new(false),
            words: RefCell::new(Box::new(crate::words::SourceWords)),
            catalogues: RefCell::new(Vec::new()),
            embeds: RefCell::new(crate::embed::EmbedState::default()),
            view_states: RefCell::new(crate::view_state::ViewStates::default()),
            calls_read: std::cell::Cell::new(false),
            invoked_read: RefCell::new(std::collections::BTreeSet::new()),
            clock: RefCell::new(None),
            zone: RefCell::new(None),
            week_start: RefCell::new(None),
            zone_name: RefCell::new(None),
            items_built: std::cell::Cell::new(0),
            program: RefCell::new(None),
            text_search: RefCell::new(None),
            invoker: RefCell::new(None),
            delegates_own: std::cell::Cell::new(false),
            published: RefCell::new(crate::published::Published::default()),
            reader: RefCell::new(None),
            trying: std::cell::Cell::new(false),
            writing: std::cell::Cell::new(false),
            running: std::cell::Cell::new(false),
            foreign: RefCell::new(crate::foreign::Foreign::default()),
            rust_views: crate::rust_views::Shared::default(),
            person_shown: std::cell::Cell::new(false),
            earlier: RefCell::new(std::collections::BTreeMap::new()),
            wanted_packages: RefCell::new(crate::foreign::WantedPackages::default()),
            failed_views: RefCell::new(crate::foreign::FailedViews::default()),
            seen_writes: RefCell::new(Vec::new()),
            seen_undo: RefCell::new(None),
            costs,
            row_place: RefCell::new(None),
        });
        // The types the host provides to every module, such as `Invocation`.
        let host = crate::registry::Package::new(
            crate::ids::ModuleHash("host".to_owned()),
            tilolang::host_meta(),
        )?;
        core.registry.borrow_mut().add(host)?;
        // The actions a workspace offers, read and called by code: a package whose calls the
        // host answers over its action connection.
        let actions = crate::registry::Package::new(
            crate::ids::ModuleHash("actions".to_owned()),
            tilolang::actions_meta(),
        )?;
        core.registry.borrow_mut().add(actions)?;
        Ok(core)
    }

    /// A strong handle to this core, for passing to the engine as the host.
    pub fn rc(&self) -> Result<Rc<Self>, String> {
        self.me
            .upgrade()
            .ok_or_else(|| "the runtime has been dropped".to_owned())
    }

    /// Put a value in the current frame and answer its handle.
    pub fn ret_h(&self, v: Value) -> HResult<Option<Wasm>> {
        let h = self.frames.borrow_mut().alloc(v)?;
        ret_i32(h)
    }

    /// A copy of the value behind a handle.
    pub fn val(&self, h: i32) -> HResult<Value> {
        Ok(self.frames.borrow().get(h)?.clone())
    }

    /// Run `f` on the value behind a handle without copying it.
    pub fn with_val<R>(&self, h: i32, f: impl FnOnce(&Value) -> HResult<R>) -> HResult<R> {
        let frames = self.frames.borrow();
        f(frames.get(h)?)
    }

    pub fn text(&self, h: i32) -> HResult<String> {
        self.with_val(h, |v| match v {
            Value::Text(s) => Ok(s.clone()),
            other => trap(format!("expected text, got {}", other.kind_name())),
        })
    }

    pub fn bytes(&self, h: i32) -> HResult<Vec<u8>> {
        self.with_val(h, |v| match v {
            Value::Bytes(b) => Ok((**b).clone()),
            other => trap(format!("expected bytes, got {}", other.kind_name())),
        })
    }

    pub fn module_of_frame(&self) -> HResult<crate::ids::ModuleHash> {
        Ok(self.frames.borrow().top()?.module.clone())
    }
}

impl Host for Core {
    fn import(&self, id: usize, memory: &[u8], args: &[Wasm]) -> Result<Option<Wasm>, HostFail> {
        let Some(imp) = abi::imp(id) else {
            return trap(format!("unknown import {id}"));
        };
        self.dispatch(imp, memory, &Args(args))
            .map_err(|e| match e {
                HostFail::Trap(m) => HostFail::Trap(format!("{}: {m}", abi::name(id))),
                suspend @ HostFail::Suspend => suspend,
            })
    }
}

impl Core {
    /// Take `json` as the reading of `target`, a resource of another app: the views that read it
    /// run again. Whether it differs from the reading held.
    pub(crate) fn take_reading(&self, target: &crate::value::ResourceRef, json: Json) -> bool {
        let mut foreign = self.foreign.borrow_mut();
        let first = foreign.generation(target) == 0;
        if !foreign.set_reading(target, json) {
            return false;
        }
        let generation = foreign.generation(target);
        let known = foreign.known();
        // A record standing in the reading (a network row in the network's) reads its fields
        // from it: a view that read one runs again as one that read the reading does.
        let standing = foreign.standing_in(target);
        drop(foreign);
        let seen = i64::try_from(generation).unwrap_or(i64::MAX);
        let mut states = self.view_states.borrow_mut();
        states.set(
            crate::foreign::reading_key(target),
            crate::value::Value::Int(seen),
        );
        for record in standing {
            states.set(
                crate::foreign::reading_key_at(&record),
                crate::value::Value::Int(seen),
            );
        }
        // Any reading that comes in: a view call waiting on one it could not know runs again.
        states.set(
            crate::foreign::arrived_key(),
            crate::value::Value::Int(seen),
        );
        // A resource known here for the first time: views that looked it up by id
        // (`resource(id)`) and found nothing run again.
        if first {
            states.set(
                crate::foreign::known_key(),
                crate::value::Value::Int(i64::try_from(known).unwrap_or(i64::MAX)),
            );
        }
        true
    }

    /// The host clock's time now, or the epoch without one.
    pub(crate) fn clock_now(&self) -> String {
        self.clock
            .borrow()
            .as_ref()
            .map_or_else(|| EPOCH.to_owned(), |clock| clock())
    }

    /// The time of day `hour`:`minute` as the person reads a clock: the host's style, or a
    /// 24-hour clock where it names none (`ShownHost::clock`).
    pub(crate) fn clock_text(&self, hour: u32, minute: u32) -> String {
        self.read_person();
        self.shown.borrow().as_ref().map_or_else(
            || format!("{hour:02}:{minute:02}"),
            |host| host.clock(hour, minute),
        )
    }
}

impl Core {
    fn dispatch(&self, imp: Imp, mem: &[u8], a: &Args<'_>) -> HResult<Option<Wasm>> {
        use Imp::Delivery;
        match imp {
            StrLit | StrConcat | StrEq | StrLen | StrWords | IntToStr | StrToInt | StrToFloat
            | FloatToStr | StrCmp | StrFind | StrSlice | StrReplace | StrTrim | StrLower
            | StrUpper | StrStartsWith | StrEndsWith | StrContains | StrSplit | StrJoin
            | StrMatches | StrMisspelled | StrSuggestions | DurationText => {
                self.text_import(imp, mem, a)
            }
            IntMin | IntMax | IntClamp | IntAbs | FloatMin | FloatMax | FloatClamp | FloatAbs
            | FloatRound | FloatRoundTo | FloatFloor | FloatFloorTo | FloatCeil | FloatCeilTo
            | FloatSqrt | FloatCbrt | FloatPow | FloatExp | FloatLn | FloatLog10 | FloatLog2
            | FloatSin | FloatCos | FloatTan | FloatAsin | FloatAcos | FloatAtan | FloatSinh
            | FloatCosh | FloatTanh | FloatGamma | FloatToRadians | FloatToDegrees | FloatSign
            | FloatTrunc | FloatFract | FloatIsFinite | FloatIsNan | ListSumI | ListSumF
            | ListAvg => self.num_import(imp, a),
            ListNew | ListConcat | ListPushI | ListPushF | ListPushH | ListLen | CollCount
            | CollCounted | CollEmpty | ListGetI | ListGetF | ListSortByKey | ListReverse
            | ListGetH | ListEnd | ListIndexOf | ListPrevious | TupleNew | TupleSetI
            | TupleSetF | TupleSetH | TupleGetI | TupleGetF | TupleGetH | SomeH | SomeI | SomeF
            | NoneV | IsSome | UnwrapI | UnwrapF | UnwrapH => self.aggregate_import(imp, a),
            // An argument a call left out before one it gives: it is no value, and the
            // invocation has the default in its place before anything reads it.
            MissingV => self.ret_h(Value::Missing),
            CollWhere => self.ret_h(self.coll_where(a.h(0)?, a.h(1)?)?),
            CollWhereWith => self.ret_h(self.coll_where_with(a.h(0)?, a.h(1)?, a.h(2)?)?),
            CollRelatives => self.ret_h(self.coll_relatives(a)?),
            CollOrdered => self.ret_h(self.coll_ordered(a.h(0)?, a.h(1)?)?),
            Query => self.ret_h(self.query_import(a.h(0)?, a.h(1)?, a.h(2)?, a.h(3)?)?),
            Filters | FilterLit | SortLit => self.catalogue_import(imp, a),
            IdentPart => self.ret_h(self.ident_part(a.h(0)?, a.h(1)?)?),
            Today | TimeDay | TimeDayIn | TimeFromWall | TimeWall | TimeOfDay | TimeClock
            | TimeSpan | DayShift | DayPart | DayWeekday | DayStartOfWeek | DayParse
            | TimeParse | WallClock | DateShow | Occurrences | Zones => self.time_import(imp, a),
            RandomInt | RandomFloat | RandomChoice | RandomShuffle | RandomId => {
                self.random_import(imp, a)
            }
            RichNew | RichPart | RichEmpty | RichAppended | RichTime | RichFromMarkdown
            | RichMarkdown | RichEmbed | RichRelinked | RichMarked => self.rich_import(imp, a),
            EnumNew | EnumSetI | EnumSetF | EnumSetH | EnumIs | EnumGetI | EnumGetF | EnumGetH
            | ValueEq => self.enum_import(imp, a),
            ResGetI | ResGetF | ResGetH | ResSetI | ResSetF | ResSetH | ResAddI | ResAddF
            | ResTextSet | CollInsert | CollInsertAs | CollInsertKeyed | CollRemove | CollMove
            | CollMoveTo | CollMoveAmong | CollPlace | SetAdd | SetRemove | MapPut | MapRemove
            | MapSetAdd | MapSetRemove | MapGetH | MapGetValue | MapKeys | MapKeysValue
            | MapNew | MapPutValue | MapRemoveValue | RecNew | RecSetI | RecSetF | RecSetH
            | RefLit | RefEq | RefIs | RefId | ResourceRef | Earlier => {
                self.resource_import(imp, a)
            }
            ActRef | Invoke | TryInvoke | Suggest | ProposalNote | CallFn | IsError | Outside
            | OutsideAt | Withdraw | ModelCall | Notify | Invoker | MadeFrom | ActorEq
            | ActorName | QueueBlock | Now | TimeShift | PersonNamed | AgentNamed
            | InvocationPush | InvocationPop => self.effect_import(imp, a),
            WebGet | WebPost | WebOk | WebStatus | WebStatusText | WebHeaders | WebBody
            | WebText | BytesLength | Base64Decode => self.web_import(imp, a),
            PageCall => self.page_import(a),
            FilePart | FileSize | FileText | FileFiles | FileBook | FileBookInfo
            | FileBookCover | SheetFile => self.file_import(imp, a),
            FileOfText | FileOfBytes | FileAt | FileZip | SheetOfFile => self.file_make(imp, a),
            FileMarkdown | FileOfDocument => self.document_import(imp, a),
            FileImageInfo | FileCrop | FileTurn | FileResize | FileEncode | FileThumbnail => {
                self.image_import(imp, a)
            }
            PdfPages | PdfOutline | PdfText | PdfLinks | PdfFields | PdfMarkup | PdfImport
            | PdfPicture | PdfSave => self.pdf_import(imp, a),
            CanvasLook | CanvasExport | CanvasGrade | CanvasWand | CanvasSvg | CanvasSvgImport
            | CanvasSave | CanvasOpen => self.canvas_import(imp, a),
            SheetValues | SheetEdit | SheetRanges | SheetNumbers | SheetRecords | SheetEntry => {
                self.sheet_call(imp, a)
            }
            FileList | FileStat | FileReadPath | FileWatch | FileOpenPath | FileOpen
            | FileMkdir | FileWrite | FileRename | FileMove | FileCopy | FileRemove | FileTrash
            | FileTrashEntries | FileRestore | FileCompress | FileExtract | ToolDocumentApps
            | ToolOpenWith | ToolTerminal | ToolContext | ToolContainerShell => {
                self.file_device_import(imp, a)
            }
            ClipboardCopyText | ClipboardCopyImage | ClipboardPasteText | ClipboardPasteImage => {
                self.clipboard_import(imp, a)
            }
            JsonParse | JsonDecode | JsonEncode | StageReading => self.json_import(imp, a),
            FontFamilies => self.font_import(imp, a),
            EngineCall | EngineStart | EngineStream | EngineState | EnginePicture
            | EngineStatus | EngineCancel => self.engine_import(imp, a),
            UiNode | UiPropH | UiPropI | UiPropF | UiPropPic | UiChild | UiBind | UiBindBegin
            | UiOn | UiAnchor | SpaceI | SpaceF | RowOpen | UiBindState | UiBindStateAt
            | StateGet | StateAt | ArgAt | ActSet | ActThen | ActMadeFrom | ActMany | ActOpen
            | ActUndo | ChangeView | UiActions | UiAction | ActChoose | ActChooseFolder
            | ActSave | ActCall | ActRunLine => self.ui_import(imp, a),
            EventValue | EventPart | Actions | Lenses | CallMembers | Invoked | LinesRun
            | Delivery | Reach | MetaOf | Versions | Links | RunChanges | Unseen | FirstUnseen
            | SeenBy | Mentions | Steps | VersionEnd | FieldChanged | Phrase | Word
            | EntryParts => self.platform_import(imp, a),
            Members | MayRead | Speakers | ActorId | ActorKind | ActorRole | ActorApp => {
                self.members_import(imp, a)
            }
            EditField => self.edit_field(a),
            Print | Find | ReadText | OfType | Keep | Grep | Describe | DescribeRef | ReadWith
            | PrintRows | CountBy | GroupBy | Take | Single | Me | ForeignRef | MatchGroup
            | Abort => self.program_import(imp, a),
        }
    }

    /// A sheet's values, edits, ranges, numbers, records or one entry.
    fn sheet_call(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::SheetValues => self.sheet_values(a),
            Imp::SheetEdit => self.sheet_edit(a),
            Imp::SheetRanges => self.sheet_ranges(a),
            Imp::SheetNumbers => self.sheet_numbers(a),
            Imp::SheetRecords => self.sheet_records(a),
            Imp::SheetEntry => self.sheet_entry(a),
            _ => trap("not a sheet import"),
        }
    }
}
