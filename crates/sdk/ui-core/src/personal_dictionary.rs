//! Current authenticated host's shared local vocabulary. No filesystem or
//! app-local fallback; missing/revoked channels clear personal hints.
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
/// Account replacement revokes this reader; create another for the new identity.
pub fn reader() -> Arc<dyn Vocabulary> {
    Arc::new(Shared {
        owner: crate::local_identity::bound().map(|identity| identity.group_public_key),
        generation: crate::local_identity::generation(),
    })
}

/// # Errors
/// No authenticated current channel, revoked identity or unavailable host.
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

/// Submit an explicit user edit through the app's existing settings capability.
/// The host validates current identity, app authority and revision again.
///
/// # Errors
/// A stale/revoked/unavailable channel or an invalid edit.
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

/// # Errors
/// Builds without an authenticated host transport have no personal vocabulary.
#[cfg(not(feature = "broker-transport"))]
pub fn snapshot() -> io::Result<Snapshot> {
    Err(io::Error::new(
        io::ErrorKind::NotConnected,
        "personal dictionary requires an authenticated host transport",
    ))
}

/// # Errors
/// Builds without an authenticated host transport cannot submit personal edits.
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

/// Production settings-channel client paired with a real host source for
/// headless app tests. It supplies no storage or identity of its own.
#[cfg(all(feature = "broker-transport", any(test, feature = "test-support")))]
pub struct TestHost {
    source: Arc<dyn settings_store::SettingsSource>,
    app: String,
    owner: [u8; 32],
    revision: u64,
    channel: Arc<crate::collaboration::channel_settings::ChannelSettings>,
    previous: Option<Arc<crate::collaboration::channel_settings::ChannelSettings>>,
}

#[cfg(all(feature = "broker-transport", any(test, feature = "test-support")))]
impl TestHost {
    /// Attach an authenticated client to the host's existing dictionary.
    ///
    /// # Errors
    /// The bound local identity differs from the certified test owner.
    pub fn new(
        source: Arc<dyn settings_store::SettingsSource>,
        app: &str,
        owner: [u8; 32],
    ) -> io::Result<Self> {
        if crate::local_identity::bound().map(|identity| identity.group_public_key) != Some(owner) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "test dictionary owner is not bound",
            ));
        }
        let channel = Arc::new(crate::collaboration::channel_settings::ChannelSettings::new(app));
        channel.linked();
        let previous =
            crate::collaboration::channel_settings::replace_for_test(Some(channel.clone()));
        let host = Self {
            source,
            app: app.to_owned(),
            owner,
            revision: 1,
            channel,
            previous,
        };
        host.channel.received(1, host.view());
        Ok(host)
    }

    fn view(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut view = settings_store::app_scope::app_view(&self.source.load(), &self.app);
        settings_store::personal_dictionary::host::project(
            self.source.as_ref(),
            &self.app,
            Some(self.owner),
            &mut view,
        );
        view
    }

    /// Run queued typed edits through production host permission and persistence
    /// checks and deliver the authoritative answer back to the mounted app.
    ///
    /// # Errors
    /// A revoked owner, stale revision or failed host persistence.
    pub fn flush(&mut self) -> io::Result<()> {
        for (section, values, action) in self.channel.take_outbox() {
            let result = settings_store::personal_dictionary::host::command(
                self.source.as_ref(),
                &self.app,
                Some(self.owner),
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
