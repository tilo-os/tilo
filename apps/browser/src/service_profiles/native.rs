//! Host root admission for the native website-session backend.
use browser_continuity::service_profiles::{GrantedRoot, Profile as NativeProfile};
use browser_continuity::sessions::SessionCookies;
use std::io;
use std::path::{Component, Path};

/// App adapter; the backend owns the engine lease and complete lifecycle.
pub(crate) struct Profile(NativeProfile);

impl Profile {
    pub(crate) fn prepare(session: &SessionCookies) -> io::Result<Self> {
        let root = crate::storage::data_dir().ok_or_else(unavailable)?;
        Self::prepare_in(&root, session)
    }

    pub(crate) fn prepare_in(root: &Path, session: &SessionCookies) -> io::Result<Self> {
        // Validate before root admission can create any directories.
        session.validate()?;
        NativeProfile::prepare(&GrantedRoot::admit_native(root)?, session).map(Self)
    }

    pub(crate) fn path(&self) -> io::Result<&Path> {
        self.0.path()
    }

    pub(crate) fn needs_import(&self) -> bool {
        self.0.needs_import()
    }

    pub(crate) fn complete(&mut self) -> io::Result<()> {
        self.0.complete()
    }

    pub(crate) fn cancel_after_engine_exit(&mut self) -> io::Result<()> {
        self.0.cancel_after_engine_exit()
    }

    pub(crate) fn begin_forget(&mut self) -> io::Result<()> {
        self.0.begin_forget()
    }

    pub(crate) fn cleanup_after_engine_exit(&mut self) -> io::Result<()> {
        self.0.cleanup_after_engine_exit()
    }

    pub(crate) fn forget_closed(person: [u8; 32], id: &str) -> io::Result<()> {
        let root = crate::storage::data_dir().ok_or_else(unavailable)?;
        if id.is_empty()
            || !matches!(
                Path::new(id).components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(_)]
            )
        {
            return Err(unavailable());
        }
        NativeProfile::forget_closed(&GrantedRoot::admit_native(&root)?, person, id)
    }
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Website session profile is unavailable",
    )
}
