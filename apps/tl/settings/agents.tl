/// The model agents answer with, and the providers and models this workspace keeps: each
/// provider's name, where it answers, and the models agents may use from it. The providers are
/// the workspace's own (`tilo.models`), kept where the workspace runs with their keys in its
/// vault; a key typed here is sealed there and never read back. Choosing a model here is the
/// same change the composer's own switcher makes, and it can be taken back.
view agent_models_pane() {
    state key_for: Text = ""
    state key: Text = ""
    let m = settings.agent_models
    let p = models.models
    ui.Section("Agents answer with") {
        if m.in_use.provider != "" {
            ui.ListRow(m.in_use.model_label, secondary: m.in_use.provider, icon: .sparkles)
        } else {
            ui.Text(
                "No model is chosen yet; an agent answers with the workspace's own.",
                role: .caption,
                secondary: true,
            )
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Undo the choice", size: .small) {
                on press: settings.undo_model(settings.agent_models)
            }
            ui.Button("Redo the choice", size: .small) {
                on press: settings.redo_model(settings.agent_models)
            }
        }
    }
    ui.Section("Providers") {
        if p.providers.is_empty() {
            ui.Text(
                "No model provider is set up in this workspace yet.",
                role: .caption,
                secondary: true,
            )
        }
        for provider in p.providers {
            ui.ListRow(
                provider.name,
                secondary: provider_words(provider),
                selected: p.default_provider == .some(provider.id),
                icon: if provider.signs_in { .fingerprint } else { .cloud },
            ) {
                trailing: {
                    ui.MenuButton(
                        "More for {provider.name}",
                        icon: .more_horizontal,
                        icon_only: true,
                    ) {
                        if provider.enabled and p.default_provider != .some(provider.id) {
                            if let model = first_model(provider) {
                                ui.MenuItem("Make it the default") {
                                    on choose: models.set_default(
                                        models.models,
                                        provider: provider.id,
                                        model: model,
                                    )
                                }
                            }
                        }
                        ui.MenuItem(
                            if provider.enabled {
                                "Stop offering it to agents"
                            } else { "Offer it to agents" },
                        ) {
                            on choose: models.set_enabled(
                                models.models,
                                provider: provider.id,
                                enabled: not provider.enabled,
                            )
                        }
                        ui.MenuItem("Ask it for its models again") {
                            on choose: models.refresh_models(models.models, provider: provider.id)
                        }
                        if not provider.signs_in {
                            if provider.configured {
                                ui.MenuItem("Remove its key") {
                                    on choose: models.remove_key(
                                        models.models,
                                        provider: provider.id,
                                    )
                                }
                            } else {
                                ui.MenuItem("Keep its key") {
                                    on choose: set(key_for, provider.id)
                                }
                            }
                        }
                        ui.MenuItem("Remove this provider") {
                            on choose: models.remove_provider(models.models, provider: provider.id)
                        }
                    }
                }
            }
            if key_for == provider.id {
                row(gap: .related, align: .center, wrap: true) {
                    ui.TextInput(
                        bind: key,
                        placeholder: "The key {provider.name} gave you",
                        secret: true,
                        fill,
                    )
                    ui.Button("Keep the key", role: .primary, size: .small) {
                        on press: models
                            .set_key(
                                models.models,
                                provider: provider.id,
                                key: key,
                            )
                            .then(set(key, ""))
                            .then(set(key_for, ""))
                    }
                    ui.Button("Cancel", size: .small) {
                        on press: set(key_for, "").then(set(key, ""))
                    }
                }
            }
            for model in provider.models {
                ui.ListRow(
                    model.label,
                    secondary: if provider.default_model == .some(model.id) {
                        "Agents use this one"
                    } else { model.id },
                    selected: provider.default_model == .some(model.id)
                        and p.default_provider == .some(provider.id),
                    icon: .sparkles,
                ) {
                    on activate: models.set_default(
                        models.models,
                        provider: provider.id,
                        model: model.id,
                    )
                }
            }
        }
    }
    ui.Section("Add a provider") {
        ui.Text(
            "A provider where models answer directly: a hosted API with a key, or a server on this device or your network. The key is kept in this workspace's vault, never in this window.",
            role: .caption,
            secondary: true,
        )
        ui.MenuButton("Add a provider", icon: .plus) {
            for preset in p.presets {
                ui.MenuItem(preset.name) {
                    on choose: models.add_preset(models.models, preset: preset.preset)
                }
            }
        }
    }
}

/// One provider's line under its name: where it answers, whether agents may use it, and where
/// its key stands.
fn provider_words(provider: models.ProviderView) -> Text {
    let key = if provider.signs_in {
        "signed in"
    } else if provider.configured { "key kept" } else { "no key" }
    let offered = if provider.enabled { "" } else { "not offered" }
    if offered == "" {
        "{provider.endpoint}, {key}"
    } else { "{provider.endpoint}, {key}, {offered}" }
}

/// The model `provider` runs when agents choose it and no model is named: its own default, else
/// the first it offers.
fn first_model(provider: models.ProviderView) -> Option<Text> {
    if provider.default_model != .none {
        provider.default_model
    } else { provider.models.first()?.id }
}
