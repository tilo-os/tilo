//! The person's shared vocabulary, read through the app's settings channel. There is no
//! app-local fallback: a missing or revoked channel clears the hints.
pub use settings_store::personal_dictionary::{Edit, Snapshot, Vocabulary, Word};
use std::io;
use std::sync::Arc;

struct Shared {
    owner: Option<[u8; 32]>,
    generation: u64,
}
impl Shared {
    fn current(&self) -> bool {
        self.owner.is_some()
            && self.owner
                == crate::local_identity::bound().map(|identity| identity.group_public_key)
            && self.generation == crate::local_identity::generation()
    }
}
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
pub fn reader() -> Arc<dyn Vocabulary> {
    Arc::new(Shared {
        owner: crate::local_identity::bound().map(|identity| identity.group_public_key),
        generation: crate::local_identity::generation(),
    })
}

#[cfg(feature = "broker-transport")]
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
    #[cfg(feature = "broker-transport")]
    {
        crate::collaboration::channel_settings::installed().map_or(0, |records| records.revision())
    }
    #[cfg(not(feature = "broker-transport"))]
    {
        0
    }
}

/// Submit an explicit user edit through the app's existing settings capability.
#[cfg(feature = "broker-transport")]
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

#[cfg(not(feature = "broker-transport"))]
pub fn snapshot() -> io::Result<Snapshot> {
    Err(io::Error::new(
        io::ErrorKind::NotConnected,
        "personal dictionary requires an authenticated host transport",
    ))
}

#[cfg(not(feature = "broker-transport"))]
pub fn edit(_: u64, _: Edit) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::NotConnected,
        "personal dictionary requires an authenticated host transport",
    ))
}

#[cfg(test)]
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

/// The app's settings channel served from a real settings source, for headless app tests.
#[cfg(all(feature = "broker-transport", any(test, feature = "test-support")))]
pub struct TestHost {
    source: Arc<dyn settings_store::SettingsSource>,
    app: String,
    revision: u64,
    channel: Arc<crate::collaboration::channel_settings::ChannelSettings>,
    previous: Option<Arc<crate::collaboration::channel_settings::ChannelSettings>>,
}

#[cfg(all(feature = "broker-transport", any(test, feature = "test-support")))]
impl TestHost {
    /// Serve the app's settings channel from `source`, as the session does.
    pub fn new(source: Arc<dyn settings_store::SettingsSource>, app: &str) -> Self {
        let channel = Arc::new(crate::collaboration::channel_settings::ChannelSettings::new(app));
        channel.linked();
        let previous =
            crate::collaboration::channel_settings::replace_for_test(Some(channel.clone()));
        let host = Self {
            source,
            app: app.to_owned(),
            revision: 1,
            channel,
            previous,
        };
        let view = host.view();
        host.channel.received(1, view);
        host
    }

    fn view(&self) -> serde_json::Map<String, serde_json::Value> {
        let document = self.source.load();
        let mut view = settings_store::app_scope::app_view(&document, &self.app);
        settings_store::personal_dictionary::delivery::project(&document, &self.app, &mut view);
        view
    }

    /// Run queued edits through the session's checks and answer the mounted app with what it holds.
    pub fn flush(&mut self) -> io::Result<()> {
        for (section, values, action) in self.channel.take_outbox() {
            let result = settings_store::personal_dictionary::delivery::command(
                self.source.as_ref(),
                &self.app,
                &section,
                &values,
            )
            .and_then(|handled| {
                if handled {
                    Ok(())
                } else {
                    settings_store::app_scope::update_for_app_by(
                        self.source.as_ref(),
                        &self.app,
                        &section,
                        values,
                        &settings_store::ChangeOrigin {
                            action,
                            principal: None,
                            invoked_by: None,
                        },
                    )
                }
            });
            self.revision = self.revision.saturating_add(1);
            self.channel.answered(
                result
                    .as_ref()
                    .map(|()| (self.revision, self.view()))
                    .map_err(ToString::to_string),
            );
            result?;
        }
        Ok(())
    }
}

#[cfg(all(feature = "broker-transport", any(test, feature = "test-support")))]
impl Drop for TestHost {
    fn drop(&mut self) {
        self.channel.disconnected();
        crate::collaboration::channel_settings::replace_for_test(self.previous.take());
    }
}
