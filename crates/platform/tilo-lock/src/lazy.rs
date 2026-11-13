//! Code a program fetches the first time it is used, or links: one
//! declaration ([`lazy_code!`](crate::lazy_code)) serves both builds of a
//! crate, so its callers ask [`LazyCode::get`] either way. The browser
//! desktop splits each such piece of code into a module of its own
//! (`crates/browser/tilo-web-desktop/src/lazy.rs`).

use crate::deferred::Deferred;

pub struct LazyCode<F: Copy + 'static> {
    linked: Option<fn() -> F>,
    fetched: Deferred<F>,
}

impl<F: Copy + 'static> LazyCode<F> {
    #[must_use]
    pub const fn linked(maker: fn() -> F) -> Self {
        Self {
            linked: Some(maker),
            fetched: Deferred::new(),
        }
    }

    #[must_use]
    pub const fn fetched() -> Self {
        Self {
            linked: None,
            fetched: Deferred::new(),
        }
    }

    /// The code, or `None` while it is still to come; a miss starts a fetch.
    pub fn get(&self) -> Option<F> {
        match self.linked {
            Some(maker) => Some(maker()),
            None => self.fetched.get(),
        }
    }

    /// The code once it is in, for work that can wait for it; `None` if the fetch fails.
    pub async fn wait(&self) -> Option<F>
    where
        F: Send + Sync,
    {
        match self.linked {
            Some(maker) => Some(maker()),
            None => self.fetched.wait().await,
        }
    }

    pub fn defer(&self, load: fn()) {
        self.fetched.defer(load);
    }

    /// The code is in: surfaces waiting on it take it up on their owner's next layout.
    pub fn install(&self, code: F) {
        self.fetched.install(code);
    }

    pub fn load_failed(&self) {
        self.fetched.load_failed();
    }

    pub fn generation(&self) -> u64 {
        self.fetched.generation()
    }

    /// Whether a fetch of the code is on its way (never where it is linked).
    pub fn fetching(&self) -> bool {
        self.linked.is_none() && self.fetched.fetching()
    }

    /// Whether the code is in, without fetching it.
    pub fn is_in(&self) -> bool {
        self.linked.is_some() || self.fetched.generation() > 0
    }

    /// Wake `waker` once the code is in (at once where it is linked) or its
    /// fetch failed.
    pub fn wake_on_arrival(&self, waker: std::task::Waker) {
        if self.linked.is_some() {
            waker.wake();
        } else {
            self.fetched.wake_on_arrival(waker);
        }
    }
}

/// Declare the code of a surface that loads on first use where `feature`
/// (a feature of the declaring crate) is on, and is linked otherwise:
///
/// ```ignore
/// tilo_lock::lazy_code! {
///     /// History's code.
///     pub(crate) static HISTORY: MakeHistory = builtin_history, feature = "lazy-history";
/// }
/// ```
///
/// `builtin` is the only path to the surface's code: the module that holds
/// it exports it (`split_code!`), and nothing else may name it.
#[macro_export]
macro_rules! lazy_code {
    ($(#[$meta:meta])* $vis:vis static $name:ident: $ty:ty = $builtin:path, feature = $feature:literal;) => {
        $(#[$meta])*
        #[cfg(not(feature = $feature))]
        $vis static $name: $crate::lazy::LazyCode<$ty> =
            $crate::lazy::LazyCode::linked($builtin);
        $(#[$meta])*
        #[cfg(feature = $feature)]
        $vis static $name: $crate::lazy::LazyCode<$ty> =
            $crate::lazy::LazyCode::fetched();
    };
}
