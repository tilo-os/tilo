//! Semantic access to fixed-catalog installer artifacts.
//!
//! This crate owns catalog admission, authenticated transport, resumable cache
//! state, and native verification. Callers receive opaque selections and
//! operation tokens; cache paths, transport manifests, native backends,
//! preparation receipts, and held readers stay private.
//!
//! A new operation saves the caller's opaque preference bytes before it starts
//! authentication or cache work. [`Client::preference`] reports the retained
//! load/save outcome. [`Client::resume`] reuses an authenticated selection and
//! does not write the preference again.
//!
//! ```no_run
//! use tilo_install_artifacts::{Client, Error, PreferenceState, ReleaseSelection, Token};
//!
//! let _: fn(&Client, ReleaseSelection, Vec<u8>) -> Result<Token, Error> = Client::start;
//! let _: fn(&Client, ReleaseSelection) -> Result<Token, Error> = Client::resume;
//! let _: fn(&Client) -> PreferenceState = Client::preference;
//! ```
//!
//! The embedded catalog is authority only when it comes from a genuinely
//! signed distribution. A development build does not add release provenance.
//! The `test-support` feature exposes bounded localhost and filesystem
//! fixtures for development tests; it is absent from normal artifacts and does
//! not change normal client construction.
mod bundle;
mod catalog;
pub mod catalog_data;
mod error;
pub mod format;
pub use catalog::{
    ReleaseSelection, bundled_catalog_json, bundled_selections, catalog_architecture,
};
pub use error::{Error, ErrorKind};
mod cache;
mod download;
mod preference;
mod transport;
pub use download::{
    Availability, Phase, PreferenceCommit, PreferenceDurability, PreferenceFailure,
    PreferenceNamespace, PreferencePublication, PreferenceState, Progress, Status, Token,
    TransferPhase,
};
#[cfg(feature = "test-support")]
pub mod test_support;
#[cfg(all(test, not(feature = "test-support")))]
mod test_support;

enum Engine {
    #[cfg(any(windows, all(target_os = "linux", not(test))))]
    Native(cache::native_coordinator::driver::Worker),
    #[cfg(all(unix, any(test, feature = "test-support")))]
    Fixture(cache::fixture_coordinator::driver::Worker),
}

/// Owns one bounded installer-artifact worker.
///
/// Closing or dropping the client requests shutdown and joins its worker. The
/// client exposes semantic operation state only; it has no raw consumer,
/// backend, cache, or transport accessors.
pub struct Client {
    engine: Engine,
}

/// Reports whether this build has a production cache backend for the host.
///
/// Fixture support does not change this result or make the normal constructor
/// select a fixture.
pub fn availability() -> Availability {
    #[cfg(any(windows, all(target_os = "linux", not(test))))]
    {
        Availability::Available
    }
    #[cfg(not(any(windows, all(target_os = "linux", not(test)))))]
    {
        Availability::UnsupportedHost
    }
}

impl Client {
    /// Opens the current Imager compatibility entrance.
    ///
    /// The client uses the fixed embedded catalog and current-user storage. It
    /// does not accept caller-provided paths, origins, backends, or authority.
    /// This compatibility constructor remains subject to the documented G02
    /// host-integration removal gate.
    pub fn open_legacy_imager() -> Result<Self, Error> {
        #[cfg(any(windows, all(target_os = "linux", not(test))))]
        {
            let worker = cache::native_coordinator::driver::Worker::spawn(refuse_protected_staging)
                .map_err(error::Failure::into_error)?;
            Ok(Self {
                engine: Engine::Native(worker),
            })
        }
        #[cfg(not(any(windows, all(target_os = "linux", not(test)))))]
        {
            Err(Error::unsupported(
                "Persistent internal download cache is not supported on this host.",
            ))
        }
    }
    /// Saves `saved_key`, then starts the authenticated selected release.
    ///
    /// Preference bytes are opaque to this service and are limited to 4096
    /// bytes. A successful return admits the request; observe [`Self::status`]
    /// and [`Self::preference`] for its eventual outcome.
    pub fn start(&self, selection: ReleaseSelection, saved_key: Vec<u8>) -> Result<Token, Error> {
        if saved_key.len() > preference::MAX_PREFERENCE_BYTES {
            return Err(Error::invalid_input(
                "Saved download selection exceeds its size limit.",
            ));
        }
        self.engine.start(selection.release, Some(saved_key))
    }
    /// Starts the selected release without rewriting the saved preference.
    pub fn resume(&self, selection: ReleaseSelection) -> Result<Token, Error> {
        self.engine.start(selection.release, None)
    }
    /// Returns the latest retained preference load or save state.
    pub fn preference(&self) -> PreferenceState {
        self.engine.preference()
    }
    /// Requests that the matching operation pause at a cooperative boundary.
    pub fn pause(&self, token: &Token) -> Result<(), Error> {
        self.engine.pause(token)
    }
    /// Requests cancellation of the matching operation.
    pub fn cancel(&self, token: &Token) -> Result<(), Error> {
        self.engine.cancel(token)
    }
    /// Releases retained verified artifacts for later use without consuming them.
    pub fn release(&self, token: &Token) -> Result<(), Error> {
        self.engine.release(token)
    }
    /// Discards the matching operation's revalidated partial data and starts a fresh token.
    pub fn discard(&self, token: &Token, selection: ReleaseSelection) -> Result<Token, Error> {
        self.engine.discard(token, selection.release)
    }
    /// Returns the most recent coalesced operation status, if any.
    pub fn status(&self) -> Option<Status> {
        self.engine.status()
    }
    /// Shuts down and joins the owned worker. Repeated calls are successful.
    pub fn close(&mut self) -> Result<(), Error> {
        self.engine.close()
    }
    #[cfg(all(unix, any(test, feature = "test-support")))]
    fn fixture(
        root: &std::path::Path,
        preference: preference::PreferenceStore,
        transport: transport::InternalTransport,
        consumptions: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            engine: Engine::Fixture(cache::fixture_coordinator::driver::Worker::fixture(
                root,
                preference,
                transport,
                consumptions,
            )),
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Verifies an already local component directory against portable bundle metadata.
///
/// This compatibility verifier does not grant access, retain a verified reader,
/// stage data, or install anything.
pub fn verify_local_components(
    bundle: &format::InternalBundle,
    path: &std::path::Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), Error> {
    bundle::verify_internal_components(bundle, path, cancel).map_err(Error::failed)
}

#[cfg(all(test, target_os = "linux"))]
mod constructor_tests {
    #[test]
    fn unit_build_normal_constructor_never_implicitly_uses_fixture() {
        assert_eq!(super::availability(), super::Availability::UnsupportedHost);
        assert_eq!(
            super::Client::open_legacy_imager().err().unwrap().kind(),
            super::ErrorKind::Unsupported
        );
    }
}

impl Engine {
    fn start(
        &self,
        release: catalog::InternalRelease,
        saved_key: Option<Vec<u8>>,
    ) -> Result<Token, Error> {
        #[cfg(not(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        )))]
        {
            match *self {}
        }
        #[cfg(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        ))]
        match self {
            #[cfg(any(windows, all(target_os = "linux", not(test))))]
            Self::Native(worker) => match saved_key {
                Some(bytes) => worker.start_with_preference(release, bytes),
                None => worker.start(release),
            }
            .map_err(error::Failure::into_error),
            #[cfg(all(unix, any(test, feature = "test-support")))]
            Self::Fixture(worker) => match saved_key {
                Some(bytes) => worker.start_with_preference(release, bytes),
                None => worker.start(release),
            }
            .map_err(error::Failure::into_error),
        }
    }
    fn pause(&self, token: &Token) -> Result<(), Error> {
        #[cfg(not(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        )))]
        {
            match *self {}
        }
        #[cfg(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        ))]
        match self {
            #[cfg(any(windows, all(target_os = "linux", not(test))))]
            Self::Native(worker) => worker.pause(token).map_err(error::Failure::into_error),
            #[cfg(all(unix, any(test, feature = "test-support")))]
            Self::Fixture(worker) => worker.pause(token).map_err(error::Failure::into_error),
        }
    }
    fn cancel(&self, token: &Token) -> Result<(), Error> {
        #[cfg(not(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        )))]
        {
            match *self {}
        }
        #[cfg(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        ))]
        match self {
            #[cfg(any(windows, all(target_os = "linux", not(test))))]
            Self::Native(worker) => worker.cancel(token).map_err(error::Failure::into_error),
            #[cfg(all(unix, any(test, feature = "test-support")))]
            Self::Fixture(worker) => worker.cancel(token).map_err(error::Failure::into_error),
        }
    }
    fn release(&self, token: &Token) -> Result<(), Error> {
        #[cfg(not(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        )))]
        {
            match *self {}
        }
        #[cfg(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        ))]
        match self {
            #[cfg(any(windows, all(target_os = "linux", not(test))))]
            Self::Native(worker) => worker.release(token).map_err(error::Failure::into_error),
            #[cfg(all(unix, any(test, feature = "test-support")))]
            Self::Fixture(worker) => worker.release(token).map_err(error::Failure::into_error),
        }
    }
    fn discard(&self, token: &Token, release: catalog::InternalRelease) -> Result<Token, Error> {
        #[cfg(not(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        )))]
        {
            match *self {}
        }
        #[cfg(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        ))]
        match self {
            #[cfg(any(windows, all(target_os = "linux", not(test))))]
            Self::Native(worker) => worker
                .discard(token, release)
                .map_err(error::Failure::into_error),
            #[cfg(all(unix, any(test, feature = "test-support")))]
            Self::Fixture(worker) => worker
                .discard(token, release)
                .map_err(error::Failure::into_error),
        }
    }
    fn close(&mut self) -> Result<(), Error> {
        #[cfg(not(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        )))]
        {
            match *self {}
        }
        #[cfg(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        ))]
        match self {
            #[cfg(any(windows, all(target_os = "linux", not(test))))]
            Self::Native(worker) => worker.close().map_err(error::Failure::into_error),
            #[cfg(all(unix, any(test, feature = "test-support")))]
            Self::Fixture(worker) => worker.close().map_err(error::Failure::into_error),
        }
    }
    fn status(&self) -> Option<Status> {
        #[cfg(not(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        )))]
        {
            match *self {}
        }
        #[cfg(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        ))]
        match self {
            #[cfg(any(windows, all(target_os = "linux", not(test))))]
            Self::Native(worker) => worker.status(),
            #[cfg(all(unix, any(test, feature = "test-support")))]
            Self::Fixture(worker) => worker.status(),
        }
    }
    fn preference(&self) -> PreferenceState {
        #[cfg(not(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        )))]
        {
            match *self {}
        }
        #[cfg(any(
            windows,
            all(target_os = "linux", not(test)),
            all(unix, any(test, feature = "test-support"))
        ))]
        match self {
            #[cfg(any(windows, all(target_os = "linux", not(test))))]
            Self::Native(worker) => worker.preference(),
            #[cfg(all(unix, any(test, feature = "test-support")))]
            Self::Fixture(worker) => worker.preference(),
        }
    }
}

#[cfg(all(test, unix))]
mod facade_tests {
    use super::*;
    use crate::test_support::{Server, manifest_bytes, manifest_value, response};

    fn wait(client: &Client, phase: Phase) -> Status {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = client.status() {
                if status.phase == phase {
                    return status;
                }
                assert!(
                    !matches!(status.phase, Phase::Failed | Phase::Cancelled),
                    "{status:?}"
                );
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{:?}",
                client.status()
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn private_consumer_refuses_protected_staging_and_retains_finals() {
        let bytes = manifest_bytes(&manifest_value());
        let server = Server::new(vec![
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
        ]);
        let root = tempfile::tempdir().unwrap();
        let mut client = server.client(root.path(), &root.path().join("selection.json"));
        let token = client
            .start(server.selection(&bytes), b"key".to_vec())
            .unwrap();
        wait(&client, Phase::Ready);
        assert_eq!(server.consumptions(), 0);
        // The public facade intentionally has no consume operation. Exercise
        // the retained private callback to prove that even this path refuses.
        match &client.engine {
            Engine::Fixture(worker) => worker.consume(&token).unwrap(),
        }
        let failed = wait(&client, Phase::Failed);
        assert_eq!(
            failed.error.as_deref(),
            Some("Protected staging is not available; verified download data was retained.")
        );
        assert_eq!(failed.failure_kind, Some(ErrorKind::Failed));
        assert_eq!(failed.finals, [true, true]);
        assert!(failed.residue);
        assert_eq!(server.consumptions(), 1);
        client.close().unwrap();
        assert_eq!(
            std::fs::read(root.path().join("0/final")).unwrap(),
            vec![b'E'; 4096]
        );
        assert_eq!(
            std::fs::read(root.path().join("1/final")).unwrap(),
            vec![b'R'; 8192]
        );
    }
}

fn refuse_protected_staging(
    _: &transport::SelectedManifest,
    _: &download::Cancellation<'_>,
    _: &mut dyn cache::HeldReadSeek,
    _: &mut dyn cache::HeldReadSeek,
) -> Result<(), error::Failure> {
    Err("Protected staging is not available; verified download data was retained.".into())
}
