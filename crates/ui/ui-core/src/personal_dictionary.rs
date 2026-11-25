//! The person's shared vocabulary, read through the app's settings channel. There is no
//! app-local fallback: a missing or revoked channel clears the hints.
pub use settings_model::personal_dictionary::{Edit, Snapshot, Vocabulary, Word};
use std::io;
#[cfg(feature = "model")]
use std::sync::Arc;

#[cfg(feature = "model")]
struct Shared {
    owner: Option<[u8; 32]>,
    generation: u64,
}
#[cfg(feature = "model")]
impl Shared {
    fn current(&self) -> bool {
        self.owner.is_some()
            && self.owner == crate::local_identity::load().map(|identity| identity.group_public_key)
            && self.generation == crate::local_identity::generation()
    }
}
#[cfg(feature = "model")]
impl Vocabulary for Shared {
    fn snapshot(&self) -> io::Result<Snapshot> {
        if !self.current() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "personal dictionary reader was revoked",
            ));
        }
        let snapshot = snapshot()?;
        if !self.current() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "personal dictionary reader was revoked",
            ));
        }
        Ok(snapshot)
    }
}

/// Reader for input consumers of the current authenticated app channel.
#[cfg(feature = "model")]
pub fn reader() -> Arc<dyn Vocabulary> {
    Arc::new(Shared {
        owner: crate::local_identity::load().map(|identity| identity.group_public_key),
        generation: crate::local_identity::generation(),
    })
}

#[cfg(all(feature = "broker-transport", feature = "model"))]
pub fn snapshot() -> io::Result<Snapshot> {
    crate::collaboration::channel_settings::installed()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "personal dictionary requires an authenticated host",
            )
        })?
        .dictionary_snapshot()
}

/// How many times the shell's records this process reads have changed: what [`snapshot`] copies out of
/// them changes only where this moved, so a reader that shows words compares it instead of taking the
/// snapshot again.
#[must_use]
pub fn revision() -> u64 {
    #[cfg(all(feature = "broker-transport", feature = "model"))]
    {
        crate::collaboration::channel_settings::installed().map_or(0, |records| records.revision())
    }
    #[cfg(not(all(feature = "broker-transport", feature = "model")))]
    {
        0
    }
}

/// Submit an explicit user edit through the app's existing settings capability.
#[cfg(all(feature = "broker-transport", feature = "model"))]
pub fn edit(revision: u64, edit: Edit) -> io::Result<()> {
    crate::collaboration::channel_settings::installed()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "personal dictionary requires an authenticated host",
            )
        })?
        .dictionary_edit(revision, edit)
}

#[cfg(not(all(feature = "broker-transport", feature = "model")))]
pub fn snapshot() -> io::Result<Snapshot> {
    Err(io::Error::new(
        io::ErrorKind::NotConnected,
        "personal dictionary requires an authenticated host transport",
    ))
}

#[cfg(not(all(feature = "broker-transport", feature = "model")))]
pub fn edit(_: u64, _: Edit) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::NotConnected,
        "personal dictionary requires an authenticated host transport",
    ))
}

#[cfg(all(test, feature = "model"))]
mod tests {
    use super::*;

    #[test]
    fn a_consumer_reader_never_transfers_to_a_new_account_or_rebinding() {
        let _guard = tilo_test_env::lock();
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::local_identity::install_source(None);
            }
        }
        let _restore = Restore;
        let identity = |key| {
            Arc::new(crate::local_identity::FixedIdentity(
                crate::local_identity::LocalIdentity {
                    group_public_key: [key; 32],
                    display_name: String::new(),
                    avatar_color: [0; 3],
                    device_public_key: None,
                },
            ))
        };
        crate::local_identity::install_source(Some(identity(1)));
        let old = Shared {
            owner: Some([1; 32]),
            generation: crate::local_identity::generation(),
        };
        assert!(old.current());
        crate::local_identity::install_source(Some(identity(2)));
        assert!(!old.current());
        assert!(old.snapshot().is_err());
        crate::local_identity::install_source(Some(identity(1)));
        assert!(
            !old.current(),
            "returning to the same account cannot revive an in-flight reader"
        );
        assert!(old.snapshot().is_err());
    }
}

/// The app's settings channel served from a real settings source by the session's own
/// server, for headless app tests: the device's settings app holds every setter, as the image
/// grants it.
#[cfg(all(
    feature = "broker-transport",
    feature = "model",
    any(test, feature = "test-support")
))]
pub struct TestSettings {
    server: runtime_collab::settings::host::AppSettingsServer,
    channel: Arc<crate::collaboration::channel_settings::ChannelSettings>,
    previous: Option<Arc<crate::collaboration::channel_settings::ChannelSettings>>,
}

#[cfg(all(
    feature = "broker-transport",
    feature = "model",
    any(test, feature = "test-support")
))]
impl TestSettings {
    /// Serve the app's settings channel from `source`, as the session does.
    pub fn new(source: Arc<dyn crate::settings_source::SettingsSource>, app: &str) -> Self {
        let channel = Arc::new(crate::collaboration::channel_settings::ChannelSettings::new(app));
        channel.linked();
        let previous =
            crate::collaboration::channel_settings::replace_for_test(Some(channel.clone()));
        let grants = if app == tilo_app_id::apps::SETTINGS {
            settings_store::workspace::EVERY_SETTER
                .iter()
                .map(|&grant| grant.to_owned())
                .collect()
        } else {
            Vec::new()
        };
        let server = runtime_collab::settings::host::AppSettingsServer::new(
            app,
            source,
            Arc::new(|| Ok(())),
        )
        .with_grants(grants);
        let (revision, view) = server.view();
        channel.received(revision, view);
        Self {
            server,
            channel,
            previous,
        }
    }

    /// Run queued edits through the session's checks and answer the mounted app with what it holds.
    pub fn flush(&mut self) -> io::Result<()> {
        for (section, values, action) in self.channel.take_outbox() {
            let bytes = serde_json::to_vec(&values).map_err(io::Error::other)?;
            let answer = self
                .server
                .update(&section, &bytes, action.as_deref())
                .map(|_| self.server.view());
            self.channel.answered(answer.clone());
            answer.map_err(io::Error::other)?;
        }
        Ok(())
    }
}

#[cfg(all(
    feature = "broker-transport",
    feature = "model",
    any(test, feature = "test-support")
))]
impl Drop for TestSettings {
    fn drop(&mut self) {
        self.channel.disconnected();
        crate::collaboration::channel_settings::replace_for_test(self.previous.take());
    }
}
