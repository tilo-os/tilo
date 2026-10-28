// Portable asynchronous backend for mounted host contract tests.
#[derive(Default)]
struct BackendState {
    config: ProviderConfig,
    revision: u64,
    keys: HashMap<String, CredentialStatus>,
    fail_next: bool,
}

#[derive(Default)]
struct FakeAdminBackend {
    state: Mutex<BackendState>,
    delay: AtomicBool,
}

impl ProviderAdmin for FakeAdminBackend {
    fn get_config(&self) -> AdminFuture<'_, ProviderSnapshot> {
        Box::pin(async move {
            let state = self.state.lock_recover();
            Ok(ProviderSnapshot {
                config: state.config.clone(),
                revision: state.revision.to_string(),
            })
        })
    }

    fn set_config(
        &self,
        revision: String,
        config: ProviderConfig,
    ) -> AdminFuture<'_, ProviderSnapshot> {
        Box::pin(async move {
            self.ready().await?;
            let mut state = self.state.lock_recover();
            if revision != state.revision.to_string() {
                return Err(ModelError::new(ErrorKind::Conflict));
            }
            // The backend requires credential cleanup before metadata deletion.
            if state
                .keys
                .keys()
                .any(|id| !config.profiles.iter().any(|profile| &profile.id == id))
            {
                return Err(ModelError::new(ErrorKind::PermissionDenied));
            }
            state.config = config;
            state.revision = state
                .revision
                .checked_add(1)
                .expect("test revision overflow");
            Ok(ProviderSnapshot {
                config: state.config.clone(),
                revision: state.revision.to_string(),
            })
        })
    }

    fn credential_status(&self, id: String) -> AdminFuture<'_, CredentialStatus> {
        Box::pin(async move {
            Ok(self
                .state
                .lock_recover()
                .keys
                .get(&id)
                .copied()
                .unwrap_or(CredentialStatus::Missing))
        })
    }

    fn list_models(
        &self,
        id: String,
    ) -> AdminFuture<'_, Vec<agent_provider_config::ModelDescriptor>> {
        Box::pin(async move {
            self.ready().await?;
            // A provider answers only with the key it was given.
            if self.state.lock_recover().keys.get(&id) != Some(&CredentialStatus::Configured) {
                return Err(ModelError::new(ErrorKind::Authentication));
            }
            Ok(vec![
                agent_provider_config::ModelDescriptor {
                    label: "Listed Model A".into(),
                    context_window: Some(200_000),
                    ..agent_provider_config::ModelDescriptor::new("listed-a")
                },
                agent_provider_config::ModelDescriptor::new("listed-b"),
            ])
        })
    }

    fn set_api_key(&self, id: String, key: String) -> AdminFuture<'_, ()> {
        Box::pin(async move {
            self.ready().await?;
            assert_eq!(key, "private-key-draft");
            self.state
                .lock_recover()
                .keys
                .insert(id, CredentialStatus::Configured);
            Ok(())
        })
    }

    fn remove_api_key(&self, id: String) -> AdminFuture<'_, ()> {
        Box::pin(async move {
            self.ready().await?;
            self.state.lock_recover().keys.remove(&id);
            Ok(())
        })
    }

    fn migrate_api_key(&self, id: String, original_endpoint: String) -> AdminFuture<'_, ()> {
        Box::pin(async move {
            self.ready().await?;
            if original_endpoint != "https://api.anthropic.com" {
                return Err(ModelError::new(ErrorKind::InvalidRequest));
            }
            self.state
                .lock_recover()
                .keys
                .insert(id, CredentialStatus::Configured);
            Ok(())
        })
    }

    fn remove_legacy_api_key(&self, id: String) -> AdminFuture<'_, ()> {
        self.remove_api_key(id)
    }
}

impl FakeAdminBackend {
    async fn ready(&self) -> Result<(), ModelError> {
        std::future::poll_fn(|_| {
            if self.delay.load(Ordering::SeqCst) {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(())
            }
        })
        .await;
        if std::mem::take(&mut self.state.lock_recover().fail_next) {
            Err(ModelError::new(ErrorKind::Transport))
        } else {
            Ok(())
        }
    }
}
