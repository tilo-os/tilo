// Portable asynchronous backend for mounted host contract tests.
#[derive(Default)]
struct BackendState {
    config: ProviderConfig,
    revision: u64,
    fail_next: bool,
    /// What a provider lists, when not the two stand-in models.
    listing: Option<Vec<agent_provider_config::ModelDescriptor>>,
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

    fn list_models(
        &self,
        _id: String,
    ) -> AdminFuture<'_, Vec<agent_provider_config::ModelDescriptor>> {
        Box::pin(async move {
            self.ready().await?;
            let listing = self.state.lock_recover().listing.clone();
            if let Some(listing) = listing {
                return Ok(listing);
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
