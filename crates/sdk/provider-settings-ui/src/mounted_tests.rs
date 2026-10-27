// Shared mounted contract, included by each production host's test adapter.

fn rendered_enabled(widget: &dyn ui::widget::Widget, id: &str) -> Option<bool> {
    if widget.test_id() == Some(id) {
        return Some(widget.is_enabled());
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| rendered_enabled(child, id))
}

#[test]
fn mounted_rendered_provider_controls_save_private_fields_and_follow_permissions() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    app.harness().resize((1800, 6000)).unwrap();
    app.settle();
    // The known providers are one chooser; typing narrows it.
    app.harness()
        .click_named("agent-models-add-provider")
        .unwrap();
    app.harness().type_text("Anthropic");
    app.harness().key("Enter");
    app.settle();
    assert_eq!(config(&mut app).profiles.len(), 1);
    assert_eq!(config(&mut app).profiles[0].label, "Anthropic");
    assert_eq!(
        config(&mut app).profiles[0].endpoint,
        "https://api.anthropic.com"
    );
    let id = config(&mut app).profiles[0].id.clone();
    let save = format!("agent-models-save-provider-{id}");
    assert_eq!(
        rendered_enabled(app.harness().root_widget(), &save),
        Some(true)
    );
    for (field, value) in [
        ("label", "Rendered provider"),
        ("url", "https://models.example.test/v1"),
        ("model", "rendered-model"),
    ] {
        app.harness()
            .click_named(&format!("agent-models-{field}-{id}"))
            .unwrap();
        app.harness().set_modifiers(&["ctrl".into()]);
        app.harness().key("a");
        app.harness().set_modifiers(&[]);
        app.harness().type_text(value);
    }
    assert_ne!(
        backend.state.lock_recover().config.profiles[0].label,
        "Rendered provider"
    );
    app.harness().click_named(&save).unwrap();
    app.settle();
    let profile = config(&mut app).profiles[0].clone();
    assert_eq!(profile.label, "Rendered provider");
    assert_eq!(profile.endpoint, "https://models.example.test/v1");
    assert_eq!(profile.default_model.as_deref(), Some("rendered-model"));
    assert_eq!(backend.state.lock_recover().config.profiles[0], profile);

    let key = format!("agent-models-set-api-key-{id}");
    app.harness()
        .click_named(&format!("agent-models-api-key-{id}"))
        .unwrap();
    app.harness().type_text("private-key-draft");
    app.harness().click_named(&key).unwrap();
    app.settle();
    assert!(key_buffer(&mut app, &id).lock_recover().is_empty());
    assert_eq!(
        backend.state.lock_recover().keys.get(&id),
        Some(&CredentialStatus::Configured)
    );
    assert_private(&mut app);

    let toggle = format!("agent-models-enabled-{id}");
    let remove = format!("agent-models-remove-provider-{id}");
    let original = config(&mut app);
    for role in [ui::Role::Viewer, ui::Role::Commenter] {
        app.set_role(role);
        app.settle();
        for control in ["agent-models-add-provider", &save, &key, &toggle, &remove] {
            assert_eq!(
                rendered_enabled(app.harness().root_widget(), control),
                Some(false),
                "{control}"
            );
            app.harness().click_named(control).unwrap();
            app.settle();
        }
        assert_eq!(config(&mut app), original);
    }
    app.set_role(ui::Role::Owner);
    app.settle();
    assert_eq!(
        rendered_enabled(app.harness().root_widget(), &save),
        Some(true)
    );
    app.harness().click_named(&toggle).unwrap();
    app.settle();
    assert!(!config(&mut app).profiles[0].enabled);
    app.harness().click_named(&remove).unwrap();
    app.settle();
    assert_eq!(
        config(&mut app).profiles.len(),
        1,
        "removal requires confirmation"
    );
    assert!(app.confirm_asked().is_some(), "the host asks");
    assert!(config(&mut app).profiles.is_empty());
    assert!(backend.state.lock_recover().config.profiles.is_empty());
    assert!(backend.state.lock_recover().keys.is_empty());
}

#[test]
fn mounted_rendered_model_limits_save_and_clear_on_the_model_row() {
    let backend = Arc::new(FakeAdminBackend::default());
    let profile = ProviderProfile::google_vertex("project-a", "global", "gemini-2.5-pro");
    let widget_id = format!(
        "{}-{}",
        hex::encode(&profile.id),
        hex::encode(&profile.models[0].id)
    );
    backend.state.lock_recover().config.upsert(profile);
    let mut app = mount(backend.clone());
    app.harness().resize((1800, 6000)).unwrap();
    app.settle();
    let save = format!("agent-model-limit-save-{widget_id}");
    let clear = format!("agent-model-limit-clear-{widget_id}");
    assert_eq!(
        rendered_enabled(app.harness().root_widget(), &save),
        Some(true)
    );
    for (field, value) in [
        ("output", "4096"),
        ("runtime", "32768"),
        ("deadline", "45000"),
    ] {
        app.harness()
            .click_named(&format!("agent-model-limit-{field}-{widget_id}"))
            .unwrap();
        app.harness().type_text(value);
    }
    assert!(
        backend.state.lock_recover().config.profiles[0].models[0]
            .request_limits
            .is_none()
    );
    app.harness().click_named(&save).unwrap();
    app.settle();
    let limits = config(&mut app).profiles[0].models[0]
        .request_limits
        .clone()
        .unwrap();
    assert_eq!(limits.max_output_tokens, 4096);
    assert_eq!(limits.runtime_context_tokens, Some(32768));
    assert_eq!(limits.plan_timeout_ms, Some(45000));
    app.set_role(ui::Role::Viewer);
    app.settle();
    for control in [&save, &clear] {
        assert_eq!(
            rendered_enabled(app.harness().root_widget(), control),
            Some(false)
        );
    }
    app.set_role(ui::Role::Owner);
    app.settle();
    app.harness().click_named(&clear).unwrap();
    app.settle();
    assert!(
        config(&mut app).profiles[0].models[0]
            .request_limits
            .is_none()
    );
    assert!(
        backend.state.lock_recover().config.profiles[0].models[0]
            .request_limits
            .is_none()
    );
}

#[test]
fn mounted_google_sign_in_never_offers_api_key_custody() {
    let backend = Arc::new(FakeAdminBackend::default());
    let profile = ProviderProfile::google_vertex("project-a", "global", "gemini-2.5-pro");
    let id = profile.id.clone();
    backend.state.lock_recover().config.upsert(profile);
    let mut app = mount(backend.clone());
    for effect in [
        ProviderEffect::SaveProviderKey {
            provider: id.clone(),
            key: Some("private-key-draft".into()),
        },
        ProviderEffect::RemoveProviderKey {
            provider: id.clone(),
        },
        ProviderEffect::RemoveLegacyProviderKey {
            provider: id.clone(),
        },
        ProviderEffect::MigrateProviderKey {
            provider: id,
            original_endpoint: String::new(),
        },
    ] {
        assert!(invoke(&mut app, effect).is_err());
    }
    with_panel(&mut app, |panel| {
        let profile = &panel.provider_config().profiles[0];
        assert!(!panel.needs_key(profile));
        let all = widgets::announced_text(widgets::page(panel.sections()).as_ref()).join(" ");
        assert!(all.contains(&crate::i18n::t("agent-sign-in-custody")));
        assert!(!all.contains(&crate::i18n::t("agent-key-needed")));
        assert!(!all.contains(&crate::i18n::t("agent-key-none")));
        let native =
            widgets::announced_text(widgets::page(panel.sections_without_sign_in()).as_ref())
                .join(" ");
        assert!(!native.contains(&crate::i18n::t("agent-sign-in-custody")));
        assert!(native.contains(profile.models[0].display_label()));
        assert!(native.contains(&crate::i18n::t("agent-request-limits-title")));
    });
    assert!(backend.state.lock_recover().keys.is_empty());
    assert_private(&mut app);
}

#[test]
fn mounted_metadata_waits_for_durable_ack_and_failed_create_can_retry() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    backend.delay.store(true, Ordering::SeqCst);
    backend.state.lock_recover().fail_next = true;
    app.start_effect(&host_effect(ProviderEffect::AddProvider {
        preset: "anthropic".into(),
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
            preset: "anthropic".into(),
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
            preset: "anthropic".into(),
        },
    );
    assert!(config(&mut second).profiles.is_empty());
    run(
        &mut second,
        ProviderEffect::AddProvider {
            preset: "openai-compatible".into(),
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
            preset: "anthropic".into(),
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
            preset: "anthropic".into(),
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
            preset: "anthropic".into(),
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
            preset: "anthropic".into(),
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
            preset: "anthropic".into(),
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

#[test]
fn mounted_agent_models_are_chosen_and_sent_back_to_the_default() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut profile = ProviderProfile::google_vertex("project-a", "global", "gemini-2.5-pro");
    profile
        .models
        .push(agent_provider_config::ModelDescriptor::new(
            "gemini-2.5-flash",
        ));
    let provider = profile.id.clone();
    backend.state.lock_recover().config.upsert(profile);
    let mut app = mount(backend.clone());
    app.harness().resize((1800, 6000)).unwrap();
    app.settle();
    let use_default = format!("agent-model-default-{}", hex::encode("writer"));
    assert_eq!(
        rendered_enabled(app.harness().root_widget(), &use_default),
        None
    );
    assert!(
        app.effect(&ProviderEffect::SetAgentModel {
            agent: "writer".into(),
            provider: Some(provider.clone()),
            model: Some("not-offered".into()),
        })
        .is_err()
    );
    app.effect(&ProviderEffect::SetAgentModel {
        agent: "writer".into(),
        provider: Some(provider.clone()),
        model: Some("gemini-2.5-flash".into()),
    })
    .unwrap();
    app.settle();
    let chosen = backend.state.lock_recover().config.agent_models["writer"].clone();
    assert_eq!(
        (chosen.profile.as_str(), chosen.model.as_str()),
        (provider.as_str(), "gemini-2.5-flash")
    );
    assert_eq!(
        rendered_enabled(app.harness().root_widget(), &use_default),
        Some(true)
    );
    app.harness().click_named(&use_default).unwrap();
    app.settle();
    assert!(backend.state.lock_recover().config.agent_models.is_empty());
    assert!(config(&mut app).agent_models.is_empty());
}

#[test]
fn mounted_test_connection_lists_models_to_choose_from_and_says_why_it_failed() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    app.harness().resize((1800, 6000)).unwrap();
    app.settle();
    app.start_effect(&host_effect(ProviderEffect::AddProvider {
        preset: "openrouter".into(),
    }))
    .unwrap();
    app.settle();
    let profile = config(&mut app).profiles[0].clone();
    assert_eq!(profile.endpoint, "https://openrouter.ai/api/v1");
    let id = profile.id;
    let test = format!("agent-models-test-{id}");

    // Without a key the provider refuses, and the panel says what to do.
    app.harness().click_named(&test).unwrap();
    app.settle();
    let refused = with_panel(&mut app, |panel| panel.listed_models.get(&id).cloned())
        .unwrap()
        .unwrap_err();
    assert_eq!(refused, crate::i18n::t("agent-test-key-refused"));
    // The refusal is said once, on the provider's own card, not again in one
    // notice line that speaks for every provider.
    assert_eq!(with_panel(&mut app, |panel| panel.notice.clone()), None);

    backend
        .state
        .lock_recover()
        .keys
        .insert(id.clone(), CredentialStatus::Configured);
    app.harness().click_named(&test).unwrap();
    app.settle();
    let listed = with_panel(&mut app, |panel| panel.listed_models.get(&id).cloned())
        .unwrap()
        .unwrap();
    assert_eq!(listed.len(), 2);
    // The result names the provider it is about.
    let notice = with_panel(&mut app, |panel| panel.notice.clone()).unwrap_or_default();
    assert!(
        notice.contains("OpenRouter") && notice.contains('2'),
        "the test result names its provider and its count: {notice}"
    );
    // Listing changes nothing until a model is chosen.
    assert_eq!(config(&mut app).profiles[0].default_model, None);

    let choose = format!("agent-models-choose-{id}");
    // The list opens on its first model.
    app.harness().click_named(&choose).unwrap();
    app.harness().key("Enter");
    app.settle();
    let chosen = config(&mut app).profiles[0].clone();
    assert_eq!(chosen.default_model.as_deref(), Some("listed-a"));
    // The listed name and context size come along.
    assert_eq!(chosen.models[0].label, "Listed Model A");
    assert_eq!(chosen.models[0].context_window, Some(200_000));

    // Any other id is still typed, through the same action.
    invoke(
        &mut app,
        ProviderEffect::SetProviderModel {
            provider: id.clone(),
            model: "vendor/unlisted".into(),
        },
    )
    .unwrap();
    app.settle();
    assert_eq!(
        config(&mut app).profiles[0].default_model.as_deref(),
        Some("vendor/unlisted")
    );
    // Sign-in providers are not added from here.
    let _ = app.start_effect(&host_effect(ProviderEffect::AddProvider {
        preset: "chatgpt".into(),
    }));
    app.settle();
    assert_eq!(config(&mut app).profiles.len(), 1);
}
