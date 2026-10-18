//! Where a [`SearchIndex`](crate::SearchIndex)'s engine comes from.
//!
//! The engine (tantivy, `tilo-search-tantivy`) is reached only through a
//! [`MakeEngine`]. A build that links it uses it directly. With the
//! `lazy-engine` feature this crate names no engine code, and the engine
//! arrives the first time something searches an index too large to answer
//! from its records: from the loader a program hands over ([`defer_engine`],
//! the browser desktop fetching a module of its own), else, natively, from
//! the runtime's engine library [`ENGINE_LIBRARY`], which the shared app
//! runtime's build turns `lazy-engine` on for (crates/runtime/engines/search).
//! Until then commits wait and searches find nothing; [`engine_ready`] tells
//! a surface to say so, and [`engine_generation`] to search again once the
//! engine is in.

use tilo_search_model::engine::MakeEngine;

/// The engine library the shared native runtime opens on first use.
pub const ENGINE_LIBRARY: &str = "libtilo_search_engine.so";
/// Its entry, a [`tilo_search_model::engine::EngineEntry`].
pub const ENGINE_ENTRY: &str = "tilo_search_engine_entry";

#[cfg(not(feature = "lazy-engine"))]
mod loading {
    use super::MakeEngine;

    /// Linked in: always there.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the lazy-engine build has no engine until it is fetched"
    )]
    pub(crate) fn make() -> Option<MakeEngine> {
        Some(tilo_search_tantivy::tantivy_engine)
    }

    /// Linked in: always there.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the lazy-engine build has no engine until it is fetched"
    )]
    pub(crate) fn installed() -> Option<MakeEngine> {
        Some(tilo_search_tantivy::tantivy_engine)
    }

    /// Whether no search waits for the engine: none does, it is linked in.
    #[must_use]
    pub fn engine_ready() -> bool {
        true
    }

    /// Fetch the engine now, ahead of a search. It is linked in.
    pub fn prepare_engine() {}

    /// How many times the engine arrived: never, as it was always there.
    #[must_use]
    pub fn engine_generation() -> u64 {
        0
    }
}

#[cfg(feature = "lazy-engine")]
mod loading {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use super::MakeEngine;

    static MAKE: OnceLock<MakeEngine> = OnceLock::new();
    static LOAD: OnceLock<fn()> = OnceLock::new();
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    /// The runtime's engine library did not open: nothing will come.
    static UNAVAILABLE: AtomicBool = AtomicBool::new(false);
    static GENERATION: AtomicU64 = AtomicU64::new(0);

    /// The engine, or `None` while its code is still to come (and then
    /// fetched). A loader that brings it at once (the native engine
    /// library) has it ready for this very call.
    pub(crate) fn make() -> Option<MakeEngine> {
        if let Some(make) = MAKE.get() {
            return Some(*make);
        }
        prepare_engine();
        MAKE.get().copied()
    }

    /// The engine if its code is in, without fetching it.
    pub(crate) fn installed() -> Option<MakeEngine> {
        MAKE.get().copied()
    }

    /// Whether no search waits for the engine: its code is in, nothing
    /// asked for it, or it cannot come. While it is on its way, searches of
    /// indexes too large to answer from their records find nothing and their
    /// commits wait.
    #[must_use]
    pub fn engine_ready() -> bool {
        MAKE.get().is_some()
            || !REQUESTED.load(Ordering::Acquire)
            || UNAVAILABLE.load(Ordering::Acquire)
    }

    /// Fetch the engine now, ahead of a search: a surface about to search
    /// (the palette opening) calls it so that results come with the first
    /// keystroke. Fetches once; again after [`engine_load_failed`].
    pub fn prepare_engine() {
        if MAKE.get().is_some() || REQUESTED.swap(true, Ordering::AcqRel) {
            return;
        }
        match LOAD.get() {
            Some(load) => load(),
            None => load_library(),
        }
    }

    /// Bumped when the engine arrives: a surface that searched before
    /// searches again once this moved.
    #[must_use]
    pub fn engine_generation() -> u64 {
        GENERATION.load(Ordering::Acquire)
    }

    /// How the program fetches the engine's code: `load` starts the fetch
    /// and, once the code is in, calls [`install_engine`] (or
    /// [`engine_load_failed`]). Without one, a native program opens the
    /// runtime's engine library ([`super::ENGINE_LIBRARY`]).
    pub fn defer_engine(load: fn()) {
        let _ = LOAD.set(load);
    }

    /// The engine's code is in: indexes commit and search from now on.
    pub fn install_engine(make: MakeEngine) {
        if MAKE.set(make).is_ok() {
            GENERATION.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The fetch failed: the next search fetches again.
    pub fn engine_load_failed() {
        REQUESTED.store(false, Ordering::Release);
    }

    /// Open the runtime's engine library and install its engine, now: the
    /// search that asked continues with it. A library that does not open
    /// will not open later in this process: it is reported once, and
    /// searches of large indexes find nothing.
    #[cfg(not(target_family = "wasm"))]
    fn load_library() {
        // SAFETY: the engine's entry is checked against `EngineEntry` where
        // it is defined (crates/runtime/engines/search), in the same build.
        let entry = unsafe {
            tilo_engine_loader::entry::<tilo_search_model::engine::EngineEntry>(
                super::ENGINE_LIBRARY,
                super::ENGINE_ENTRY,
            )
        };
        match entry {
            Ok(entry) => install_engine(entry()),
            Err(error) => {
                tracing::error!(%error, "search cannot index large lists without its engine");
                UNAVAILABLE.store(true, Ordering::Release);
            }
        }
    }

    /// No engine library in the browser: without a loader nothing comes.
    #[cfg(target_family = "wasm")]
    fn load_library() {
        engine_load_failed();
    }
}

#[cfg(feature = "lazy-engine")]
pub use loading::{defer_engine, engine_load_failed, install_engine};
pub use loading::{engine_generation, engine_ready, prepare_engine};
pub(crate) use loading::{installed, make};
