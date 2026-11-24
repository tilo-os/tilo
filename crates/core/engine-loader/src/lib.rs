//! Engine libraries the shared runtime opens the first time something uses
//! them: code beside `libruntime_dylib.so` that only some apps need.
//!
//! Engines the toolkit reaches on every app's behalf — searching a large
//! index (tantivy), highlighting code (the syntax grammars), compiling
//! ad-block filter lists — are opened here rather than named by the app, so
//! an app that never searches, highlights or blocks never maps them. A
//! library opened here is never closed: what it hands out lives as long as
//! the process. An engine exports one entry function under an unmangled name,
//! whose type the runtime side defines.
//!
//! # An app's engine
//!
//! An engine that answers an app's `engine.call`, `engine.start` and
//! `engine.cancel` exports [`ENTRY`], which answers [`EngineApi`]: the app it
//! answers, the job ops it runs and what each needs, and the four calls. The
//! engine declares what a job op needs ([`Needs`]) so the device decides where
//! it runs: an op that needs nothing of the app's own runs in the device's one
//! engine host, and one that needs a secret, the app's network or what its
//! process keeps runs in the app's own process.
//!
//! What a call needs of the device reaches the engine through [`Device`]: the
//! app's content kept and read back, and the person's clock. The engine holds
//! no authority of its own: every one of those is the call's own, checked by
//! the host as the app's call and not as the library's.

use std::ffi::c_void;
use std::fmt;

/// Why an engine library's entry could not be reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The loader could not open the library (it is missing from the
    /// runtime bundle, or does not link against this runtime).
    Open { library: String, reason: String },
    /// The library has no such entry.
    Missing { library: String, symbol: String },
    /// This target opens no libraries at run time.
    Unsupported,
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open { library, reason } => write!(formatter, "opening {library}: {reason}"),
            Self::Missing { library, symbol } => write!(formatter, "{library} has no {symbol}"),
            Self::Unsupported => formatter.write_str("this target loads no engine libraries"),
        }
    }
}

impl std::error::Error for LoadError {}

/// The entry `symbol` of engine library `library` (its file name, as in the
/// runtime bundle), opening the library the first time.
///
/// # Errors
///
/// [`LoadError`] when the library does not open or lacks the entry, and for
/// a `T` that is not the size of a function pointer.
///
/// # Safety
///
/// `T` must be the type of the function `symbol` names: a `fn` pointer type,
/// spelled from the same definition the engine's entry is checked against,
/// in a library built with this runtime.
pub unsafe fn entry<T: Copy>(library: &str, symbol: &str) -> Result<T, LoadError> {
    let address = imp::open(library, symbol)?;
    if size_of::<T>() != size_of::<*const ()>() {
        return Err(LoadError::Missing {
            library: library.to_owned(),
            symbol: format!("{symbol} as a {}-byte entry", size_of::<T>()),
        });
    }
    // SAFETY: the caller guarantees `T` is the entry's function pointer
    // type, which has the size checked above; `address` is that function.
    Ok(unsafe { std::mem::transmute_copy::<*const (), T>(&address) })
}

/// The entry every app engine exports: the one that answers [`EngineApi`].
pub const ENTRY: &str = "tilo_engine_entry";

/// What a job op needs beyond the engine's code, as its engine declares it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    /// A secret the vault released to the app.
    pub secret: bool,
    /// The app's network, under its own grant alone.
    pub network: bool,
    /// What the app's own process keeps: the app's content store, or answers
    /// its calls read back.
    pub app_state: bool,
}

impl Needs {
    /// Nothing beyond the engine's code: work the device's shared engine host
    /// runs.
    pub const NOTHING: Self = Self {
        secret: false,
        network: false,
        app_state: false,
    };
    /// A released secret.
    pub const SECRET: Self = Self {
        secret: true,
        network: false,
        app_state: false,
    };
    /// The app's network.
    pub const NETWORK: Self = Self {
        secret: false,
        network: true,
        app_state: false,
    };
    /// What the app's own process keeps.
    pub const APP_STATE: Self = Self {
        secret: false,
        network: false,
        app_state: true,
    };

    /// Where work with these needs runs.
    #[must_use]
    pub const fn place(self) -> Place {
        if self.secret || self.network || self.app_state {
            Place::App
        } else {
            Place::Shared
        }
    }
}

/// Where a job runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// The device's one engine host, which the daemon owns and every app's
    /// jobs share.
    Shared,
    /// The app's own sandboxed process.
    App,
}

/// One job op an engine runs, as it declares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobOp {
    pub op: &'static str,
    pub needs: Needs,
}

impl JobOp {
    /// An op that needs `needs`.
    #[must_use]
    pub const fn new(op: &'static str, needs: Needs) -> Self {
        Self { op, needs }
    }
}

/// Where a job tells each move it makes: its state's word and its own
/// fields, as JSON.
#[derive(Clone, Copy)]
pub struct JobSink {
    context: *mut c_void,
    tell: fn(*mut c_void, &str, &str),
}

// SAFETY: the host that starts a job keeps what its sink names for as long as the job runs, so
// a move told from the engine's own thread reads a context that is still there.
unsafe impl Send for JobSink {}
// SAFETY: as [`Send`].
unsafe impl Sync for JobSink {}

impl JobSink {
    /// A sink telling each move to `tell`, with `context`.
    #[must_use]
    pub fn new(context: *mut c_void, tell: fn(*mut c_void, &str, &str)) -> Self {
        Self { context, tell }
    }

    /// The job now stands at the state `state` names, with the engine's own
    /// fields `detail` (a JSON object).
    pub fn moved(&self, state: &str, detail: &str) {
        (self.tell)(self.context, state, detail);
    }
}

/// A file kept in the app's content store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// The hash its content is kept under.
    pub hash: String,
    /// What it is called, extension included.
    pub name: String,
    /// Its media type (`image/png`).
    pub media: String,
    /// How many bytes its content is.
    pub size: i64,
}

impl File {
    /// Where the file's content stands: `tilo-blob://<hash>`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("tilo-blob://{}", self.hash)
    }
}

/// The frames of a stream, handed to the window that shows them as they
/// arrive: the job it belongs to, which frame of it, its size, and its pixels
/// as RGBA.
#[derive(Clone, Copy)]
pub struct Frames {
    context: *mut c_void,
    frame: fn(*mut c_void, &str, u64, u32, u32, &[u8]),
}

// SAFETY: the host that hands a stream its frames keeps what they name for the process's life,
// so a frame told from the stream's own thread reads a context that is still there.
unsafe impl Send for Frames {}
// SAFETY: as [`Send`].
unsafe impl Sync for Frames {}

impl Frames {
    /// Frames handed to `frame`, with `context`.
    #[must_use]
    pub fn new(context: *mut c_void, frame: fn(*mut c_void, &str, u64, u32, u32, &[u8])) -> Self {
        Self { context, frame }
    }

    /// One frame of the stream `job`: which frame it is, its size, and its
    /// pixels as RGBA.
    pub fn frame(&self, job: &str, generation: u64, width: u32, height: u32, pixels: &[u8]) {
        (self.frame)(self.context, job, generation, width, height, pixels);
    }
}

/// What a call needs of the device, answered by the host that runs it: every
/// answer is the app's own, under the call's grants.
#[derive(Clone, Copy)]
pub struct Device {
    context: *mut c_void,
    moment: fn(*mut c_void) -> Result<(String, i64), String>,
    keep_content: fn(*mut c_void, &str, &str, &[u8]) -> Result<File, String>,
    read: fn(*mut c_void, &str) -> Result<Vec<u8>, String>,
}

impl Device {
    /// A device answering with `context`'s own.
    #[must_use]
    pub fn new(
        context: *mut c_void,
        moment: fn(*mut c_void) -> Result<(String, i64), String>,
        keep_content: fn(*mut c_void, &str, &str, &[u8]) -> Result<File, String>,
        read: fn(*mut c_void, &str) -> Result<Vec<u8>, String>,
    ) -> Self {
        Self {
            context,
            moment,
            keep_content,
            read,
        }
    }

    /// The person's clock: the date they read as today, `YYYY-MM-DD`, and the
    /// instant now, in seconds since the epoch.
    ///
    /// # Errors
    ///
    /// The device has no clock to read.
    pub fn moment(&self) -> Result<(String, i64), String> {
        (self.moment)(self.context)
    }

    /// `bytes` kept in the app's content store as `name`, `media`'s.
    ///
    /// # Errors
    ///
    /// The content store refused it.
    pub fn keep_content(&self, name: &str, media: &str, bytes: &[u8]) -> Result<File, String> {
        (self.keep_content)(self.context, name, media, bytes)
    }

    /// The bytes of the content `url` names.
    ///
    /// # Errors
    ///
    /// The content store has no such content, or refused the read.
    pub fn read(&self, url: &str) -> Result<Vec<u8>, String> {
        (self.read)(self.context, url)
    }
}

/// One app's engine, as its library exports it.
#[derive(Clone, Copy)]
pub struct EngineApi {
    /// The app this engine answers, and no other's.
    pub app: &'static str,
    /// The ops this engine runs as jobs, each with what it needs.
    pub jobs: fn() -> &'static [JobOp],
    /// One request answered now: JSON, or the words of a refusal.
    pub call: fn(&Device, &str, &str) -> Result<String, String>,
    /// One request started as the job `job`, which tells each move to `moved`.
    pub start: fn(&Device, &str, &str, &str, &JobSink) -> Result<(), String>,
    /// How far the job `job` has got, as JSON: a job the device records needs
    /// nothing here, so a host that keeps its jobs' records may leave this be.
    pub status: fn(&Device, &str) -> Result<String, String>,
    /// One stream started, whose frames reach `frames` as they arrive: the app
    /// shows it by `tilo-live://<job>`.
    pub stream: fn(&Device, &str, &str, &Frames) -> Result<String, String>,
    /// Stop the job `job`: false when it is not this engine's or had finished.
    pub cancel: fn(&Device, &str) -> bool,
}

/// The engine library `library` (its file name, as in the runtime bundle)
/// answers, opening the library the first time.
///
/// # Errors
///
/// [`LoadError`] when the library does not open, has no [`ENTRY`], or names no
/// engine this build reads.
///
/// # Safety
///
/// The library must be an engine built with this runtime: one that exports
/// [`ENTRY`] answering an [`EngineApi`] whose calls share this build's types.
pub unsafe fn engine(library: &str) -> Result<EngineApi, LoadError> {
    // SAFETY: the caller guarantees the library is an engine of this runtime,
    // and its entry answers its `EngineApi`.
    let entry: fn() -> &'static EngineApi = unsafe { entry::<fn() -> &'static EngineApi>(library, ENTRY)? };
    Ok(*entry())
}

#[cfg(unix)]
mod imp {
    use std::ffi::{CStr, CString, c_void};
    use std::sync::{Mutex, PoisonError};

    use super::LoadError;

    /// An open library. Never closed: the process uses what it handed out.
    struct Handle(*mut c_void);

    // SAFETY: a `dlopen` handle may be used from any thread.
    unsafe impl Send for Handle {}

    static OPEN: Mutex<Vec<(String, Handle)>> = Mutex::new(Vec::new());

    pub(super) fn open(library: &str, symbol: &str) -> Result<*const (), LoadError> {
        let symbol_name = CString::new(symbol).map_err(|_| LoadError::Missing {
            library: library.to_owned(),
            symbol: symbol.to_owned(),
        })?;
        let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
        let handle = if let Some((_, handle)) = open.iter().find(|(name, _)| name == library) {
            handle.0
        } else {
            let name = CString::new(library).map_err(|_| LoadError::Open {
                library: library.to_owned(),
                reason: "the name holds a nul".to_owned(),
            })?;
            // SAFETY: `name` is a C string. Local symbols: the engine's
            // own crates bind inside it (it links with -Bsymbolic), and
            // nothing later resolves against them by accident.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                return Err(LoadError::Open {
                    library: library.to_owned(),
                    reason: last_error(),
                });
            }
            open.push((library.to_owned(), Handle(handle)));
            handle
        };
        drop(open);
        // SAFETY: `handle` is an open library (never closed) and
        // `symbol_name` a C string.
        let address = unsafe { libc::dlsym(handle, symbol_name.as_ptr()) };
        if address.is_null() {
            return Err(LoadError::Missing {
                library: library.to_owned(),
                symbol: symbol.to_owned(),
            });
        }
        Ok(address.cast_const().cast::<()>())
    }

    fn last_error() -> String {
        // SAFETY: dlerror returns null or a C string valid until the next
        // dl* call on this thread, copied out at once.
        let error = unsafe { libc::dlerror() };
        if error.is_null() {
            return "unknown error".to_owned();
        }
        // SAFETY: non-null, nul-terminated, see above.
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(not(unix))]
mod imp {
    use super::LoadError;

    pub(super) fn open(_library: &str, _symbol: &str) -> Result<*const (), LoadError> {
        Err(LoadError::Unsupported)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_missing_library_or_entry_is_an_error_not_a_crash() {
        // SAFETY: nothing is called through the result.
        let missing = unsafe { entry::<fn()>("libtilo_no_such_engine.so", "tilo_no_such_entry") };
        assert!(
            matches!(missing, Err(LoadError::Open { ref library, .. }) if library == "libtilo_no_such_engine.so"),
            "{missing:?}"
        );
        // SAFETY: as above.
        let absent = unsafe { entry::<fn()>("libc.so.6", "tilo_no_such_entry") };
        assert!(
            matches!(absent, Err(LoadError::Missing { .. })),
            "{absent:?}"
        );
        // A real function of it is found, opened once however often asked.
        for _ in 0..2 {
            // SAFETY: `getpid` has this C signature.
            let getpid =
                unsafe { entry::<unsafe extern "C" fn() -> libc::pid_t>("libc.so.6", "getpid") }
                    .expect("libc has getpid");
            // SAFETY: getpid has no preconditions.
            assert_eq!(
                unsafe { getpid() },
                i32::try_from(std::process::id()).unwrap()
            );
        }
    }

    #[test]
    fn an_apps_engine_that_is_not_shipped_says_so_and_crashes_nothing() {
        // SAFETY: nothing is called through the result.
        let missing = unsafe { engine("libtilo_no_such_app_engine.so") };
        assert!(
            matches!(&missing, Err(LoadError::Open { library, .. }) if library == "libtilo_no_such_app_engine.so"),
            "{:?}",
            missing.err()
        );
        // SAFETY: as above; the library opens but names no engine.
        let no_entry = unsafe { engine("libc.so.6") };
        assert!(
            matches!(&no_entry, Err(LoadError::Missing { symbol, .. }) if symbol == ENTRY),
            "{:?}",
            no_entry.err()
        );
    }
}
