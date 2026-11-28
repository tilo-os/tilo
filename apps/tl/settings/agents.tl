/// The model agents answer with, and the providers and models this workspace keeps: each
/// provider's name, where it answers, and the models agents may use from it. The providers are
/// the workspace's own (`tilo.models`), kept where the workspace runs with their keys in its
/// vault; a key typed here is sealed there and never read back. Choosing a model here is the
/// same change the composer's own switcher makes, and it can be taken back.
view agent_models_pane() {
    state key_for: Text = ""
    state key: Text = ""
    state adding_for: Text = ""
    state adding_model: Text = ""
    state editing_provider: Text = ""
    state edit_name: Text = ""
    state edit_endpoint: Text = ""
    state edit_protocol: Text = "open-ai-compatible"
    state sign_in: Text = ""
    state sign_in_code: Text = ""
    state sign_in_page: Text = ""
    state sign_in_note: Text = ""
    state providers_before: Int = 0
    state vertex_project: Text = ""
    state vertex_model: Text = ""
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
    ui.Section("Sign in to your AI account") {
        let signed_in = p.providers.filter(x -> x.signs_in)
        ui.Text(
            "Sign in to a plan you already pay for, and its models answer as one of this workspace's providers. Nothing is typed and no key is kept here: the sign-in is sealed in the workspace's vault.",
            role: .caption,
            secondary: true,
        )
        if sign_in != "" and p.providers.len() <= providers_before {
            if sign_in_page != "" {
                ui.Link(sign_in_page, label: "Open the page to finish signing in")
            }
            if sign_in_code != "" {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text("Enter the code there:", role: .caption)
                    ui.Text(sign_in_code, role: .code)
                }
            }
            ui.Text(
                if sign_in_note != "" {
                    sign_in_note
                } else {
                    "Waiting for you to finish signing in. Check it once you have."
                },
                role: .caption,
                secondary: true,
            )
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Check the sign-in", size: .small) {
                    on press: {
                        let state = models.wait_sign_in(models.models, signIn: sign_in)
                        match state {
                            .done(provider) => set(
                                    sign_in_note,
                                    "Signed in: {provider.name} is ready to answer.",
                                )
                                .then(set(sign_in, "")),
                            .waiting => set(
                                sign_in_note,
                                "Still waiting for you to finish signing in.",
                            ),
                            .failed(reason) => set(
                                sign_in_note,
                                "The sign-in did not finish: {reason}",
                            ),
                        }
                    }
                }
                ui.Button("Cancel the sign-in", size: .small) {
                    on press: models
                        .cancel_sign_in(models.models, signIn: sign_in)
                        .then(set(sign_in, ""))
                        .then(set(sign_in_note, ""))
                }
            }
        }
        if sign_in == "" {
            if not signed_in.any(x -> x.configured) {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Button("Sign in with ChatGPT", icon: .fingerprint) {
                        on press: {
                            let step = models.begin_sign_in(models.models, using: .subscription)
                            set(providers_before, models.models.providers.len())
                                .then(set(sign_in, step.sign_in))
                                .then(set(sign_in_code, step.code))
                                .then(set(sign_in_page, step.page))
                                .then(set(sign_in_note, ""))
                        }
                    }
                    ui.Button("Sign in with API usage instead", size: .small) {
                        on press: {
                            let step = models.begin_sign_in(models.models, using: .api-usage)
                            set(providers_before, models.models.providers.len())
                                .then(set(sign_in, step.sign_in))
                                .then(set(sign_in_code, step.code))
                                .then(set(sign_in_page, step.page))
                                .then(set(sign_in_note, ""))
                        }
                    }
                }
                ui.Text(
                    "A ChatGPT plan answers on the subscription you pay for. API usage bills your OpenAI organization per token.",
                    role: .caption,
                    secondary: true,
                )
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.MenuButton("Sign in to another provider", icon: .fingerprint) {
                    ui.MenuItem("GitHub Copilot", icon: .fingerprint) {
                        on choose: {
                            let step = models.begin_sign_in(
                                models.models,
                                service: "github-copilot",
                            )
                            set(providers_before, models.models.providers.len())
                                .then(set(sign_in, step.sign_in))
                                .then(set(sign_in_code, step.code))
                                .then(set(sign_in_page, step.page))
                                .then(set(sign_in_note, ""))
                        }
                    }
                    ui.MenuItem("OpenRouter", icon: .key) {
                        on choose: {
                            let step = models.begin_sign_in(models.models, service: "openrouter")
                            set(providers_before, models.models.providers.len())
                                .then(set(sign_in, step.sign_in))
                                .then(set(sign_in_code, step.code))
                                .then(set(sign_in_page, step.page))
                                .then(set(sign_in_note, ""))
                        }
                    }
                    ui.MenuItem(
                        "Hugging Face, for this computer's model downloads",
                        icon: .download,
                    ) {
                        on choose: {
                            let step = models.begin_sign_in(models.models, service: "hugging-face")
                            set(providers_before, models.models.providers.len())
                                .then(set(sign_in, step.sign_in))
                                .then(set(sign_in_code, step.code))
                                .then(set(sign_in_page, step.page))
                                .then(set(sign_in_note, ""))
                        }
                    }
                }
            }
            ui.Text(
                "Vertex AI and the Gemini API sign in with your Google account, billed to a Cloud project.",
                role: .caption,
                secondary: true,
            )
            row(gap: .related, align: .center, wrap: true) {
                ui.TextInput(
                    bind: vertex_project,
                    placeholder: "Your Google Cloud project id",
                    fill,
                )
                ui.TextInput(
                    bind: vertex_model,
                    placeholder: "The model, such as gemini-4-argon",
                    fill,
                )
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Sign in to Vertex AI", size: .small) {
                    on press: {
                        let step = models.begin_sign_in(
                            models.models,
                            service: "google-vertex",
                            project: vertex_project,
                            model: vertex_model,
                        )
                        set(providers_before, models.models.providers.len())
                            .then(set(sign_in, step.sign_in))
                            .then(set(sign_in_code, step.code))
                            .then(set(sign_in_page, step.page))
                            .then(set(sign_in_note, ""))
                    }
                }
                ui.Button("Sign in to Gemini", size: .small) {
                    on press: {
                        let step = models.begin_sign_in(
                            models.models,
                            service: "google-gemini",
                            project: vertex_project,
                            model: vertex_model,
                        )
                        set(providers_before, models.models.providers.len())
                            .then(set(sign_in, step.sign_in))
                            .then(set(sign_in_code, step.code))
                            .then(set(sign_in_page, step.page))
                            .then(set(sign_in_note, ""))
                    }
                }
            }
        }
    }
    ui.Section("The model each kind of work runs on") {
        ui.Text(
            "Every kind of work follows the default above until it is given a model of its own.",
            role: .caption,
            secondary: true,
        )
        for kind in work_kinds() {
            let chosen = p.assignments.find(a -> a.kind == kind)
            let kind_name = if kind == "one_shot" {
                "A short answer"
            } else if kind == "task" {
                "A whole task"
            } else if kind == "subtask" {
                "Part of a task"
            } else if kind == "request" {
                "A message to the agent"
            } else if kind == "classifier" {
                "Choosing a label"
            } else if kind == "dictation" { "Tidying dictation" } else { kind }
            ui.ListRow(
                kind_name,
                secondary: if let a = chosen {
                    "{provider_name(p, a.provider)}, {model_label(p, a.provider, a.model)}"
                } else { "Follows the default" },
                icon: .sparkles,
            ) {
                trailing: {
                    ui.MenuButton(
                        "Choose the model for {kind_name}",
                        icon: .sparkles,
                        icon_only: true,
                    ) {
                        ui.MenuItem("Follow the default") {
                            on choose: models.set_assignments(models.models, kinds: [kind])
                        }
                        for provider in p.providers {
                            if provider.enabled {
                                for model in provider.models {
                                    ui.MenuItem("{provider.name}: {model.label}") {
                                        on choose: models.set_assignments(
                                            models.models,
                                            kinds: [kind],
                                            provider: provider.id,
                                            model: model.id,
                                        )
                                    }
                                }
                            }
                        }
                    }
                }
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
                        ui.MenuItem("Add a model by hand") {
                            on choose: set(adding_for, provider.id)
                        }
                        ui.MenuItem("Edit this provider") {
                            on choose: set(editing_provider, provider.id)
                                .then(set(edit_name, provider.name))
                                .then(
                                    set(edit_endpoint, provider.endpoint),
                                )
                                .then(
                                    set(
                                        edit_protocol,
                                        if provider.protocol == .anthropic {
                                            "anthropic"
                                        } else { "open-ai-compatible" },
                                    ),
                                )
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
                        } else if provider.configured {
                            ui.MenuItem("Sign out") {
                                on choose: models.remove_key(models.models, provider: provider.id)
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
            if adding_for == provider.id {
                row(gap: .related, align: .center, wrap: true) {
                    ui.TextInput(
                        bind: adding_model,
                        placeholder: "The model's id, as {provider.name} names it",
                        fill,
                    )
                    ui.Button("Add the model", role: .primary, size: .small) {
                        on press: models
                            .add_model(
                                models.models,
                                provider: provider.id,
                                model: adding_model,
                            )
                            .then(set(adding_model, ""))
                            .then(set(adding_for, ""))
                    }
                    ui.Button("Cancel", size: .small) {
                        on press: set(adding_for, "").then(set(adding_model, ""))
                    }
                }
            }
            if editing_provider == provider.id {
                row(gap: .related, align: .center, wrap: true) {
                    ui.TextInput(bind: edit_name, placeholder: "The provider's name", fill)
                    ui.TextInput(
                        bind: edit_endpoint,
                        placeholder: "The base URL every request goes to",
                        fill,
                    )
                }
                row(gap: .related, align: .center, wrap: true) {
                    ui.MenuButton(protocol_words(edit_protocol), icon: .sliders_horizontal) {
                        ui.MenuItem("OpenAI-compatible API") {
                            on choose: set(edit_protocol, "open-ai-compatible")
                        }
                        ui.MenuItem("Anthropic's API") {
                            on choose: set(edit_protocol, "anthropic")
                        }
                    }
                    ui.Button("Save", role: .primary, size: .small) {
                        on press: models
                            .update_provider(
                                models.models,
                                provider: provider.id,
                                name: edit_name,
                                endpoint: edit_endpoint,
                                protocol: if edit_protocol == "anthropic" {
                                    .anthropic
                                } else { .open-ai-compatible },
                            )
                            .then(set(editing_provider, ""))
                    }
                    ui.Button("Cancel", size: .small) {
                        on press: set(editing_provider, "")
                    }
                }
                ui.Text(
                    "A key is kept for the address it was entered for, so a new address stops it being sent until its key is kept again.",
                    role: .caption,
                    secondary: true,
                )
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
    ui.Section("Models this computer runs") {
        ui.Text(
            "Installed here and served by this computer's own model service, so their answers come from this machine. A workspace kept elsewhere runs none.",
            role: .caption,
            secondary: true,
        )
        if let local = p.local {
            if not local.available {
                ui.Text(
                    local.problem ?? "This computer's model service did not answer.",
                    role: .caption,
                    secondary: true,
                )
            } else {
                let moving = local.models.any(
                    x -> x.phase.state == "downloading" or x.phase.state == "installing",
                )
                    or local.in_process.any(
                        x -> x.phase.state == "downloading" or x.phase.state == "installing",
                    )
                if moving {
                    every 1s: models.local_models(models.models)
                }
                ui.Text(
                    "{local_fit_words(local.hardware.fit)}: {local.hardware.reason}",
                    role: .caption,
                    secondary: true,
                )
                if local.accelerator == "available" {
                    ui.Button("Set up the accelerator this computer needs", size: .small) {
                        on press: models.stage_accelerator(models.models)
                    }
                } else if local.accelerator == "queued" or local.accelerator == "staging" {
                    ui.Text(
                        "Preparing the accelerator this computer needs.",
                        role: .caption,
                        secondary: true,
                    )
                } else if local.accelerator == "restart-required" {
                    ui.Text(
                        "The accelerator is ready after this computer restarts.",
                        role: .caption,
                        secondary: true,
                    )
                }
                if let package = local.recommendation {
                    ui.ListRow(
                        package.name,
                        secondary: "{local_size_words(package.download_bytes)} to download",
                        icon: .download,
                    ) {
                        trailing: {
                            ui.Button("Install", role: .primary, size: .small) {
                                on press: models.install_local_model(
                                    models.models,
                                    model: package.id,
                                    revision: package.revision,
                                    acceptLicense: true,
                                )
                            }
                        }
                    }
                    ui.Text(package.description, role: .caption, secondary: true)
                    row(gap: .related, align: .center, wrap: true) {
                        ui.Text("{package.license}, at its licence page", role: .caption)
                        if package.license_url != "" {
                            ui.Link(package.license_url, label: "The model's licence")
                        }
                    }
                }
                if local.models.is_empty() {
                    ui.Text(
                        "No model is installed on this computer yet.",
                        role: .caption,
                        secondary: true,
                    )
                }
                for model in local.models {
                    ui.ListRow(model.name, secondary: local_phase_words(model), icon: .hard_drive) {
                        trailing: {
                            row(gap: .tight, align: .center, wrap: true) {
                                if model.phase.state == "ready" {
                                    if model.phase.running {
                                        ui.Button("Stop", size: .small) {
                                            on press: models.stop_local_model(
                                                models.models,
                                                model: model.id,
                                            )
                                        }
                                        if local_in_use(p, model) {
                                            ui.Chip("In use")
                                        } else {
                                            ui.Button("Use it", size: .small) {
                                                on press: models.use_local_model(
                                                    models.models,
                                                    model: model.id,
                                                )
                                            }
                                        }
                                    } else {
                                        ui.Button("Start", size: .small) {
                                            on press: models.start_local_model(
                                                models.models,
                                                model: model.id,
                                            )
                                        }
                                    }
                                }
                                if model.phase.state == "error"
                                    or model.phase.state == "ready"
                                    or model.phase.state == "installing"
                                    or model.phase.state == "downloading" {
                                    ui.Button("Remove", size: .small) {
                                        on press: models.remove_local_model(
                                            models.models,
                                            model: model.id,
                                        )
                                    }
                                }
                            }
                        }
                    }
                    if model.phase.state == "downloading" or model.phase.state == "installing" {
                        ui.ProgressBar(
                            value: model.phase.percent.to_float() / 100.0,
                            label: local_phase_words(model),
                        )
                    }
                }
                for model in local.in_process {
                    ui.ListRow(model.name, secondary: local_phase_words(model), icon: .sparkles) {
                        trailing: {
                            if model.phase.state == "" or model.phase.state == "error" {
                                ui.Button("Install", size: .small) {
                                    on press: models.install_local_model(
                                        models.models,
                                        model: model.id,
                                        revision: model.revision,
                                        acceptLicense: true,
                                    )
                                }
                            } else {
                                ui.Button("Remove", size: .small) {
                                    on press: models.remove_local_model(
                                        models.models,
                                        model: model.id,
                                    )
                                }
                            }
                        }
                    }
                    if model.phase.state == "downloading" or model.phase.state == "installing" {
                        ui.ProgressBar(
                            value: model.phase.percent.to_float() / 100.0,
                            label: local_phase_words(model),
                        )
                    }
                }
            }
        } else {
            after 0s: models.local_models(models.models)
            ui.Text("Looking for the models this computer runs…", role: .caption, secondary: true)
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
        if provider.configured { "signed in" } else { "signed out" }
    } else if provider.configured { "key kept" } else { "no key" }
    let offered = if provider.enabled { "" } else { "not offered" }
    if offered == "" {
        "{provider.endpoint}, {key}"
    } else { "{provider.endpoint}, {key}, {offered}" }
}

/// What a sign-in's state means, in the words a person reads.
fn sign_in_words(state: models.SignInState) -> Text {
    match state {
        .waiting => "Still waiting for you to finish signing in.",
        .done(provider) => "Signed in: {provider.name} is ready to answer.",
        .failed(reason) => "The sign-in did not finish: {reason}",
    }
}

/// What a provider speaks, in the words a person reads.
fn protocol_words(protocol: Text) -> Text {
    if protocol == "anthropic" { "Anthropic's API" } else { "OpenAI-compatible API" }
}

/// How well a local model fits this computer, in one word.
fn local_fit_words(fit: Text) -> Text {
    if fit == "excellent" {
        "Runs well here"
    } else if fit == "good" {
        "Fits here"
    } else if fit == "tight" {
        "Fits, but close"
    } else if fit == "unsupported" { "Too large for this computer" } else { fit }
}

/// Where a local model's setup stands, in the words a person reads.
fn local_phase_words(model: models.LocalModel) -> Text {
    let phase = model.phase
    if phase.state == "downloading" {
        "Downloading {local_size_words(phase.downloaded_bytes)} of {local_size_words(phase.total_bytes)}"
    } else if phase.state == "installing" {
        if phase.message != "" { phase.message } else { "Installing" }
    } else if phase.state == "ready" {
        if phase.running {
            if phase.endpoint == "" { "Running" } else { "Running at {phase.endpoint}" }
        } else { "Installed, not running" }
    } else if phase.state == "error" {
        if phase.message != "" { "Failed: {phase.message}" } else { "Failed" }
    } else if phase.state == "" { "Not set up" } else { phase.state }
}

/// Whether the workspace already asks `model` at the address this computer serves it at, and
/// work runs on it.
fn local_in_use(p: models.Models, model: models.LocalModel) -> Bool {
    model.phase.endpoint != ""
        and p.providers.any(
            x -> x.endpoint == model.phase.endpoint and p.default_provider == .some(x.id),
        )
}

/// Bytes in the words a person reads: `18.2 GiB`, `740 MiB`.
fn local_size_words(bytes: Int) -> Text {
    if bytes < 1024 {
        "{bytes} B"
    } else if bytes < 1048576 {
        "{local_tenths(bytes, 1024)} KiB"
    } else if bytes < 1073741824 {
        "{local_tenths(bytes, 1048576)} MiB"
    } else { "{local_tenths(bytes, 1073741824)} GiB" }
}

/// `bytes` divided by `unit`, to one place, without a trailing zero.
fn local_tenths(bytes: Int, unit: Int) -> Text {
    let whole = bytes / unit
    let part = bytes % unit * 10 / unit
    if part == 0 { "{whole}" } else { "{whole}.{part}" }
}

/// The model `provider` runs when agents choose it and no model is named: its own default, else
/// the first it offers.
fn first_model(provider: models.ProviderView) -> Option<Text> {
    if provider.default_model != .none {
        provider.default_model
    } else { provider.models.first()?.id }
}

/// Every kind of work a model can be chosen for, in the order the workspace lists them.
fn work_kinds() -> List<Text> {
    ["one_shot", "task", "subtask", "request", "classifier", "dictation"]
}

/// What people call the provider `id`, or the id itself when the workspace no longer keeps it.
fn provider_name(p: models.Models, id: Text) -> Text {
    if let provider = p.providers.find(x -> x.id == id) { provider.name } else { id }
}

/// What people call the model `model` of `provider`, or its id when the provider no longer
/// offers it.
fn model_label(p: models.Models, provider: Text, model: Text) -> Text {
    if let found = p.providers.find(x -> x.id == provider) {
        if let offered = found.models.find(x -> x.id == model) { offered.label } else { model }
    } else { model }
}
