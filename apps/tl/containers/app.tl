app so.tilo.containers "Containers" (
    icon: .box,
    category: .development,
    tagline: "Linux containers, images, volumes and distroboxes on this computer",
    keywords: ["containers", "podman", "docker", "images", "volumes", "distrobox", "container", "virtualisierung"],
    splash_budget: 250,
)

use so.tilo.device as device

/// What a person calls each of the four places.
fn place_label(p: Text) -> Text {
    if p == "containers" {
        std.say("Containers")
    } else if p == "images" {
        std.say("Images")
    } else if p == "volumes" { std.say("Volumes") } else { std.say("Distrobox") }
}

/// The dimension a detail tab stands at in the strip.
fn tab_index(tab: Text) -> Int {
    if tab == "stats" {
        1
    } else if tab == "inspect" { 2 } else if tab == "terminal" { 3 } else { 0 }
}

fn tab_key(index: Int) -> Text {
    if index == 1 {
        "stats"
    } else if index == 2 { "inspect" } else if index == 3 { "terminal" } else { "logs" }
}

/// The new-container form: what the person typed, one text per field, edited together by
/// everyone looking at it.
type Draft {
    name: Text = ""
    image: Text = ""
    ports: Text = ""
    env: Text = ""
    volumes: Text = ""
    command: Text = ""
}

/// An image being pulled, and what the engine wrote while it did: the person's own, on their
/// device, so it never reaches anyone else at the workspace.
type Pull (readers: author) {
    image: Text
    /// The engine's job, empty before it starts.
    job: Text = ""
    /// The engine's output, newest last.
    lines: List<Text>
    done: Bool = false
    failed: Text = ""
}

/// The app's own record: the new-container draft and the pulls started here.
type Containers {
    draft: Draft
    pulls: Collection<Pull>
}

root containers: Containers

default view window for Containers

/// What begins a pull says which image.
type PullAsk {
    image: Text
}

/// What the engine answers when a pull starts.
type JobAnswer {
    job: Text
}

/// How a pull stands, as the engine reports it.
type JobState {
    state: Text
    lines: List<Text>
    error: Text
}

/// How a container's state reads.
fn state_words(state: device.ContainerState) -> Text {
    if state == .running {
        std.say("Running")
    } else if state == .exited {
        std.say("Exited")
    } else if state == .created {
        std.say("Created")
    } else if state == .paused {
        std.say("Paused")
    } else if state == .restarting { std.say("Restarting") } else { std.say("Needs attention") }
}

/// A size in the words a person reads: `18.2 MB`, `1.5 GB`.
fn size_words(bytes: Int) -> Text {
    if bytes < 1000 {
        std.say("{bytes} B")
    } else if bytes < 1000000 {
        std.say("{bytes / 1000} KB")
    } else if bytes < 1000000000 {
        std.say("{bytes / 1000000} MB")
    } else { std.say("{bytes / 1000000000} GB") }
}

/// How many containers use an image or a volume.
fn used_by(count: Int) -> Text {
    if count == 0 {
        std.say("Used by no container")
    } else if count == 1 {
        std.say("Used by 1 container")
    } else { std.say("Used by {count} containers") }
}

/// An image as one written reference, `repository:tag`.
fn reference_of(image: device.Image) -> Text {
    "{image.repository}:{image.tag}"
}

/// The bytes the images hold, summed one at a time.
fn image_bytes(images: List<device.Image>, at: Int) -> Int {
    if at >= images.len() {
        0
    } else {
        (images.at(at)?.size_bytes ?? 0) + image_bytes(images, at + 1)
    }
}

/// The share of a memory limit a container uses, 0 when no limit was read.
fn mem_fraction(stats: device.ContainerStats) -> Float {
    if stats.mem_limit_bytes == 0 {
        0.0
    } else {
        stats.mem_bytes.to_float() / stats.mem_limit_bytes.to_float()
    }
}

/// The lines of a container's log that name an error or worse.
fn trouble_line(line: Text) -> Bool {
    let lower = line.lower()
    lower.contains("error")
        or lower.contains("fatal")
        or lower.contains("panic")
        or lower.contains("critical")
}

fn warn_line(line: Text) -> Bool {
    line.lower().contains("warn")
}

/// Clears the new-container form.
action clear_create(c: Containers) {
    c.draft.name = ""
    c.draft.image = ""
    c.draft.ports = ""
    c.draft.env = ""
    c.draft.volumes = ""
    c.draft.command = ""
}

/// Fills one field of the new-container form, as an agent or a script would: `name`, `image`,
/// `ports`, `env`, `volumes` or `command`.
action fill(c: Containers, field: Text, text: Text) {
    if field == "name" {
        c.draft.name = text
    } else if field == "image" {
        c.draft.image = text
    } else if field == "ports" {
        c.draft.ports = text
    } else if field == "env" {
        c.draft.env = text
    } else if field == "volumes" {
        c.draft.volumes = text
    } else if field == "command" {
        c.draft.command = text
    }
}

/// Runs an image as a new container from the draft, and clears the form when the engine took it.
action create_container(c: Containers) {
    require c.draft.image.trim() != "" "An image is needed to run a container"
    device.create(
        device.containers,
        name: c.draft.name,
        image: c.draft.image,
        ports: c.draft.ports,
        env: c.draft.env,
        volumes: c.draft.volumes,
        command: c.draft.command,
    )
    clear_create(c)
}

/// Makes a volume.
action make_volume(c: Containers, name: Text) {
    require name.trim() != "" "A volume needs a name"
    device.create_volume(device.containers, name: name)
}

/// Gives an image another reference.
action tag_image(c: Containers, image: Text, tag: Text) {
    require tag.trim() != "" "A tag needs a name"
    device.tag(device.containers, image: image, tag: tag)
}

/// Starts pulling an image, and keeps its output as it comes.
action begin_pull(c: Containers, image: Text) {
    require image.trim() != "" "There is no image to pull"
    if let running = c.pulls.find(p -> p.job != "" and not p.done) {
        return
    }
    match engine.start("pull", json.encode(PullAsk(image: image))) {
        .err(why) => {
            c.pulls.insert(Pull(image: image, failed: why, done: true))
        }
        .ok(text) => match json.decode<JobAnswer>(text) {
            .err(why) => {
                c.pulls.insert(Pull(image: image, failed: why, done: true))
            }
            .ok(answer) => {
                c.pulls.insert(Pull(image: image, job: answer.job))
            }
        },
    }
}

/// Asks how far a pull has got; a finished or failed one says so and stays.
action poll_pull(c: Containers) {
    for p in c.pulls {
        if p.job != "" and not p.done {
            match engine.status(p.job) {
                .err(why) => {
                    p.failed = why
                    p.done = true
                    p.job = ""
                }
                .ok(text) => match json.decode<JobState>(text) {
                    .err(why) => {
                        p.failed = why
                        p.done = true
                        p.job = ""
                    }
                    .ok(state) => {
                        p.lines = state.lines
                        if state.state == "done" {
                            p.done = true
                            p.job = ""
                        } else if state.state == "failed" {
                            p.failed = state.error
                            p.done = true
                            p.job = ""
                        } else if state.state == "cancelled" {
                            p.done = true
                            p.job = ""
                        }
                    }
                },
            }
        }
    }
}

/// Ends a pull and the engine's process with it.
action cancel_pull(c: Containers, p: Pull) {
    if p.job != "" {
        engine.cancel(p.job)
        p.job = ""
        p.done = true
    }
}

/// Takes a finished or failed pull out of the window.
action dismiss_pull(c: Containers, p: Pull) {
    c.pulls.remove(p)
}

/// A pull runs while the window is open, so its progress is asked for once a second.
trigger watch_pull(c: Containers) every 1s: poll_pull(c)

/// Opens a shell in a running container: podman's, in the Tilo terminal.
action open_shell(container: Text) uses files {
    match tool.container_shell(container, "podman") {
        .ok => {}
        .err(why) => { tool.terminal("") }
    }
}

/// Opens a shell in a distrobox.
action enter_box(box: Text) uses files {
    match tool.container_shell(box, "distrobox") {
        .ok => {}
        .err(why) => { tool.terminal("") }
    }
}

/// Runs what the prompt asked, once the person pressed its verb.
action prompt_run(prompt: Text, typed: Text) {
    if prompt == "volume" {
        make_volume(root Containers, typed)
    } else if prompt == "pull" {
        begin_pull(root Containers, typed)
    } else if prompt.starts_with("tag:") {
        tag_image(root Containers, prompt.slice(4, prompt.len()), typed)
    } else if prompt.starts_with("remove-image:") {
        device.remove_image(device.containers, image: prompt.slice(13, prompt.len()))
    } else if prompt.starts_with("remove-volume:") {
        device.remove_volume(device.containers, volume: prompt.slice(14, prompt.len()))
    }
}

/// Does what the confirmation named.
action confirm_run(confirm: Text) {
    if confirm.starts_with("container:") {
        device.remove(device.containers, container: confirm.slice(10, confirm.len()))
    } else if confirm.starts_with("image:") {
        device.remove_image(device.containers, image: confirm.slice(6, confirm.len()))
    } else if confirm.starts_with("volume:") {
        device.remove_volume(device.containers, volume: confirm.slice(7, confirm.len()))
    }
}

/// The window: a sidebar of the four things the engine holds, and the chosen one beside it.
view window(c: Containers) {
    state place: Text = "containers"
    state chosen: Text = ""
    state tab: Text = "logs"
    state creating: Bool = false
    state confirm: Text = ""
    state prompt: Text = ""
    state typed: Text = ""
    let engine_now = device.containers
    ui.NavShell(content_kind: .page, title: place_label(place), sidebar_title: "Containers") {
        sidebar: {
            stack {
                ui.NavList(selected: place) {
                    ui.NavSection("This computer") {
                        ui.NavItem(
                            "containers",
                            "Containers",
                            icon: .box,
                            count: engine_now.containers.len(),
                        ) {
                            on select: set(place, "containers")
                        } as place_containers
                        ui.NavItem(
                            "images",
                            "Images",
                            icon: .layers,
                            count: engine_now.images.len(),
                        ) {
                            on select: set(place, "images")
                        } as place_images
                        ui.NavItem(
                            "volumes",
                            "Volumes",
                            icon: .hard_drive,
                            count: engine_now.volumes.len(),
                        ) {
                            on select: set(place, "volumes")
                        } as place_volumes
                        ui.NavItem(
                            "distrobox",
                            "Distrobox",
                            icon: .terminal,
                            count: engine_now.distroboxes.len(),
                        ) {
                            on select: set(place, "distrobox")
                        } as place_distrobox
                    }
                }
                if not engine_now.live {
                    ui.StatusLine("Podman isn't answering", status: .warning)
                }
                if let problem = engine_now.problem {
                    ui.StatusLine(problem, status: .error)
                }
            }
        }
        actions: {
            if engine_now.serving {
                ui.Button("New container", icon: .plus, role: .primary) {
                    on press: set(creating, true)
                } as new_container
            }
            ui.Button("Read the engine again", icon: .rotate_cw, icon_only: true) {
                on press: device.refresh(device.containers)
            }
        }
        if not engine_now.serving {
            engine_missing(engine_now)
        } else if place == "containers" {
            containers_page(c, engine_now, chosen, tab, confirm)
        } else if place == "images" {
            images_page(c, engine_now, prompt)
        } else if place == "volumes" {
            volumes_page(c, engine_now, prompt)
        } else {
            distrobox_page(engine_now)
        }
    }
    create_form(c, creating)
    confirm_dialog(confirm)
    prompt_dialog(prompt, typed)
}

/// What a computer with no podman says, with the reason the probe gave behind it.
view engine_missing(engine_now: device.Containers) {
    ui.EmptyState(
        "Podman isn't available",
        message: "This computer has no container engine to read, or podman did not answer. Install podman and try again.",
        icon: .box,
        action: "Try again",
    ) {
        on act: device.refresh(device.containers)
    }
    if engine_now.connection != "" {
        ui.Text(engine_now.connection, role: .caption, secondary: true)
    }
}

/// The containers: a list that opens a container's detail beside it.
view containers_page(
    c: Containers,
    engine_now: device.Containers,
    chosen: State<Text>,
    tab: State<Text>,
    confirm: State<Text>,
) {
    for pull in c.pulls {
        pull_banner(c, pull)
    }
    split(side: .leading, collapse: .stack) {
        primary(min: 16rem) {
            if engine_now.containers.is_empty() {
                ui.EmptyState("No containers", message: "Run an image to make one.", icon: .box)
            }
            scroll {
                stack(gap: .none) {
                    for item in engine_now.containers {
                        ui.ListRow(
                            item.name,
                            secondary: item.image,
                            value: state_words(item.state),
                            meta_tone: if item.state == .running {
                                .success
                            } else if item.state == .paused or item.state == .restarting {
                                .warning
                            } else if item.state == .dead { .danger } else { .neutral },
                            icon: .box,
                            selected: chosen == item.id,
                        ) {
                            on activate: set(chosen, item.id)
                        }
                    }
                }
            }
        }
        secondary(min: 20rem) {
            if let item = engine_now.containers.find(x -> x.id == chosen) {
                container_detail(item, tab, confirm)
            } else {
                ui.EmptyState(
                    "No container selected",
                    message: "Choose a container to see its logs, usage and settings.",
                    icon: .box,
                )
            }
        }
    }
}

/// The chosen container: what it is, its verbs, and one detail tab.
view container_detail(item: device.Container, tab: State<Text>, confirm: State<Text>) {
    ui.ObjectHeader(item.name, caption: item.image, icon: .box)
    row(gap: .tight, wrap: true) {
        ui.Badge(
            state_words(item.state),
            tone: if item.state == .running {
                .success
            } else if item.state == .paused or item.state == .restarting {
                .warning
            } else if item.state == .dead { .danger } else { .neutral },
        )
        ui.Text(item.status_detail, role: .caption, secondary: true)
        spacer
        lifecycle_buttons(item)
        ui.Button("Remove", icon: .trash, role: .destructive) {
            on press: set(confirm, "container:{item.id}")
        }
    }
    ui.ViewTabs(selected: tab_index(tab), label: "Detail") {
        ui.Segment("Logs")
        ui.Segment("Stats")
        ui.Segment("Inspect")
        ui.Segment("Terminal")
        on change(i): set(tab, tab_key(i))
    }
    if tab == "logs" {
        logs_pane(item)
    } else if tab == "stats" {
        stats_pane(item)
    } else if tab == "inspect" {
        inspect_pane(item)
    } else {
        terminal_pane(item)
    }
}

/// The verbs a container's state allows.
view lifecycle_buttons(item: device.Container) {
    if item.state == .running {
        ui.Button("Stop", icon: .square) {
            on press: device.stop(device.containers, container: item.id)
        }
        ui.Button("Restart", icon: .rotate_cw) {
            on press: device.restart(device.containers, container: item.id)
        }
        ui.Button("Pause", icon: .pause) {
            on press: device.pause(device.containers, container: item.id)
        }
    } else if item.state == .paused {
        ui.Button("Resume", icon: .play) {
            on press: device.unpause(device.containers, container: item.id)
        }
        ui.Button("Stop", icon: .square) {
            on press: device.stop(device.containers, container: item.id)
        }
    } else {
        ui.Button("Start", icon: .play, role: .primary) {
            on press: device.start(device.containers, container: item.id)
        }
    }
}

/// A container's recent log, each line in the ink of what it says.
view logs_pane(item: device.Container) {
    if let error = item.logs_error {
        ui.StatusLine(error, status: .error)
    } else if item.logs.is_empty() {
        ui.Text("This container hasn't written anything yet.", role: .caption, secondary: true)
    } else {
        scroll {
            stack(gap: .none) {
                for line in item.logs {
                    row(gap: .tight, align: .center) {
                        if trouble_line(line) {
                            ui.Icon(
                                .triangle_alert,
                                size: .small,
                                tone: .danger,
                                description: "Error",
                            )
                        } else if warn_line(line) {
                            ui.Icon(
                                .triangle_alert,
                                size: .small,
                                tone: .warning,
                                description: "Warning",
                            )
                        }
                        ui.Text(line, role: .code)
                    }
                }
            }
        }
    }
}

/// How much of the processor and memory a container uses, and what it read and wrote.
view stats_pane(item: device.Container) {
    if item.state != .running and item.state != .restarting {
        ui.Text("Start the container to see what it uses.", role: .caption, secondary: true)
    } else if not item.stats.available {
        ui.Text("Podman returned no sample for this container.", role: .caption, secondary: true)
    } else {
        ui.Section("Usage") {
            ui.ProgressBar(value: item.stats.cpu_percent, label: "Processor")
            ui.Text(
                "{item.stats.cpu_percent * 100.0}% of one processor",
                role: .caption,
                secondary: true,
            )
            ui.ProgressBar(value: mem_fraction(item.stats), label: "Memory")
            if item.stats.mem_limit_bytes > 0 {
                ui.Text(
                    "{size_words(item.stats.mem_bytes)} of {size_words(item.stats.mem_limit_bytes)}",
                    role: .caption,
                    secondary: true,
                )
            } else {
                ui.Text("{size_words(item.stats.mem_bytes)}", role: .caption, secondary: true)
            }
            ui.PropertyList {
                ui.Property("Processes", "{item.stats.pids}")
            }
        }
        ui.Section("Activity") {
            row(gap: .related, wrap: true) {
                ui.StatTile("Received", size_words(item.stats.net_rx_bytes))
                ui.StatTile("Sent", size_words(item.stats.net_tx_bytes))
                ui.StatTile("Read from disk", size_words(item.stats.block_read_bytes))
                ui.StatTile("Written to disk", size_words(item.stats.block_write_bytes))
            }
        }
    }
}

/// What a container is: its id, command, creation, ports, mounts, environment and labels.
view inspect_pane(item: device.Container) {
    ui.Section("Details") {
        ui.PropertyList {
            ui.Property("ID", item.id.slice(0, 12), copyable: true)
            ui.Property("Image", item.image)
            ui.Property("Command", item.command)
            ui.Property("Created", item.created)
            ui.Property("State", item.status_detail)
        }
    }
    if not item.ports.is_empty() {
        ui.Section("Ports") {
            ui.PropertyList {
                for port in item.ports {
                    ui.Property("{port.container_port}/{port.protocol}", "{port.host_port}")
                }
            }
        }
    }
    if not item.mounts.is_empty() {
        ui.Section("Mounts") {
            ui.PropertyList {
                for mount in item.mounts {
                    if mount.read_only {
                        ui.Property(mount.destination, "{mount.source} (read-only)", copyable: true)
                    } else {
                        ui.Property(mount.destination, mount.source, copyable: true)
                    }
                }
            }
        }
    }
    if not item.env.is_empty() {
        ui.Section("Environment") {
            ui.PropertyList {
                for entry in item.env {
                    ui.Property(entry.name, entry.value)
                }
            }
        }
    }
    if not item.labels.is_empty() {
        ui.Section("Labels") {
            ui.PropertyList {
                for label in item.labels {
                    ui.Property(label.name, label.value)
                }
            }
        }
    }
}

/// The terminal tab: a shell in the container, or the start that makes one possible.
view terminal_pane(item: device.Container) {
    if item.state == .running or item.state == .restarting {
        ui.Text(
            "Opens /bin/sh inside the container, in a terminal window.",
            role: .caption,
            secondary: true,
        )
        ui.Button("Open a shell", icon: .terminal) {
            on press: open_shell(item.id)
        }
    } else {
        ui.Text("Start the container to open a shell in it.", role: .caption, secondary: true)
        ui.Button("Start", icon: .play) {
            on press: device.start(device.containers, container: item.id)
        }
    }
}

/// The pull in flight: what it is fetching, all its output, and the control that ends it.
view pull_banner(c: Containers, pull: Pull) {
    ui.Banner(
        "Pulling {pull.image}",
        message: if pull.failed != "" {
            pull.failed
        } else if pull.job == "" {
            "The image is ready."
        } else { "The engine is fetching the image." },
        kind: if pull.failed != "" { .error } else { .info },
        icon: .download,
        action: if pull.job != "" { "Stop this pull" } else { "Close" },
    ) {
        on act: if pull.job != "" { cancel_pull(c, pull) } else { dismiss_pull(c, pull) }
    }
    if not pull.lines.is_empty() {
        scroll {
            stack(gap: .none) {
                for line in pull.lines {
                    ui.Text(line, role: .code, secondary: true)
                }
            }
        }
    }
}

/// The images, each with what it can do: pull it again, tag it, remove it.
view images_page(c: Containers, engine_now: device.Containers, prompt: State<Text>) {
    if engine_now.images.is_empty() {
        ui.EmptyState(
            "No images",
            message: "Pull an image to have something to run.",
            icon: .layers,
        )
    } else {
        ui.Text(
            "{engine_now.images.len()} images holding {size_words(image_bytes(engine_now.images, 0))}",
            role: .caption,
            secondary: true,
        )
        stack(gap: .none) {
            for image in engine_now.images {
                ui.ListRow(
                    reference_of(image),
                    secondary: used_by(image.in_use_by),
                    meta: size_words(image.size_bytes),
                    icon: .layers,
                ) {
                    trailing: {
                        row(gap: .tight) {
                            ui.Button("Pull again", size: .small) {
                                on press: begin_pull(c, reference_of(image))
                            }
                            ui.Button("Add a tag", size: .small) {
                                on press: set(prompt, "tag:{reference_of(image)}")
                            }
                            ui.Button(
                                "Remove",
                                icon: .trash,
                                icon_only: true,
                                role: .destructive,
                                size: .small,
                            ) {
                                on press: set(prompt, "remove-image:{reference_of(image)}")
                            }
                        }
                    }
                }
            }
        }
    }
    row(gap: .tight) {
        ui.Button("Pull an image", icon: .download, role: .primary) {
            on press: set(prompt, "pull")
        } as pull_image
    }
}

/// The volumes, each with what it can do.
view volumes_page(c: Containers, engine_now: device.Containers, prompt: State<Text>) {
    if engine_now.volumes.is_empty() {
        ui.EmptyState(
            "No volumes",
            message: "A volume keeps a container's data across restarts.",
            icon: .hard_drive,
        )
    }
    stack(gap: .none) {
        for volume in engine_now.volumes {
            ui.ListRow(
                volume.name,
                secondary: "{volume.driver}, {used_by(volume.in_use_by)}",
                meta: volume.mountpoint,
                icon: .hard_drive,
            ) {
                trailing: {
                    ui.Button(
                        "Remove",
                        icon: .trash,
                        icon_only: true,
                        role: .destructive,
                        size: .small,
                    ) {
                        on press: set(prompt, "remove-volume:{volume.name}")
                    }
                }
            }
        }
    }
    row(gap: .tight) {
        ui.Button("New volume", icon: .plus, role: .primary) {
            on press: set(prompt, "volume")
        }
    }
}

/// The distroboxes on this computer, each one press from its shell.
view distrobox_page(engine_now: device.Containers) {
    if engine_now.distroboxes.is_empty() {
        ui.EmptyState(
            "No distroboxes",
            message: "Distrobox runs another distribution's tools in a container.",
            icon: .terminal,
        )
    }
    stack(gap: .none) {
        for box in engine_now.distroboxes {
            ui.ListRow(
                box.name,
                secondary: box.image,
                value: state_words(box.state),
                icon: .terminal,
            ) {
                trailing: {
                    ui.Button("Enter", icon: .terminal, size: .small) {
                        on press: enter_box(box.name)
                    }
                }
            }
        }
    }
}

/// The new-container form: one field per part of the draft, with the images this computer holds
/// offered in the image field.
view create_form(c: Containers, open: State<Bool>) {
    if open {
        ui.ModalDialog(
            title: "New container",
            description: .some("Runs an image as a container on this computer."),
            open: true,
            primary: "Create",
            secondary: "Cancel",
        ) {
            body: {
                form {
                    field("Image") {
                        ui.Autocomplete(
                            bind: c.draft.image,
                            placeholder: "docker.io/library/nginx:alpine",
                        ) {
                            for image in device.containers.images {
                                ui.Suggestion(reference_of(image), label: image.repository)
                            }
                        }
                    }
                    field("Name", help: "Empty for a name podman picks.") {
                        ui.TextInput(bind: c.draft.name, placeholder: "my-service")
                    }
                    field("Ports", help: "Host and container port, separated by commas.") {
                        ui.TextInput(bind: c.draft.ports, placeholder: "8080:80, 8443:443")
                    }
                    field("Environment", help: "KEY=value pairs, separated by commas.") {
                        ui.TextInput(bind: c.draft.env, placeholder: "TZ=UTC, LOG_LEVEL=info")
                    }
                    field("Volumes", help: "Volume and path, separated by commas.") {
                        ui.TextInput(
                            bind: c.draft.volumes,
                            placeholder: "pgdata:/var/lib/postgresql/data",
                        )
                    }
                    field("Command", help: "Empty to run the image's default.") {
                        ui.TextInput(bind: c.draft.command)
                    }
                }
            }
            on act_primary: create_container(c).then(set(open, false))
            on act_secondary: set(open, false)
            on dismiss: set(open, false)
        }
    }
}

/// What the prompt asks for: a new volume, a tag for an image, an image to pull, or the removal
/// of an image or volume.
fn prompt_title(prompt: Text) -> Text {
    if prompt == "volume" {
        std.say("New volume")
    } else if prompt == "pull" {
        std.say("Pull an image")
    } else if prompt.starts_with("tag:") {
        std.say("Add a tag to {prompt.slice(4, prompt.len())}")
    } else if prompt.starts_with("remove-image:") {
        std.say("Remove {prompt.slice(13, prompt.len())}?")
    } else if prompt.starts_with("remove-volume:") {
        std.say("Remove {prompt.slice(14, prompt.len())}?")
    } else { "" }
}

fn prompt_label(prompt: Text) -> Text {
    if prompt == "volume" {
        std.say("The volume's name")
    } else if prompt == "pull" {
        std.say("The image to pull")
    } else if prompt.starts_with("tag:") { std.say("The new tag") } else { "" }
}

fn prompt_primary(prompt: Text) -> Text {
    if prompt == "volume" {
        std.say("Create volume")
    } else if prompt == "pull" {
        std.say("Pull image")
    } else if prompt.starts_with("tag:") {
        std.say("Add tag")
    } else if prompt.starts_with("remove-") { std.say("Remove") } else { std.say("OK") }
}

/// The prompt's dialog: one field for what it asks, and the verb that does it.
view prompt_dialog(prompt: State<Text>, typed: State<Text>) {
    if prompt != "" {
        ui.ModalDialog(
            title: prompt_title(prompt),
            open: true,
            primary: prompt_primary(prompt),
            secondary: "Cancel",
            destructive: prompt.starts_with("remove-"),
        ) {
            body: {
                if prompt_label(prompt) != "" {
                    ui.TextInput(bind: typed, placeholder: prompt_label(prompt), autofocus: true)
                }
                if prompt.starts_with("remove-image:") {
                    ui.Text(
                        "The image is removed from this computer. Containers using it keep running.",
                    )
                } else if prompt.starts_with("remove-volume:") {
                    ui.Text("The volume and everything it kept are removed.")
                }
            }
            on act_primary: prompt_run(prompt, typed).then(set(prompt, ""))
            on act_secondary: set(prompt, "")
            on dismiss: set(prompt, "")
        }
    }
}

/// What a removal confirms.
fn confirm_title(confirm: Text) -> Text {
    if confirm.starts_with("container:") {
        std.say("Remove this container?")
    } else if confirm.starts_with("image:") {
        std.say("Remove this image?")
    } else { std.say("Remove this volume?") }
}

/// The confirmation a removal asks for: the container, image or volume, and the two answers.
view confirm_dialog(confirm: State<Text>) {
    if confirm != "" {
        ui.ModalDialog(
            title: confirm_title(confirm),
            open: true,
            primary: "Remove",
            secondary: "Cancel",
            destructive: true,
        ) {
            body: {
                if confirm.starts_with("container:") {
                    ui.Text("The container is removed. It is gone for good.")
                } else if confirm.starts_with("image:") {
                    ui.Text(
                        "The image is removed from this computer. Containers using it keep running.",
                    )
                } else {
                    ui.Text("The volume and everything it kept are removed.")
                }
            }
            on act_primary: confirm_run(confirm).then(set(confirm, ""))
            on act_secondary: set(confirm, "")
            on dismiss: set(confirm, "")
        }
    }
}
