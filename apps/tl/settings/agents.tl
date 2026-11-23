/// The model agents answer with, and the providers and models this person set up. Choosing one
/// here is the same change the composer's own switcher makes, and it can be taken back.
view agent_models_pane() {
    let m = settings.agent_models
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
        if m.providers.is_empty() {
            ui.Text(
                "No model provider is set up in this workspace yet.",
                role: .caption,
                secondary: true,
            )
        }
        for provider in m.providers {
            ui.ListRow(
                provider.label,
                secondary: if provider.enabled { "On" } else { "Off" },
                icon: .brain,
            )
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
                    on activate: settings.use_model(
                        settings.agent_models,
                        provider: provider.id,
                        model: model.id,
                    )
                }
            }
        }
    }
}
