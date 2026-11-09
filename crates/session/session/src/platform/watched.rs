//! A value the host keeps current for the session's loop: the records an
//! app's workspace indexes name, read away from the thread that shows them.
//!
//! The session never opens, stats or lists a path to find out whether the
//! value is stale: reading a value the host has already read is what the
//! loop does, on every frame.
//!
//! On a Linux desktop the store is a directory of files, and the host's
//! worker reads the value again whenever one of them changes. The page keeps
//! its records in its own store and drives the reads itself.

#[cfg(target_os = "linux")]
pub use tilo_files::native::Watched;

#[cfg(not(target_os = "linux"))]
pub use self::once::Watched;

#[cfg(not(target_os = "linux"))]
mod once {
    use std::path::PathBuf;
    use std::sync::{Arc, PoisonError, RwLock};

    /// A value read once when it is started, and again when the page says its
    /// store changed. See the module docs.
    pub struct Watched<T> {
        read: Box<dyn Fn() -> T + Send + Sync>,
        value: RwLock<Arc<T>>,
    }

    impl<T: Send + Sync + 'static> Watched<T> {
        /// Read `read` now and keep it. `directories` is where a Linux host
        /// would watch: the page's store has none.
        ///
        /// # Errors
        /// Never: the page's store answers where it is asked.
        pub fn start(
            _name: &str,
            _directories: Vec<PathBuf>,
            read: impl Fn() -> T + Send + Sync + 'static,
        ) -> std::io::Result<Self> {
            let value = RwLock::new(Arc::new(read()));
            Ok(Self {
                read: Box::new(read),
                value,
            })
        }

        /// The latest value. Never reads the store.
        #[must_use]
        pub fn get(&self) -> Arc<T> {
            Arc::clone(&self.value.read().unwrap_or_else(PoisonError::into_inner))
        }

        /// Read the value again, now: the page calls it when its store changed.
        pub fn refresh(&self) {
            let fresh = Arc::new((self.read)());
            *self.value.write().unwrap_or_else(PoisonError::into_inner) = fresh;
        }
    }
}
