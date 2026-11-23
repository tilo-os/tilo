/// The model agents answer with, and the providers and models this person set up: each
/// provider's name, where it answers, the models agents may use from it and whether its key is
/// kept. Choosing a model here is the same change the composer's own switcher makes, and it can
/// be taken back.
view agent_models_pane() {
    // The provider whose key is being typed, and what is typed: a key is handed over once and
    // never read back, so the field clears as soon as it is kept.
    state key_provider: Text = ""
    state key_text: Text = ""
    let m = settings.agent_models
    let p = settings.agent_providers
    ui.Section("Agents answer with") {
        if m.in_use.provider != "" {
            ui.ListRow(
                m.in_use.model_label,
                secondary: m.in_use.provider,
                icon: .sparkles,
            )
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
                provider.label,
                secondary: provider_words(provider),
                selected: provider.in_use,
                icon: if provider.location == "this-device" { .laptop } else { .cloud },
            ) {
                trailing: {
                    ui.MenuButton(
                        "More for {provider.label}",
                        icon: .more_horizontal,
                        icon_only: true,
                    ) {
                        ui.MenuItem(
                            if provider.enabled {
                                "Stop offering it to agents"
                            } else {
                                "Offer it to agents"
                            },
                        ) {
                            on choose: settings.set_provider_enabled(
                                settings.agent_providers,
                                provider.id,
                                not provider.enabled,
                            )
                        }
                        if provider.enabled and not provider.in_use {
                            ui.MenuItem("Make it the default") {
                                on choose: settings.set_default_provider(
                                    settings.agent_providers,
                                    provider.id,
                                )
                            }
                        }
                        if provider.authentication == "api-key" {
                            ui.MenuItem(
                                if provider.key == "stored" {
                                    "Change the key"
                                } else {
                                    "Keep a key"
                                },
                            ) {
                                on choose: set(key_provider, provider.id).then(set(key_text, ""))
                            }
                            if provider.key == "stored" {
                                ui.MenuItem("Forget the key", destructive: true) {
                                    on choose: settings.clear_key(
                                        settings.agent_providers,
                                        provider.id,
                                    )
                                }
                            }
                        }
                        ui.MenuItem("Remove", destructive: true) {
                            on choose: settings.remove_provider(
                                settings.agent_providers,
                                provider.id,
                            )
                        }
                    }
                }
            }
            for model in provider.models {
                ui.ListRow(
                    model.label,
                    secondary: if model.id == provider.model {
                        "Agents use this one"
                    } else {
                        model.id
                    },
                    selected: model.id == provider.model,
                    icon: .sparkles,
                ) {
                    on activate: settings.set_provider_model(
                        settings.agent_providers,
                        provider.id,
                        model.id,
                    )
                }
            }
        }
    }
    if key_provider != "" {
        ui.Section("A key for {provider_label(p, key_provider)}") {
            ui.Text(
                "The key is kept in this device's vault, never in the settings the workspace syncs.",
                role: .caption,
                secondary: true,
            )
            ui.TextInput(
                bind: key_text,
                placeholder: "The API key",
                secret: true,
            ) {
                on submit: settings.set_key(
                    settings.agent_providers,
                    key_provider,
                    key_text,
                ).then(set(key_text, "")).then(set(key_provider, ""))
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Keep the key", role: .primary, enabled: key_text != "") {
                    on press: settings.set_key(
                        settings.agent_providers,
                        key_provider,
                        key_text,
                    ).then(set(key_text, "")).then(set(key_provider, ""))
                }
                ui.Button("Cancel") {
                    on press: set(key_provider, "").then(set(key_text, ""))
                }
            }
        }
    }
    ui.Section("Add a provider") {
        ui.Text(
            "A provider where models answer directly: a hosted API with a key, or a server on this device or your network.",
            role: .caption,
            secondary: true,
        )
        ui.MenuButton("Add a provider", icon: .plus) {
            for preset in p.presets {
                ui.MenuItem(preset.label) {
                    on choose: settings.add_provider(settings.agent_providers, preset.slug)
                }
            }
        }
    }
}

/// One provider's line under its name: where it answers, whether agents may use it, and where
/// its key stands.
fn provider_words(provider: settings.AgentProviderShown) -> Text {
    let place = if provider.location == "this-device" {
        "On this device"
    } else if provider.location == "private-network" {
        "On your network"
    } else {
        provider.protocol
    }
    let key = if provider.authentication == "sign-in" {
        if provider.sign_in != "" {
            "signed in"
        } else {
            "not signed in"
        }
    } else if provider.key == "stored" {
        "key kept"
    } else if provider.key == "not-needed" {
        ""
    } else {
        "no key"
    }
    let offered = if provider.enabled { "" } else { "not offered" }
    if key == "" and offered == "" {
        place
    } else if key == "" {
        "{place}, {offered}"
    } else if offered == "" {
        "{place}, {key}"
    } else {
        "{place}, {key}, {offered}"
    }
}

/// What the person calls the provider `id` of `providers`, or the id itself for one they removed
/// while its key field was open.
fn provider_label(providers: settings.AgentProviderSetup, id: Text) -> Text {
    providers.providers.find(p -> p.id == id)?.label ?? id
}
