//! The engines this process opens from its runtime bundle, the first time an app asks one:
//! code too heavy for the process every app shares, beside `libruntime_dylib.so` rather than
//! linked into this binary.
//!
//! The app asks by name and this process files the answer under the app's name, so one app can
//! never be answered by another's engine. A library is opened through [`tilo_engine_loader`] and
//! stays open for the life of the process; the first ask is where it costs anything, and an app
//! that never asks never maps it.
//!
//! What a call needs of this device reaches the engine through [`Device`], and every answer is
//! the call's own: the app's content store this process holds, under the same grants its own
//! calls run with, and the person's clock. The engine holds no authority of its own.
//!
//! The engines this device ships are the ones whose code an app asks for and no other app maps:
//! Typst's typesetting, and the ones that follow it.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use tilo_engine_loader::{Device, EngineApi, File};
use tilolang_tilo::engine::{JobSink, State};
use tilolang_tilo::instance::Platform;

/// The app whose typesetting `libtilo_typst_engine.so` answers.
pub const TYPST_APP: &str = "so.tilo.typst-editor";

/// The app whose document checks `libtilo_chip_engine.so` answers.
pub const CHIP_DESIGN_APP: &str = "so.tilo.chip-design";

/// The app whose text merges `libtilo_diffmerge_engine.so` answers.
pub const DIFFMERGE_APP: &str = "so.tilo.diffmerge";

/// The app whose pictures, codes, pages and clips `libtilo_camera_engine.so` answers.
pub const CAMERA_APP: &str = "so.tilo.camera";

/// The app whose repositories `libtilo_git_engine.so` answers.
pub const GIT_APP: &str = "so.tilo.git-client";

/// The app whose downloads `libtilo_browser_engine.so` fetches.
pub const BROWSER_APP: &str = "so.tilo.browser";

/// The app whose Grayjay link `libtilo_grayjay_engine.so` answers.
pub const GRAYJAY_APP: &str = "so.tilo.tube";

/// The app whose setup links, pictures and keys `libtilo_totp_engine.so` answers.
pub const TOTP_APP: &str = "so.tilo.totp";

/// The library that answers `app`, when this device ships an engine for it.
fn library_of(app: &str) -> Option<&'static str> {
    match app {
        TYPST_APP => Some("libtilo_typst_engine.so"),
        CHIP_DESIGN_APP => Some("libtilo_chip_engine.so"),
        DIFFMERGE_APP => Some("libtilo_diffmerge_engine.so"),
        CAMERA_APP => Some("libtilo_camera_engine.so"),
        GIT_APP => Some("libtilo_git_engine.so"),
        GRAYJAY_APP => Some("libtilo_grayjay_engine.so"),
        BROWSER_APP => Some("libtilo_browser_engine.so"),
        TOTP_APP => Some("libtilo_totp_engine.so"),
        _ => None,
    }
}

/// Whether `app` is answered by a library this device loads, rather than by an engine linked
/// into this process.
#[must_use]
pub fn answers(app: &str) -> bool {
    library_of(app).is_some() || given(app).is_some()
}

/// The engines a test hands this process in place of a library, by the app each answers.
#[cfg(any(test, feature = "test-support"))]
fn given_engines() -> MutexGuard<'static, BTreeMap<String, EngineApi>> {
    static GIVEN: OnceLock<Mutex<BTreeMap<String, EngineApi>>> = OnceLock::new();
    GIVEN
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(any(test, feature = "test-support"))]
fn given(app: &str) -> Option<EngineApi> {
    given_engines().get(app).copied()
}

#[cfg(not(any(test, feature = "test-support")))]
fn given(_app: &str) -> Option<EngineApi> {
    None
}

/// Answer `engine.app`'s engine with `engine`, as though this device loaded it from a library:
/// what a test reaches the loader's own path with, with no library built.
#[cfg(any(test, feature = "test-support"))]
pub fn give(engine: EngineApi) {
    given_engines().insert(engine.app.to_owned(), engine);
}

/// The engines opened so far, by the library's file name: an engine that could not be opened is
/// kept as its refusal, so a missing library is reported once and never reopened.
fn opened() -> &'static Mutex<BTreeMap<&'static str, Result<EngineApi, String>>> {
    static OPENED: OnceLock<Mutex<BTreeMap<&'static str, Result<EngineApi, String>>>> =
        OnceLock::new();
    OPENED.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn held() -> MutexGuard<'static, BTreeMap<&'static str, Result<EngineApi, String>>> {
    opened()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The engine answering `app`, opening its library the first time and never closing it.
fn engine_of(app: &str) -> Result<EngineApi, String> {
    if let Some(engine) = given(app) {
        return Ok(engine);
    }
    let library =
        library_of(app).ok_or_else(|| format!("no engine of this device's runs for {app}"))?;
    let mut opened = held();
    if let Some(engine) = opened.get(library) {
        return engine.clone();
    }
    // SAFETY: the library is one this build ships, built with this runtime: an engine exporting
    // the one entry whose `EngineApi` shares this build's types.
    let engine = unsafe { tilo_engine_loader::engine(library) }
        .map_err(|why| format!("the engine of {app} did not open: {why}"));
    opened.insert(library, engine.clone());
    engine
}

/// One `engine.call` for `app`, answered by the library this device loads for it.
///
/// # Errors
///
/// The engine's refusal, or that this device loads none for `app`.
pub fn call(platform: &dyn Platform, app: &str, op: &str, input: &str) -> Result<String, String> {
    let engine = engine_of(app)?;
    let context = Context(platform);
    let device = device(&context);
    (engine.call)(&device, op, input)
}

/// One `engine.start` for `app`: the job `op` started as `job`, each of its moves told to
/// `moved`. The job keeps what it makes in `store`, the app's content store, for as long as it
/// runs.
///
/// # Errors
///
/// The engine's refusal, or that this device loads none for `app`.
pub fn start(
    store: &tilo_content::ContentStore,
    app: &str,
    job: &str,
    op: &str,
    input: &str,
    moved: JobSink,
) -> Result<(), String> {
    let engine = engine_of(app)?;
    let held = {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    };
    running().insert(
        held,
        Arc::new(Running {
            store: store.clone(),
            moved,
        }),
    );
    jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(job.to_owned(), app.to_owned());
    // The context is the job's number, not a pointer: a callback after the job ended finds no
    // job rather than freed memory.
    let context = std::ptr::without_provenance_mut::<c_void>(held);
    let device = Device::new(context, job_moment, job_keep_content, job_read);
    let sink = tilo_engine_loader::JobSink::new(context, tell);
    (engine.start)(&device, job, op, input, &sink).inspect_err(|_| {
        running().remove(&held);
        jobs()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(job);
    })
}

/// A job a loaded engine runs, as its device and sink answer it until it ends.
struct Running {
    store: tilo_content::ContentStore,
    moved: JobSink,
}

/// The jobs loaded engines run, by the number their callbacks name them with.
fn running() -> MutexGuard<'static, BTreeMap<usize, Arc<Running>>> {
    static RUNNING: OnceLock<Mutex<BTreeMap<usize, Arc<Running>>>> = OnceLock::new();
    RUNNING
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The job a callback names, while it runs. A job's thread may still be finishing when the
/// person stops it: what it then keeps is refused, and what it tells is past the move that ended
/// it.
fn running_job(context: *mut c_void) -> Result<Arc<Running>, String> {
    running()
        .get(&context.addr())
        .cloned()
        .ok_or_else(|| "the job ended".to_owned())
}

/// A move a loaded engine told: its state's word and its fields, onto the job's sink. Its last
/// move lets go of the job.
fn tell(context: *mut c_void, state: &str, detail: &str) {
    let Ok(job) = running_job(context) else {
        return;
    };
    let state = State::from_word(state)
        .unwrap_or_else(|| panic!("a loaded engine told a job's move as {state:?}, no state"));
    let detail = serde_json::from_str(detail)
        .unwrap_or_else(|error| panic!("a loaded engine told a job's fields not as JSON: {error}"));
    if state.ended() {
        running().remove(&context.addr());
    }
    job.moved.moved(state, detail);
}

fn job_moment(_context: *mut c_void) -> Result<(String, i64), String> {
    Ok(said_moment(&tilolang_tilo::instance::wall_moment()))
}

fn job_keep_content(
    context: *mut c_void,
    name: &str,
    media: &str,
    bytes: &[u8],
) -> Result<File, String> {
    let file = crate::files::keep(&running_job(context)?.store, name, media, bytes)?;
    Ok(File {
        hash: file.hash,
        name: file.name,
        media: file.media,
        size: file.size,
    })
}

fn job_read(context: *mut c_void, url: &str) -> Result<Vec<u8>, String> {
    let hash = url
        .strip_prefix("tilo-blob://")
        .ok_or_else(|| format!("`{url}` is no content address"))?;
    crate::files::read(&running_job(context)?.store, hash)
        .ok_or_else(|| format!("the content store does not hold `{hash}`"))
}

/// One `engine.stream` for `app`: the stream `op` started, answered with its job's id, whose
/// frames reach `frames` as they arrive.
///
/// # Errors
///
/// The engine's refusal, or that this device loads none for `app`.
pub fn stream(
    platform: &dyn Platform,
    app: &str,
    op: &str,
    input: &str,
    frames: &tilo_engine_loader::Frames,
) -> Result<String, String> {
    let engine = engine_of(app)?;
    let context = Context(platform);
    let device = device(&context);
    let answer = (engine.stream)(&device, op, input, frames)?;
    // A stream is a job, answered as `{job}`: the app stops it by the id it read, so the engine
    // that runs it is found again for `status` and `cancel`. An answer that names no job cannot
    // be stopped, so it is said rather than read as a job that never started.
    match serde_json::from_str::<serde_json::Value>(&answer) {
        Ok(answer) => {
            if let Some(job) = answer.get("job").and_then(|job| job.as_str()) {
                jobs()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(job.to_owned(), app.to_owned());
            }
        }
        Err(error) => {
            tracing::warn!(%error, %app, %op, "a stream's answer names no job to stop");
        }
    }
    Ok(answer)
}

/// The jobs this process started in a loaded engine, by the job's id: the app whose engine runs
/// each.
fn jobs() -> &'static Mutex<BTreeMap<String, String>> {
    static JOBS: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// How far a job a loaded engine runs has got, as JSON: `None` when no loaded engine started it.
///
/// # Errors
///
/// The engine's refusal.
pub fn status(platform: &dyn Platform, job: &str) -> Option<Result<String, String>> {
    let app = jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(job)
        .cloned()?;
    let Ok(engine) = engine_of(&app) else {
        return Some(Err(format!("no engine of this device's runs for {app}")));
    };
    let context = Context(platform);
    let device = device(&context);
    Some((engine.status)(&device, job))
}

/// Stop the job `job` a loaded engine runs: `None` when no loaded engine started it.
#[must_use]
pub fn cancel(platform: &dyn Platform, job: &str) -> Option<bool> {
    let app = jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(job)
        .cloned()?;
    let Ok(engine) = engine_of(&app) else {
        return Some(false);
    };
    let context = Context(platform);
    let device = device(&context);
    Some((engine.cancel)(&device, job))
}

/// The device a call runs as: the platform the app's own call runs on, as the engine's
/// callbacks read it.
struct Context<'a>(&'a dyn Platform);

/// The callbacks one engine call reads its device through, for as long as the call runs.
fn device(context: &Context<'_>) -> Device {
    Device::new(
        std::ptr::from_ref(context).cast_mut().cast::<c_void>(),
        moment,
        keep_content,
        read,
    )
    .redeeming(redeem)
}

/// A request's context a window of this process handed over for a job to start with: the
/// window's own, kept in this process and never in a record.
fn redeem(_context: *mut c_void, handle: &str) -> Option<String> {
    tilo_engine_loader::kept::redeem(handle)
}

/// The `Context` behind a callback's pointer. The pointer names the context for the call that
/// handed it over, which is alive for as long as the engine's call runs.
fn behind<'a>(context: *mut c_void) -> &'a Context<'a> {
    // SAFETY: the pointer is the `Context` the call handed the engine, alive until it returns.
    unsafe { &*context.cast::<Context<'a>>() }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the engine's device names every callback's answer as a Result"
)]
fn moment(context: *mut c_void) -> Result<(String, i64), String> {
    Ok(said_moment(&behind(context).0.moment()))
}

/// A moment as the engine reads it: the date, `YYYY-MM-DD`, and seconds since the epoch.
fn said_moment(moment: &tilolang_tilo::instance::Zoned) -> (String, i64) {
    let date = moment.date();
    (
        format!(
            "{}-{:02}-{:02}",
            date.year(),
            i32::from(date.month()),
            i32::from(date.day())
        ),
        moment.timestamp().as_second(),
    )
}

fn keep_content(
    context: *mut c_void,
    name: &str,
    media: &str,
    bytes: &[u8],
) -> Result<File, String> {
    let file = behind(context)
        .0
        .keep_content(name, media, bytes)
        .map_err(|why| format!("`{name}` could not be kept: {why}"))?;
    Ok(File {
        hash: file.hash,
        name: file.name,
        media: file.media,
        size: file.size,
    })
}

fn read(context: *mut c_void, url: &str) -> Result<Vec<u8>, String> {
    let hash = url
        .strip_prefix("tilo-blob://")
        .ok_or_else(|| format!("`{url}` is no content address"))?;
    behind(context)
        .0
        .read_content(hash)
        .ok_or_else(|| format!("the content store does not hold `{hash}`"))
}
