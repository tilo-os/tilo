//! Engine libraries the shared runtime opens the first time something uses
//! them.
//!
//! An engine library (`crates/runtime/engines/`) is code beside
//! `libruntime_dylib.so` that only some apps need. Most are named
//! `DT_NEEDED` by their apps, so the loader maps and relocates them at exec.
//! Some are reached by the toolkit on behalf of every app, but only once a
//! person does something particular: searching a large index (tantivy),
//! highlighting code (the syntax grammars), compiling ad-block filter lists.
//! No app names those; the toolkit opens them here on first use, so an app
//! that never searches, highlights or blocks never maps them.
//!
//! An engine exports one entry function under an unmangled name, whose type
//! the runtime side defines and the engine crate is checked against at
//! compile time. The library is found the way the loader found the runtime
//! (the runtime bundle's `lib/`, on the sandbox's library path), opened with
//! its symbols local, and never closed: what it hands out lives as long as
//! the process.

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

#[cfg(unix)]
mod imp {
    use std::ffi::{CStr, CString, c_void};
    use std::sync::{Mutex, PoisonError};

    use super::LoadError;

    /// An open library. Never closed: its code and data stay mapped for as
    /// long as anything it handed out may be used, which is the process.
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
        // The C library is open in every process; it has no such entry.
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
}
