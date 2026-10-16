// Shared mounted contract, included by each production host's test adapter.
#[test]
fn mounted_metadata_waits_for_durable_ack_and_failed_create_can_retry() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    backend.delay.store(true, Ordering::SeqCst);
    backend.state.lock_recover().fail_next = true;
    app.start_effect(&host_effect(ProviderEffect::AddProvider {
        protocol: "anthropic".into(),
    }))
    .unwrap();
    assert!(config(&mut app).profiles.is_empty());
    assert!(backend.state.lock_recover().config.profiles.is_empty());
    assert_private(&mut app);
    backend.delay.store(false, Ordering::SeqCst);
    app.settle();
    assert!(config(&mut app).profiles.is_empty());
    run(
        &mut app,
        ProviderEffect::AddProvider {
            protocol: "anthropic".into(),
        },
    );
    assert_eq!(config(&mut app).profiles.len(), 1);
    assert_private(&mut app);
}

#[test]
fn mounted_administration_is_scoped_to_each_host_not_an_ambient_default() {
    let first_backend = Arc::new(FakeAdminBackend::default());
    let second_backend = Arc::new(FakeAdminBackend::default());
    let mut first = mount(first_backend);
    let mut second = mount(second_backend.clone());
    run(
        &mut first,
        ProviderEffect::AddProvider {
            protocol: "anthropic".into(),
        },
    );
    assert!(config(&mut second).profiles.is_empty());
    run(
        &mut second,
        ProviderEffect::AddProvider {
            protocol: "openai-compatible".into(),
        },
    );
    assert_ne!(
        config(&mut first).profiles[0].protocol,
        config(&mut second).profiles[0].protocol
    );
    let id = config(&mut first).profiles[0].id.clone();
    // Explicit key arguments use the same secret-marked action door as masked input.
    invoke(
        &mut first,
        ProviderEffect::SaveProviderKey {
            provider: id,
            key: Some("private-key-draft".into()),
        },
    )
    .unwrap();
    assert!(second_backend.state.lock_recover().keys.is_empty());
    assert_private(&mut first);
    assert_private(&mut second);
}

#[test]
fn mounted_provider_administration_create_edit_key_status_default_remove() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    run(
        &mut app,
        ProviderEffect::AddProvider {
            protocol: "anthropic".into(),
        },
    );
    let id = config(&mut app).profiles[0].id.clone();
    run(
        &mut app,
        ProviderEffect::SetProviderDetails {
            provider: id.clone(),
            label: "Hosted model".into(),
            endpoint: "https://api.anthropic.com".into(),
            model: Some("model-a".into()),
        },
    );
    let draft = key_buffer(&mut app, &id);
    *draft.lock_recover() = "private-key-draft".into();
    run(
        &mut app,
        ProviderEffect::SaveProviderKey {
            provider: id.clone(),
            key: None,
        },
    );
    assert!(draft.lock_recover().is_empty());
    with_panel(&mut app, |panel| {
        assert_eq!(panel.keys.get(&id), Some(&Ok(CredentialStatus::Configured)));
    });
    run(
        &mut app,
        ProviderEffect::SetDefaultProvider {
            provider: id.clone(),
        },
    );
    assert_eq!(
        config(&mut app).default_profile.as_deref(),
        Some(id.as_str())
    );
    assert_private(&mut app);
    run(&mut app, ProviderEffect::RemoveProvider { provider: id });
    assert!(config(&mut app).profiles.is_empty());
    assert!(backend.state.lock_recover().keys.is_empty());
}

#[test]
fn mounted_delayed_failure_retains_private_key_and_retries_without_racing_metadata() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    run(
        &mut app,
        ProviderEffect::AddProvider {
            protocol: "anthropic".into(),
        },
    );
    let id = config(&mut app).profiles[0].id.clone();
    let draft = key_buffer(&mut app, &id);
    *draft.lock_recover() = "private-key-draft".into();
    backend.delay.store(true, Ordering::SeqCst);
    backend.state.lock_recover().fail_next = true;
    app.start_effect_on(
        &format!("settings.agent-provider:{id}"),
        &host_effect(ProviderEffect::SaveProviderKey {
            provider: id.clone(),
            key: None,
        }),
    )
    .unwrap();
    assert!(
        invoke(
            &mut app,
            ProviderEffect::SetProviderEnabled {
                provider: id.clone(),
                on: false
            }
        )
        .is_err()
    );
    assert_eq!(*draft.lock_recover(), "private-key-draft");
    assert_private(&mut app);
    backend.delay.store(false, Ordering::SeqCst);
    app.settle();
    assert_eq!(*draft.lock_recover(), "private-key-draft");
    run(
        &mut app,
        ProviderEffect::SaveProviderKey {
            provider: id,
            key: None,
        },
    );
    assert!(draft.lock_recover().is_empty());
    assert_private(&mut app);
}

#[test]
fn mounted_conflict_reloads_authority_and_keeps_details_draft_for_explicit_retry() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    run(
        &mut app,
        ProviderEffect::AddProvider {
            protocol: "anthropic".into(),
        },
    );
    let id = config(&mut app).profiles[0].id.clone();
    let fields = with_panel(&mut app, |panel| {
        panel.field_buffers(&panel.provider_config().profiles[0])
    });
    *fields.label.lock_recover() = "My unsaved draft".into();
    {
        let mut state = backend.state.lock_recover();
        state.config.profiles[0].label = "Changed elsewhere".into();
        state.revision += 1;
    }
    let edit = ProviderEffect::SetProviderDetails {
        provider: id,
        label: "My unsaved draft".into(),
        endpoint: "https://api.anthropic.com".into(),
        model: Some("model-a".into()),
    };
    assert!(invoke(&mut app, edit.clone()).is_err());
    assert_eq!(config(&mut app).profiles[0].label, "Changed elsewhere");
    assert_eq!(*fields.label.lock_recover(), "My unsaved draft");
    with_panel(&mut app, |panel| {
        assert!(panel.notice.as_ref().unwrap().contains("Review your draft"));
    });
    run(&mut app, edit);
    assert_eq!(config(&mut app).profiles[0].label, "My unsaved draft");
    assert_private(&mut app);
}

#[test]
fn mounted_legacy_migration_and_both_removal_controls_use_host_authority() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    run(
        &mut app,
        ProviderEffect::AddProvider {
            protocol: "anthropic".into(),
        },
    );
    let id = config(&mut app).profiles[0].id.clone();
    backend
        .state
        .lock_recover()
        .keys
        .insert(id.clone(), CredentialStatus::MigrationRequired);
    run(&mut app, ProviderEffect::RefreshProviders);
    assert!(
        invoke(
            &mut app,
            ProviderEffect::MigrateProviderKey {
                provider: id.clone(),
                original_endpoint: "https://not-the-original.invalid".into(),
            }
        )
        .is_err()
    );
    run(
        &mut app,
        ProviderEffect::MigrateProviderKey {
            provider: id.clone(),
            original_endpoint: "https://api.anthropic.com".into(),
        },
    );
    run(
        &mut app,
        ProviderEffect::RemoveProviderKey {
            provider: id.clone(),
        },
    );
    assert!(!backend.state.lock_recover().keys.contains_key(&id));
    backend
        .state
        .lock_recover()
        .keys
        .insert(id.clone(), CredentialStatus::MigrationRequired);
    run(
        &mut app,
        ProviderEffect::RemoveLegacyProviderKey {
            provider: id.clone(),
        },
    );
    assert!(!backend.state.lock_recover().keys.contains_key(&id));
}

#[test]
fn mounted_ack_preserves_a_newer_private_key_draft() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    run(
        &mut app,
        ProviderEffect::AddProvider {
            protocol: "anthropic".into(),
        },
    );
    let id = config(&mut app).profiles[0].id.clone();
    let draft = key_buffer(&mut app, &id);
    *draft.lock_recover() = "private-key-draft".into();
    backend.delay.store(true, Ordering::SeqCst);
    app.start_effect_on(
        &format!("settings.agent-provider:{id}"),
        &host_effect(ProviderEffect::SaveProviderKey {
            provider: id,
            key: None,
        }),
    )
    .unwrap();
    *draft.lock_recover() = "new-private-key-draft".into();
    backend.delay.store(false, Ordering::SeqCst);
    app.settle();
    assert_eq!(*draft.lock_recover(), "new-private-key-draft");
    assert_private(&mut app);
    assert!(!format!("{:?}", app.history()).contains("new-private-key-draft"));
}
