app so.tilo.publisher "App Publisher" (
    icon: .upload,
    category: .utility,
    tagline: "Signs your apps and proves their namespaces",
    keywords: ["publish", "release", "store upload", "app release", "submit app", "package", "signing", "deploy", "distribute", "developer", "veröffentlichen", "veroeffentlichen", "bereitstellen", "paketieren", "signieren", "entwickler", "app einreichen"],
    splash_budget: 250,
)

op "key-state"
op "create"
op "key-file"
op "apply" leaves
op "status"

/// What the registry answered about the application, when it was asked: its state, the namespaces
/// and any reason, with the moment of the check.
type Reading {
    /// When the application was checked.
    at: Time
    /// The registry's own line: the state, the namespaces and any reason.
    summary: Text
}

/// The App Publisher: the key that signs your apps, and the namespaces the registry lets it
/// publish under.
///
/// The registry, the namespaces to claim and the contact belong to the workspace, so every member
/// reads the same application. The key itself never is a field here, or anywhere in the record: it
/// stays in this computer's secret store, and the engine signs with it.
type Publisher (untitled: "App Publisher") {
    /// The registry the key publishes to: credential-free https, or plain http on this machine.
    registry: Text = "https://apps.tilo.so"
    /// A domain whose namespace to claim, such as `example.com`.
    domain: Text
    /// A forge account whose namespace to claim, such as `github:ada`.
    forge_account: Text
    /// How the registry's owner can reach the person about the application.
    contact: Text
    /// This computer's key, as last read: `missing`, `present` or `unreadable`.
    key: Text = ""
    /// The key's public half, as hexadecimal; empty unless it is present.
    public_key: Text = ""
    /// What the last operation said: its kind (`info`, `success` or `error`), its headline and the
    /// detail under it.
    notice_kind: Text = ""
    notice_title: Text = ""
    notice_detail: Text = ""
    /// The registry call under way, by the job this computer gave it; empty when nothing runs.
    job: Text = ""
    /// What the application checks said, newest first.
    readings: Collection<Reading>
}

root publisher: Publisher

default view publisher_window for Publisher

/// The proof line for `public`: the words a person publishes where only they can write.
// ---------------------------------------------------------------- the copy around a value
fn proof_of(public: Text) -> Text {
    if public == "" { "" } else { "tilo-publisher={public.lower()}" }
}

/// The places the registry looks for the proof of the profile's claims: none before a claim names
/// a domain or a forge account.
fn proof_places(p: Publisher) -> List<Text> {
    let out: List<Text> = []
    let domain = p.domain.trim().lower()
    if domain != "" {
        out = [
            out,
            [
                "DNS TXT _tilo-publisher.{domain}",
                "or https://{domain}/.well-known/tilo-publisher.txt",
            ],
        ]
            .flat_map(place -> place)
    }
    let forge = forge_parts(p.forge_account)
    if let parts = forge {
        out = [out, ["https://{parts.host}/{parts.account}/tilo-publisher"]]
            .flat_map(place -> place)
    }
    out
}

/// A forge account as the registry reads one: the host and the account, its name lowercased.
type ForgeParts {
    host: Text
    account: Text
}

/// The forge and account `text` names (`github:ada`), or nothing when it is no account there.
fn forge_parts(text: Text) -> Option<ForgeParts> {
    let said = text.trim().lower()
    let parts = said.split(":")
    if parts.len() < 2 {
        return .none
    }
    let account = parts.last() ?? ""
    if not label_ok(account) {
        return .none
    }
    let host = parts.take(parts.len() - 1).join(":")
    if host == "github" or host == "github.com" {
        .some(ForgeParts(host: "github.com", account: account))
    } else if host == "codeberg" or host == "codeberg.org" {
        .some(ForgeParts(host: "codeberg.org", account: account))
    } else if host.contains(".") and not host.contains(" ") {
        .some(ForgeParts(host: host, account: account))
    } else { .none }
}

/// Whether `name` is an account name a namespace can hold: lowercase letters, digits, `-` and `_`,
/// starting with a letter or a digit.
fn label_ok(name: Text) -> Bool {
    if name == "" or name.len() > 63 {
        return false
    }
    if name.starts_with("-") or name.starts_with("_") {
        return false
    }
    not name.contains(" ")
        and not name.contains(".")
        and not name.contains(":")
        and not name.contains("/")
}

/// Whether `address` is a registry this computer may publish to: credential-free https, or plain
/// http on this machine alone. The engine refuses the same addresses, so what the window says and
/// what a call does cannot drift.
fn registry_ok(address: Text) -> Bool {
    let said = address.trim()
    if said == "" or said.contains(" ") or said.contains("@") {
        return false
    }
    if said.starts_with("https://") {
        return host_of(authority(said.slice(8, said.len()))) != ""
    }
    if said.starts_with("http://") {
        return loopback(authority(said.slice(7, said.len())))
    }
    false
}

/// The host part of an authority: what stands before its port, if any.
fn host_of(authority: Text) -> Text {
    if authority.starts_with("[") {
        return authority.split("]").first() ?? ""
    }
    authority.split(":").first() ?? ""
}

/// The host and port part of an address: what stands before its first slash.
fn authority(rest: Text) -> Text {
    rest.split("/").first() ?? ""
}

/// Whether `host` names this machine: `localhost`, 127.0.0.0/8, or `[::1]`.
fn loopback(host: Text) -> Bool {
    if host == "" {
        return false
    }
    let name = if host.starts_with("[") {
        host.slice(1, host.len()).split("]").first() ?? ""
    } else { host.split(":").first() ?? "" }
    if name == "::1" {
        return true
    }
    let trimmed = if name.ends_with(".") { name.slice(0, name.len() - 1) } else { name }
    if trimmed == "localhost" or trimmed.ends_with(".localhost") {
        return true
    }
    ipv4_loopback(trimmed)
}

/// Whether `text` is a dotted IPv4 address in 127.0.0.0/8: `127.0.0.1`, `127.1.2.3`, but not a
/// name that merely starts with the same characters.
fn ipv4_loopback(text: Text) -> Bool {
    let parts = text.split(".")
    if parts.len() != 4 or (parts.first() ?? "") != "127" {
        return false
    }
    parts.all(part -> octet_ok(part))
}

/// Whether `part` is one dotted octet of an IPv4 address: digits from 0 to 255.
fn octet_ok(part: Text) -> Bool {
    if part == "" or part.len() > 3 {
        return false
    }
    if not part.split("").all(ch -> "0123456789".contains(ch)) {
        return false
    }
    if part.len() > 1 and part.starts_with("0") {
        return false
    }
    (part.to_int() ?? 300) <= 255
}

/// Reads this computer's publisher key: whether it has one, and its public half. Opening the app
/// reads it, so the window shows the key's state without anything to press.
// ---------------------------------------------------------------- what the window says
// ---------------------------------------------------------------- this computer's key
action read_key(p: Publisher) {
    match engine.call("key-state", "") {
        .err(why) => {
            p.key = "unreadable"
            p.public_key = ""
            p.notice_kind = "error"
            p.notice_detail = why
        }
        .ok(text) => match json.decode<KeyState>(text) {
            .err(why) => {
                p.key = "unreadable"
                p.public_key = ""
                p.notice_kind = "error"
                p.notice_detail = why
            }
            .ok(state) => {
                p.key = state.state
                p.public_key = state.public
            }
        },
    }
}

/// Opening the app reads this computer's key.
opened action open_key(p: Publisher) {
    read_key(p)
}

/// Makes this computer's publisher key. It signs the person's apps and their requests to the
/// registry, and a second one is never made: a publisher key is one per machine.
action create_key(p: Publisher) {
    match engine.call("create", "") {
        .err(why) => {
            p.notice_kind = "error"
            p.notice_detail = why
        }
        .ok(text) => match json.decode<KeyState>(text) {
            .err(why) => {
                p.notice_kind = "error"
                p.notice_detail = why
            }
            .ok(state) => {
                p.key = state.state
                p.public_key = state.public
                let public = state.public
                p.notice_kind = "success"
                p.notice_title = std.say("Created your publisher key")
                p.notice_detail = std.say(
                    "Back up publisher-key.json: it is the only copy. Public key {public}.",
                )
            }
        },
    }
}

/// Copies the proof line: the words to publish where only the person can write.
action copy_proof(p: Publisher) uses clipboard {
    let proof = proof_of(p.public_key)
    if proof == "" {
        p.notice_kind = "error"
        p.notice_detail = std.say("This computer has no publisher key yet.")
        return
    }
    clipboard.copy(proof)
    p.notice_kind = "info"
    p.notice_title = std.say("Copied the proof line.")
    p.notice_detail = proof
}

/// Copies the key file, for the person's CI: one secret, `TILO_PUBLISHER_KEY`, holding it.
///
/// The key is copied as a secret, so histories and other devices do not keep it.
action copy_secret(p: Publisher) uses clipboard {
    match export_key_text(p) {
        .err(why) => {
            p.notice_kind = "error"
            p.notice_detail = why
        }
        .ok(key) => {
            clipboard.copy_secret(key.text)
            p.notice_kind = "success"
            p.notice_title = std.say("Copied the key")
            p.notice_detail = std.say(
                "Paste it into your CI’s TILO_PUBLISHER_KEY secret. The clipboard clears it in 45 seconds.",
            )
        }
    }
}

/// The key file as text, for a copy or an export; never a field.
type KeyFile {
    text: Text
}

/// This computer's key, as `tilolang keys export` writes it.
fn export_key_text(p: Publisher) -> Result<KeyFile, Text> {
    match engine.call("key-file", "") {
        .err(why) => .err(why),
        .ok(text) => match json.decode<KeyFile>(text) {
            .err(why) => .err(why),
            .ok(file) => .ok(file),
        },
    }
}

/// The key file, for the person to keep somewhere safe: what they restore the key from.
action export_key(p: Publisher) -> File (icon: .download, label: "Export key…") {
    match export_key_text(p) {
        .err(why) => {
            p.notice_kind = "error"
            p.notice_detail = why
            File.of_text("The key could not be read.txt", why)
        }
        .ok(key) => {
            File.of_text("publisher-key.json", key.text)
        }
    }
}

/// The key file's save is over: the window says where it went, that the person closed the
/// chooser, or why it was refused.
action exported(p: Publisher, saved: host.Saved (label: "Save result")) {
    if saved.refused != "" {
        p.notice_kind = "error"
        p.notice_title = std.say("The key could not be saved")
        p.notice_detail = saved.refused
    } else if saved.cancelled {
        p.notice_kind = "info"
        p.notice_title = std.say("Export cancelled")
        p.notice_detail = ""
    } else if saved.at != "" {
        p.notice_kind = "success"
        p.notice_title = std.say("Exported your publisher key")
        p.notice_detail = std.say(
            "Kept at {saved.at}. It is the only copy outside this device: keep it safe.",
        )
    } else {
        p.notice_kind = "success"
        p.notice_title = std.say("Exported your publisher key")
        p.notice_detail = std.say(
            "Keep publisher-key.json somewhere safe; it is the only copy outside this device.",
        )
    }
}

/// The namespaces the profile claims, as the engine reads them: one application for all of them.
// ---------------------------------------------------------------- the application
type ApplyAsk {
    registry: Text
    domain: Text
    forge_account: Text
    contact: Text
}

/// The registry whose application is read.
type StatusAsk {
    registry: Text
}

/// The registry's own line about the application, once its call ended.
type RegistryWords {
    summary: Text = ""
}

/// This computer's key, as the engine reads it.
type KeyState {
    state: Text
    public: Text
}

/// Asks the registry for the namespaces the profile's domain and forge account name.
primary action apply_for_namespaces(p: Publisher) (
    icon: .send,
    label: "Apply",
    did: "applied for namespaces",
) {
    require p.job == "" "The registry has not answered yet"
    require registry_ok(p.registry) "A registry is reached over credential-free https, or plain http on this machine alone"
    p.notice_kind = ""
    p.notice_title = ""
    p.notice_detail = ""
    let ask = ApplyAsk(
        registry: p.registry,
        domain: p.domain,
        forge_account: p.forge_account,
        contact: p.contact,
    )
    match engine.start("apply", json.encode(ask), done: j -> answered(p, j)) {
        .err(why) => {
            p.notice_kind = "error"
            p.notice_detail = why
        }
        .ok(job) => {
            p.job = job
        }
    }
}

/// Reads what the registry holds about this computer's key's application.
action check_status(p: Publisher) (
    icon: .refresh_ccw,
    label: "Check application",
    did: "checked the application",
) {
    require p.job == "" "The registry has not answered yet"
    require registry_ok(p.registry) "A registry is reached over credential-free https, or plain http on this machine alone"
    p.notice_kind = ""
    p.notice_title = ""
    p.notice_detail = ""
    match engine.start(
        "status",
        json.encode(StatusAsk(registry: p.registry)),
        done: j -> answered(p, j),
    ) {
        .err(why) => {
            p.notice_kind = "error"
            p.notice_detail = why
        }
        .ok(job) => {
            p.job = job
        }
    }
}

/// The registry answered: what it said about the application, or why it could not.
action answered(p: Publisher, j: host.Job) uses time {
    p.job = ""
    if j.state == .done {
        match json.decode<RegistryWords>(j.detail) {
            .err(why) => {
                p.notice_kind = "error"
                p.notice_detail = why
            }
            .ok(words) => {
                if words.summary == "" {
                    p.notice_kind = "info"
                    p.notice_title = std.say("No application yet")
                    p.notice_detail = std.say("This registry has no application from this key yet.")
                } else {
                    p.notice_kind = "info"
                    p.notice_title = std.say("Application status")
                    p.notice_detail = words.summary
                    p.readings.insert(Reading(at: now, summary: words.summary))
                    let all = p.readings.sort_by(r -> r.at, desc)
                    if all.len() > 5 {
                        if let cut = all.take(5).last() {
                            let gone = all.filter(r -> r.at < cut.at)
                            for r in gone {
                                p.readings.remove(r)
                            }
                        }
                    }
                }
            }
        }
    } else if j.state == .failed {
        p.notice_kind = "error"
        p.notice_detail = j.error
    } else if j.state == .cancelled {
        p.notice_kind = "info"
        p.notice_title = std.say("The registry call was cancelled.")
        p.notice_detail = ""
    }
}

/// The window: this computer's key and the proof line, where the proof goes, the namespaces to
/// claim, and what the registry said.
// ---------------------------------------------------------------- the window
view publisher_window(p: Publisher) {
    std.page(p) {
        content: {
            key_section(p)
            proof_places_section(p)
            namespaces_section(p)
            apply_row(p)
            status_section(p)
            if p.notice_kind != "" {
                ui.Banner(
                    if p.notice_kind == "error" { "That didn’t work" } else { p.notice_title },
                    message: .some(p.notice_detail),
                    kind: if p.notice_kind == "success" {
                        .success
                    } else if p.notice_kind == "error" { .error } else { .info },
                )
            }
        }
    }
}

/// This computer's key: its public half and the proof line, the ways to copy and keep it, or what
/// still has to be done before there is one.
view key_section(p: Publisher) {
    ui.Section(title: "Signing key") {
        if p.key == "present" {
            let proof = proof_of(p.public_key)
            ui.PropertyList {
                ui.Property("Public key", p.public_key, copyable: true)
                ui.Property("Proof line", proof, copyable: true)
            }
            ui.Text(
                "Publish this line where only you can write, to prove a namespace.",
                role: .caption,
                secondary: true,
            )
            row(gap: .related) {
                ui.Button("Copy proof line", icon: .copy) {
                    on press: copy_proof(p)
                }
                ui.Button("Copy key for CI", icon: .key) {
                    on press: copy_secret(p)
                }
                ui.Button("Export key…", icon: .download) {
                    on press: file.save(
                        export_key(p),
                        name: "publisher-key.json",
                        then: saved -> exported(p, saved),
                    )
                }
                ui.Button("Check application", icon: .refresh_ccw) {
                    on press: check_status(p)
                }
            }
        } else if p.key == "missing" {
            ui.Text(
                "This computer has no publisher key yet. It signs your apps and your requests to the registry.",
            )
            ui.Button("Create key", icon: .key, role: .primary) {
                on press: create_key(p)
            }
        } else if p.key == "unreadable" {
            stack(gap: .group) {
                ui.Banner(
                    "The publisher key can’t be read",
                    message: .some(
                        "publisher-key.json is not a valid publisher key. Restore it from a backup, or move it aside and create a new key.",
                    ),
                    kind: .error,
                )
            }
        } else {
            ui.Text("Reading this computer’s key…", role: .caption, secondary: true)
        }
    }
}

/// Every place the registry looks for the proof of the profile's claims.
view proof_places_section(p: Publisher) {
    let places = proof_places(p)
    ui.Section(title: "Where to publish the proof") {
        if places.is_empty() {
            ui.Text(
                "Name a domain or a forge account to see where its proof goes.",
                role: .caption,
                secondary: true,
            )
        } else {
            ui.Text(
                "Put the proof line where only you can write it, then apply.",
                role: .caption,
                secondary: true,
            )
            ui.PropertyList {
                for placed in places {
                    ui.Property("Publish at", placed, copyable: true)
                }
            }
        }
    }
}

/// What to apply for: the namespaces, how to reach the person about them, and the registry.
view namespaces_section(p: Publisher) {
    ui.Section(title: "Namespaces") {
        ui.Text(
            "Claim the domains and forge accounts whose apps you publish.",
            role: .caption,
            secondary: true,
        )
        ui.ControlRow(label: "Domain") {
            ui.TextInput(bind: p.domain, placeholder: "example.com", fill: 1.0)
        }
        ui.Text(
            "Proved with a DNS TXT record or a .well-known file on the domain.",
            role: .caption,
            secondary: true,
        )
        ui.ControlRow(label: "Forge account") {
            ui.TextInput(bind: p.forge_account, placeholder: "github:ada", fill: 1.0)
        }
        ui.Text(
            "Proved with a tilo-publisher repository on that account.",
            role: .caption,
            secondary: true,
        )
        ui.ControlRow(label: "Contact") {
            ui.TextInput(bind: p.contact, placeholder: "you@example.com", fill: 1.0)
        }
        ui.Text(
            "How the registry’s owner can reach you about the application.",
            role: .caption,
            secondary: true,
        )
        ui.ControlRow(label: "Registry") {
            ui.TextInput(bind: p.registry, placeholder: "https://apps.tilo.so", fill: 1.0)
        }
        if not registry_ok(p.registry) {
            ui.Text(
                "A registry is reached over credential-free https, or plain http on this machine alone.",
                role: .caption,
                secondary: true,
            )
        }
    }
}

/// The one thing to do with the claims: apply, or say that the registry is still answering.
view apply_row(p: Publisher) {
    row(gap: .related, align: .center) {
        ui.Button("Apply", icon: .send, role: .primary) {
            on press: apply_for_namespaces(p)
        }
        if p.job != "" {
            ui.Text("Waiting for the registry…", role: .caption, secondary: true)
        }
    }
}

/// The application as the registry answered it, newest first.
view status_section(p: Publisher) {
    ui.Section(title: "Application status") {
        let readings = p.readings.sort_by(r -> r.at, desc)
        if readings.is_empty() {
            ui.Text(
                "No application status has been checked here yet.",
                role: .caption,
                secondary: true,
            )
        } else {
            for r in readings {
                ui.ListRow(title: r.summary, meta: r.at.iso(), icon: .badge_check)
            }
        }
    }
}

// ---------------------------------------------------------------- what a first run shows
scenario "no key yet" {
    let p = root Publisher
    p.domain = "lovelace.dev"
    p.forge_account = "codeberg:ada"
    p.contact = "ada@lovelace.dev"
}

/// A key on this computer, with its public half and proof line.
scenario "a key on this computer" {
    let p = root Publisher
    p.key = "present"
    p.public_key = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"
    p.domain = "lovelace.dev"
    p.forge_account = "codeberg:ada"
    p.contact = "ada@lovelace.dev"
}

/// A key on this computer that cannot be read.
scenario "an unreadable key" {
    let p = root Publisher
    p.key = "unreadable"
}

/// The proof line copied, as the window says once it is.
scenario "a copied proof line" {
    let p = root Publisher
    p.key = "present"
    p.public_key = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"
    p.notice_kind = "info"
    p.notice_title = "Copied the proof line."
    p.notice_detail = proof_of(p.public_key)
}

/// An application the registry approved.
scenario "an approved application" {
    let p = root Publisher
    p.key = "present"
    p.public_key = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"
    p.domain = "lovelace.dev"
    p.contact = "ada@lovelace.dev"
    p.notice_kind = "info"
    p.notice_title = "Application status"
    p.notice_detail = "approved (dev.lovelace)"
    p.readings.insert(Reading(at: now, summary: "approved (dev.lovelace)"))
}

/// An application the registry refused.
scenario "a failed application" {
    let p = root Publisher
    p.key = "present"
    p.public_key = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"
    p.domain = "lovelace.dev"
    p.notice_kind = "error"
    p.notice_title = "That didn’t work"
    p.notice_detail = "missing proof at DNS TXT _tilo-publisher.lovelace.dev"
}
