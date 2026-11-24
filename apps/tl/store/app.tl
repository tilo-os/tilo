app so.tilo.store "App Store" (
    icon: .store,
    category: .system,
    tagline: "The apps of your workspace, and what they run on this computer",
    keywords: ["store", "apps", "install", "update", "uninstall", "get", "flatpak", "registry", "repositories", "bots"],
)

use so.tilo.session as session

/// Where an app in the catalogue comes from.
enum Origin {
    /// A Tilo registry.
    tilo (icon: .package)
    /// Flathub.
    flathub (icon: .box)
    /// A Flatpak repository this workspace or computer configured.
    remote (icon: .server)
    /// A package file someone handed this app.
    local (icon: .package)
}

/// What a source's review of an app comes to.
enum Trust {
    /// Its publisher's signature and the registry's review.
    publisher_signed (icon: .shield_check, tone: .success)
    /// Flathub, which Tilo has not reviewed itself.
    flathub (icon: .box, tone: .neutral)
    /// A repository this workspace configured, which Tilo has not reviewed.
    configured_remote (icon: .server, tone: .warning)
    /// A package verified by an attestor this computer already trusts.
    attestor (icon: .shield_check, tone: .info)
}

/// One signed version of an app, as its source offers it.
type Version {
    /// The version, as its source writes it.
    version: Text
    /// The day its source published it.
    released: Text = ""
    /// The signed package root, hex.
    root: Text = ""
    /// The signature the package itself carries, hex.
    surface: Text = ""
    /// Its bytes, as its source states them.
    size: Int = 0
    /// The runtime reference it needs, when it names one.
    runtime: Text = ""
    /// Where its package file downloads from, when its source names a place.
    download: Text = ""
}

/// One thing an app can do on this computer, as its source says.
type Permission {
    /// What it asks for, in the source's own words: `network`, `files:all`.
    what: Text
    /// Whether the request reaches everything rather than what a person chooses.
    broad: Bool = false
}

/// What changed in one version of an app, as its source says.
type Release {
    version: Text
    notes: Text = ""
}

/// One app the catalogue knows: what it is, where it comes from, what it offers, and how far it
/// is on this workspace's devices.
type App (title: name) {
    /// The app's identity: its package id.
    identity: Text
    name: Text
    /// The one line a row shows, the tagline or else the description's first line.
    summary: Text = ""
    description: Text = ""
    developer: Text = ""
    license: Text = ""
    categories: Set<Text>
    /// Its icon, as a web address the device fetches.
    icon: Text = ""
    screenshots: List<Text>
    homepage: Text = ""
    source_code: Text = ""
    origin: Text = "tilo"
    /// The registry or repository it came through, as people read it.
    registry: Text = ""
    /// The branch a Flatpak repository builds it in.
    branch: Text = "stable"
    /// Whether this workspace may change a repository app: a computer's own Flatpak repository
    /// is its administrator's.
    user_managed: Bool = true
    trust: Text = "publisher_signed"
    versions: Collection<Version>
    /// The versions its source offers, newest first, as text.
    available: Text = ""
    /// The version this workspace runs, empty when it runs none.
    installed: Text = ""
    /// Whether this workspace keeps the version it runs.
    pinned: Bool = false
    /// The version this computer has fetched for the workspace.
    fetched: Text = ""
    /// Whether its source builds it for this computer. An app only built for other processors is
    /// still found, said to be unavailable here and why, and never offered for installing.
    runnable: Bool = true
    /// The processors it is built for, in the source's words, when this computer is not one.
    processors: Text = ""
    /// Its builds all speak an older interface than this system serves.
    awaiting_developer: Bool = false
    /// Its place in its source's featured list, smaller first; 0 when it is not featured.
    featured: Int = 0
    /// The day its source last changed it.
    updated: Text = ""
    publisher: Text = ""
    /// The digest of the package this computer fetched, hex.
    digest: Text = ""
    /// A directory bot: what it does and needs, and no version to fetch on this computer.
    bot: Bool = false
    bot_triggers: Set<Text>
    bot_apps: Set<Text>
    /// Its connectors, each as "name, version or newer".
    bot_connectors: Set<Text>
    bot_appearance: Bool = false
    /// What it asks for, once its page read its source's details.
    permissions: Collection<Permission>
    releases: Collection<Release>
    /// The bytes its source downloads, when it says.
    size: Int = 0
    /// The `.flatpakref` it was opened from, when it was.
    reference: Text = ""
    /// The runtime it needs, and the repository that offers it.
    runtime: Text = ""
    runtime_registry: Text = ""
    /// The change running for it, by the engine's job id.
    job: Text = ""
    /// Whether its page read what its source says about it.
    details_read: Bool = false
    /// Whether a source offers a newer version than this computer runs.
    updatable: Bool = false
    /// Why this computer cannot change it, in the source's words; empty when it can.
    trouble: Text = ""

    search {
        filter "Discover" = not bot and installed == "" (icon: .store)
        filter "Updates" = updatable (icon: .arrow_down)
        filter "On this device" = installed != "" (icon: .circle_check)
        filter "Bots" = bot (icon: .bot)
        filter categories
        hide available
        hide fetched
        hide featured
        hide job
        hide updated
        hide publisher
        hide digest
        hide reference
        hide runtime
        hide runtime_registry
        hide trust
        hide user_managed
        hide branch
        hide awaiting_developer
        hide runnable
        hide processors
        hide versions
        hide permissions
        hide releases
        hide bot_triggers
        hide bot_apps
        hide bot_connectors
        hide bot_appearance
    }
}

/// A Flatpak repository, as this computer and this workspace offer it.
type Remote (title: title) {
    name: Text
    title: Text
    url: Text = ""
    /// "device" or "account": whose repository it is.
    scope: Text = "account"
    enabled: Bool = true
    /// Why its catalogue could not be read, when it could not.
    trouble: Text = ""
    /// How many apps its catalogue holds, once it was read.
    count: Int = 0
    /// Whether its apps are loaded.
    loaded: Bool = false
}

/// Someone's block of one app. A blocked app appears nowhere in the Store or in search, and
/// cannot be opened or installed from them.
type Block (readers: author) {
    /// The app's identity.
    identity: Text
}

/// A report about an app: a draft this workspace shares until someone submits it, then the
/// registry's case and its status.
type Report (title: identity) {
    /// The app's identity.
    identity: Text
    /// What the person saw, as evidence another reviewer can check.
    note: Text = ""
    /// The registry's status: "Not submitted" until it answers.
    status: Text = "Not submitted"
    /// The registry's case id, once there is one.
    case: Text = ""
    /// The signed operation the registry accepted, so a refresh or an appeal names exactly it.
    operation: Text = ""
    /// The registry that holds the case.
    registry: Text = ""
}

/// The workspace's App Store: the catalogue its sources offer, the repositories it configured,
/// the reports people made, and each person's own blocks.
type Store {
    apps: Collection<App>
    remotes: Collection<Remote>
    reports: Collection<Report>
    blocks: Collection<Block>
    /// What the last read of the sources came to.
    status: Text = ""
    /// Why the last read of the sources failed, when it did.
    trouble: Text = ""
    /// The read running now, by the engine's job id.
    reading: Text = ""
    /// Whether this computer has Flatpak.
    flatpak: Bool = false
    /// Why this computer's Flatpak could not be read.
    flatpak_trouble: Text = ""
    /// The enabled registries that did not answer the last read.
    unanswered: Set<Text>
    /// The enabled registries whose catalogue has expired.
    expired: Set<Text>
    /// The package someone handed the app, reviewed but not installed yet: its root, and the
    /// file it was read from.
    reviewed_root: Text = ""
    reviewed_app: Text = ""
    reviewed_file: Option<File> = .none
}

root store: Store

default view catalogue for Store

default view app_page for App

default view remote_page for Remote

default view report_page for Report

row view app_row for App

/// One app from a source, as the engine read it.
type Item {
    identity: Text
    name: Text
    summary: Text = ""
    description: Text = ""
    developer: Text = ""
    license: Text = ""
    categories: List<Text>
    icon: Text = ""
    screenshots: List<Text>
    homepage: Text = ""
    source_code: Text = ""
    origin: Text = "tilo"
    registry: Text = ""
    branch: Text = "stable"
    user_managed: Bool = true
    trust: Text = "publisher_signed"
    /// The versions its source offers, each as its own JSON text.
    versions: List<Text>
    available: Text = ""
    installed: Text = ""
    pinned: Bool = false
    fetched: Text = ""
    runnable: Bool = true
    processors: Text = ""
    awaiting_developer: Bool = false
    featured: Int = 0
    updated: Text = ""
    publisher: Text = ""
    digest: Text = ""
    bot: Bool = false
    bot_triggers: List<Text>
    bot_apps: List<Text>
    bot_connectors: List<Text>
    bot_appearance: Bool = false
    runtime: Text = ""
    updatable: Bool = false
    trouble: Text = ""
}

/// What one read of the sources found.
type Catalogue {
    /// Each app a source read, as its own JSON text.
    items: List<Text>
    /// Each repository, as its own JSON text.
    remotes: List<Text>
    status: Text = ""
    trouble: Text = ""
    flatpak: Bool = false
    flatpak_trouble: Text = ""
    unanswered: List<Text>
    expired: List<Text>
}

/// One repository, as the engine read it.
type RemoteRead {
    name: Text
    title: Text = ""
    url: Text = ""
    scope: Text = "account"
    enabled: Bool = true
    trouble: Text = ""
}

/// What a page's read of one app adds: the source's details.
type Details {
    description: Text = ""
    developer: Text = ""
    license: Text = ""
    homepage: Text = ""
    source_code: Text = ""
    size: Int = 0
    screenshots: List<Text>
    permissions: List<Text>
    releases: List<Text>
    runtime: Text = ""
    runtime_registry: Text = ""
    digest: Text = ""
}

/// A reviewed package, ready to install.
type Review {
    item: Text
    root: Text = ""
    trouble: Text = ""
}

/// One repository's answer about a case.
type Case {
    case: Text = ""
    status: Text = ""
    situation: Text = ""
    trouble: Text = ""
}

/// A report prepared for the registry: the operation it signs, and the registry that holds it.
type Prepared {
    operation: Text = ""
    registry: Text = ""
    trouble: Text = ""
}

/// How far the engine's job has got.
type Job {
    state: Text = ""
    done: Int = 0
    total: Int = 0
    error: Text = ""
    status: Text = ""
}

/// A registry's moderation case as this app keeps it for a refresh.
type CaseRead {
    case: Text = ""
    status: Text = ""
    situation: Text = ""
}

/// Whether `a` is a newer version than `b`: dot-separated numbers, then more parts.
fn newer(a: Text, b: Text) -> Bool {
    if b == "" { a != "" } else if a == "" or a == b { false } else { newer_part(a, b, 0) }
}

fn newer_part(a: Text, b: Text, i: Int) -> Bool {
    let left = a.split(".")
    let right = b.split(".")
    if i >= left.len() and i >= right.len() {
        false
    } else {
        let x = if i < left.len() { (left.at(i) ?? "").to_int() ?? 0 } else { 0 }
        let y = if i < right.len() { (right.at(i) ?? "").to_int() ?? 0 } else { 0 }
        if x != y { x > y } else { newer_part(a, b, i + 1) }
    }
}

/// Whether `a` is blocked by the person looking.
fn blocked(s: Store, a: App) -> Bool {
    s.blocks.any(b -> b.identity == a.identity)
}

/// The apps a place lists out of everything the catalogue holds: what a person browses, never a
/// blocked app, a runtime or a language pack, and never one this computer cannot run.
fn listed(s: Store, place: Filter<App>) -> List<App> {
    s.apps.matching(place).filter(a -> not blocked(s, a) and not hidden(a))
}

/// Everything a place lists, before a person's blocks are read: the filters of a catalogue and
/// the headings over it agree on what an app is.
fn browsable(s: Store) -> List<App> {
    s.apps.filter(a -> not blocked(s, a) and not hidden(a))
}

/// Whether an app is kept out of the catalogue's lists: a runtime or a language pack belongs to
/// the system updater, and an app with no build for this computer is found by search alone.
fn hidden(a: App) -> Bool {
    runtime_package(a.identity) or language_pack(a.identity) or only_other_processors(a)
}

fn runtime_package(identity: Text) -> Bool {
    identity.starts_with("runtime:") or identity.starts_with("so.tilo.runtime")
}

fn language_pack(identity: Text) -> Bool {
    identity.starts_with("lang:")
}

/// Whether its source builds it only for processors this computer is not.
fn only_other_processors(a: App) -> Bool {
    a.installed == "" and not a.runnable
}

/// The one action an app's page offers now.
fn verb_of(a: App, flatpak: Bool) -> Text {
    if a.bot {
        "add-bot"
    } else if a.origin == "tilo" or a.origin == "local" {
        if runtime_package(a.identity) {
            if a.installed != "" { "" } else { "install-runtime" }
        } else if a.installed == "" {
            "get"
        } else if newer(a.available, a.installed) { "update" } else { "open" }
    } else if not flatpak {
        "flatpak-setup"
    } else if a.installed == "" {
        "install"
    } else if newer(a.available, a.installed) and a.user_managed { "update" } else { "open" }
}

fn action_label(a: App, flatpak: Bool) -> Text {
    let verb = verb_of(a, flatpak)
    if verb == "open" {
        "Open"
    } else if verb == "get" {
        "Get"
    } else if verb == "install" {
        "Install"
    } else if verb == "update" {
        "Update"
    } else if verb == "install-runtime" {
        "Install runtime"
    } else if verb == "add-bot" {
        "Add to this workspace…"
    } else if verb == "flatpak-setup" { "Install Flatpak first" } else { "" }
}

/// Whether this workspace may change the app: a version it runs, of a kind it manages, may be
/// kept or removed.
fn can_pin(a: App) -> Bool {
    not a.bot
        and not runtime_package(a.identity)
        and not language_pack(a.identity)
        and a.user_managed
}

fn pin_label(a: App) -> Text {
    if a.pinned { "Allow updates" } else { "Keep this version" }
}

/// Why the app cannot be changed, when it cannot.
fn refusal(a: App, flatpak: Bool) -> Text {
    if a.trouble != "" {
        a.trouble
    } else if a.awaiting_developer {
        "Waiting for an update from the developer"
    } else if only_other_processors(a) {
        "Not available for this computer: {a.name} is only built for {a.processors}."
    } else if (a.origin == "remote" or a.origin == "flathub") and not flatpak {
        "Flatpak unavailable"
    } else if a.origin == "remote" and not a.user_managed {
        "This repository belongs to the computer. Software changes require its administrator."
    } else if a.origin == "tilo" and a.available == "" and a.installed == "" and not a.bot {
        "No version from this repository can run on this workspace."
    } else { "" }
}

/// The name a source goes by.
fn origin_words(a: App) -> Text {
    if a.origin == "tilo" {
        "Tilo, from {a.registry}"
    } else if a.origin == "flathub" {
        "Flathub"
    } else if a.origin == "remote" {
        a.registry
    } else if a.origin == "local" { "Local package" } else { a.origin }
}

fn trust_words(a: App) -> Text {
    if a.trust == "publisher_signed" {
        "Publisher identity and package integrity are verified. This is not an independent quality review."
    } else if a.trust == "flathub" {
        "Tilo has not independently reviewed this app."
    } else if a.trust == "configured_remote" {
        "This metadata comes from a repository your workspace configured. Tilo has not independently reviewed the app."
    } else if a.trust == "attestor" {
        "Verified by an attestor this computer already trusts."
    } else { "" }
}

fn trust_short(a: App) -> Text {
    if a.trust == "publisher_signed" {
        "Publisher verified"
    } else if a.trust == "flathub" {
        "Flathub"
    } else if a.trust == "configured_remote" {
        "Configured repository"
    } else if a.trust == "attestor" { "Verified locally" } else { "" }
}

/// Where the app stands on this workspace: what a person reads on its card and page.
fn device_words(a: App) -> Text {
    if a.bot {
        "Runs in the workspace it is added to"
    } else if a.installed == "" {
        if a.fetched != "" { "Downloaded, not running" } else { "Not running in this workspace" }
    } else if a.pinned {
        "Kept: version {a.installed}"
    } else {
        "Version {a.installed}, may be cleaned up when unused"
    }
}

fn size_words(bytes: Int) -> Text {
    if bytes <= 0 {
        ""
    } else if bytes >= 1073741824 {
        "{rounded(bytes / 1073741824)} GB"
    } else if bytes >= 1048576 {
        "{rounded(bytes / 1048576)} MB"
    } else { "{rounded(bytes / 1024)} KB" }
}

fn rounded(n: Int) -> Text {
    "{n}"
}

fn parts_words(n: Int, one: Text, many: Text) -> Text {
    if n == 1 { "1 {one}" } else { "{n} {many}" }
}

fn permission_words(p: Permission) -> Text {
    if p.what == "network" {
        "Uses the internet"
    } else if p.what == "files" {
        "Opens and saves files you choose"
    } else if p.what == "files:all" {
        "Reads and changes all your files"
    } else if p.what == "home" {
        "Reads and changes your home folder"
    } else if p.what == "camera" {
        "Uses the camera"
    } else if p.what == "microphone" {
        "Uses the microphone"
    } else if p.what == "screen" {
        "Captures your screen"
    } else if p.what == "location" {
        "Uses your location"
    } else if p.what == "devices" {
        "Uses all devices, such as cameras and game controllers"
    } else if p.what == "bluetooth" {
        "Uses Bluetooth"
    } else if p.what == "system" {
        "Talks to system services"
    } else if p.what == "secrets" {
        "Keeps secrets in your keyring"
    } else if p.what == "ssh" {
        "Uses your SSH or GPG keys"
    } else if p.what == "printing" {
        "Prints"
    } else if p.what == "clipboard" {
        "Reads and changes the clipboard"
    } else if p.what == "notifications" {
        "Shows notifications"
    } else if p.what == "programs" {
        "Runs programs"
    } else if p.what == "sandbox" {
        "Can run programs outside its sandbox"
    } else if p.what == "links" { "Opens links in other apps" } else { p.what }
}

fn bot_trigger_words(kind: Text) -> Text {
    if kind == "connector" {
        "When its connector reports an event"
    } else if kind == "resource" {
        "When a resource changes"
    } else if kind == "schedule" {
        "On a schedule"
    } else if kind == "manual" {
        "When someone runs it"
    } else if kind == "message" { "When a message arrives" } else { kind }
}

/// Reads the workspace's sources again: the registries, Flathub and this computer's Flatpak
/// repositories, and what the workspace runs.
// ---------------------------------------------------------------- reading the sources
action refresh(s: Store) (icon: .refresh) {
    if s.reading != "" {
        return
    }
    match engine.start("catalogue", json.encode(Ask(force: false))) {
        .ok(text) => match json.decode<Started>(text) {
            .ok(started) => {
                s.reading = started.job
                s.trouble = ""
            }
            .err(why) => {
                s.trouble = why
            }
        },
        .err(why) => {
            s.trouble = why
        }
    }
}

/// Asks how far the read or the change running has got, and puts what it found into the
/// workspace. Runs while the window is open.
action poll(s: Store) {
    if s.reading != "" {
        match engine.status(s.reading) {
            .err(why) => {
                s.trouble = why
                s.reading = ""
            }
            .ok(text) => match json.decode<Job>(text) {
                .err(why) => {
                    s.trouble = why
                    s.reading = ""
                }
                .ok(job) => {
                    if job.state == "done" {
                        s.reading = ""
                        s.status = job.status
                        take_catalogue(s, job.status)
                    } else if job.state == "failed" {
                        s.reading = ""
                        s.trouble = job.error
                    } else if job.state == "cancelled" {
                        s.reading = ""
                    } else if job.total > 0 {
                        s.status = job.status
                    }
                }
            },
        }
    }
    for a in s.apps.filter(x -> x.job != "") {
        follow(s, a)
    }
}

/// A read or a change runs while the app is in use, so what it came to is asked for once a
/// second: the engine keeps the answer after the window is closed too.
trigger watch(s: Store) every 1s: poll(s)

/// The job an app's change is, as far as the engine has got: its row says where it stands, and
/// what it made is read once it ends.
action follow(s: Store, a: App) {
    match engine.status(a.job) {
        .err(why) => {
            a.job = ""
            s.trouble = why
        }
        .ok(text) => match json.decode<Job>(text) {
            .err(why) => {
                a.job = ""
                s.trouble = why
            }
            .ok(job) => {
                if job.state == "done" {
                    a.job = ""
                    s.status = job.status
                    read_app(s, a)
                } else if job.state == "failed" {
                    a.job = ""
                    s.trouble = "{a.name}: {job.error}"
                    read_app(s, a)
                } else if job.state == "cancelled" {
                    a.job = ""
                }
            }
        },
    }
}

/// A job that changes the catalogue itself rather than one app, followed as the read is: its end
/// reads the sources again, so a new repository or an update of everything lands.
action begin_reading(s: Store, op: Text, ask: Text) {
    if s.reading != "" {
        return
    }
    match engine.start(op, ask) {
        .ok(text) => match json.decode<Started>(text) {
            .ok(started) => {
                if started.job != "" {
                    s.reading = started.job
                } else {
                    s.trouble = started.error
                }
            }
            .err(why) => {
                s.trouble = why
            }
        },
        .err(why) => {
            s.trouble = why
        }
    }
}

/// A job the engine started for `a`, or why it did not start.
action begin(s: Store, a: App, op: Text, ask: Text) {
    if a.job != "" {
        return
    }
    match engine.start(op, ask) {
        .ok(text) => match json.decode<Started>(text) {
            .ok(started) => {
                a.job = started.job
            }
            .err(why) => {
                s.trouble = why
            }
        },
        .err(why) => {
            s.trouble = why
        }
    }
}

/// Reads one app as it stands now: what the workspace runs, what the computer fetched, and the
/// newest offer its source makes.
action read_app(s: Store, a: App) {
    match engine.call("one", json.encode(OneAsk(identity: a.identity))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => match json.decode<One>(text) {
            .err(why) => {
                s.trouble = why
            }
            .ok(one) => match one.item {
                .some(text) => absorb_text(s, text),
                .none => {}
            },
        },
    }
}

/// Reads one app's page: what its source says about it.
action show_details(s: Store, a: App) {
    match engine.call("details", json.encode(OneAsk(identity: a.identity))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => match json.decode<Details>(text) {
            .err(why) => {
                s.trouble = why
            }
            .ok(d) => {
                a.description = d.description
                a.developer = d.developer
                a.license = d.license
                a.homepage = d.homepage
                a.source_code = d.source_code
                a.size = d.size
                a.screenshots = d.screenshots
                a.runtime = d.runtime
                a.runtime_registry = d.runtime_registry
                a.digest = d.digest
                for p in a.permissions {
                    a.permissions.remove(p)
                }
                for text in d.permissions {
                    match json.decode<Permission>(text) {
                        .ok(p) => { a.permissions.insert(p) }
                        .err(why) => {
                            s.trouble = why
                        }
                    }
                }
                for r in a.releases {
                    a.releases.remove(r)
                }
                for text in d.releases {
                    match json.decode<Release>(text) {
                        .ok(r) => { a.releases.insert(r) }
                        .err(why) => {
                            s.trouble = why
                        }
                    }
                }
                a.details_read = true
            }
        },
    }
}

/// The catalogue one read found, put into the workspace: apps in the order the sources gave
/// them, each replaced where it stands, and the ones its sources stopped listing taken out.
action take_catalogue(s: Store, status: Text) {
    match engine.call("catalogue", json.encode(Ask(force: false))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => match json.decode<Catalogue>(text) {
            .err(why) => {
                s.trouble = why
            }
            .ok(c) => {
                s.status = if c.status != "" { c.status } else { status }
                s.trouble = c.trouble
                s.flatpak = c.flatpak
                s.flatpak_trouble = c.flatpak_trouble
                for name in s.unanswered {
                    s.unanswered.remove(name)
                }
                for name in c.unanswered {
                    s.unanswered.add(name)
                }
                for name in s.expired {
                    s.expired.remove(name)
                }
                for name in c.expired {
                    s.expired.add(name)
                }
                for text in c.items {
                    absorb_text(s, text)
                }
                for text in c.remotes {
                    absorb_remote_text(s, text)
                }
            }
        },
    }
}

/// One app as a source wrote it, read and kept where it stands: an app this workspace already
/// knew keeps what the person looked at of it.
action absorb_text(s: Store, text: Text) -> Option<App> {
    match json.decode<Item>(text) {
        .err(why) => {
            s.trouble = why
            return .none
        }
        .ok(item) => {
            let found = if let known = s.apps.find(x -> x.identity == item.identity) {
                known.name = item.name
                known.summary = item.summary
                if known.description == "" {
                    known.description = item.description
                }
                known.developer = item.developer
                known.license = item.license
                for c in known.categories {
                    known.categories.remove(c)
                }
                for c in item.categories {
                    known.categories.add(c)
                }
                known.icon = item.icon
                known.homepage = item.homepage
                known.source_code = item.source_code
                known.origin = item.origin
                known.registry = item.registry
                known.branch = item.branch
                known.user_managed = item.user_managed
                known.trust = item.trust
                for v in known.versions {
                    known.versions.remove(v)
                }
                for text in item.versions {
                    match json.decode<Version>(text) {
                        .ok(v) => { known.versions.insert(v) }
                        .err(why) => {
                            s.trouble = why
                        }
                    }
                }
                known.available = item.available
                known.installed = item.installed
                known.pinned = item.pinned
                known.fetched = item.fetched
                known.runnable = item.runnable
                known.processors = item.processors
                known.awaiting_developer = item.awaiting_developer
                known.updatable = item.updatable
                known.trouble = item.trouble
                known.featured = item.featured
                known.updated = item.updated
                known.publisher = item.publisher
                if item.digest != "" {
                    known.digest = item.digest
                }
                known.bot = item.bot
                known.bot_appearance = item.bot_appearance
                for k in known.bot_triggers {
                    known.bot_triggers.remove(k)
                }
                for k in item.bot_triggers {
                    known.bot_triggers.add(k)
                }
                for k in known.bot_apps {
                    known.bot_apps.remove(k)
                }
                for k in item.bot_apps {
                    known.bot_apps.add(k)
                }
                for k in known.bot_connectors {
                    known.bot_connectors.remove(k)
                }
                for k in item.bot_connectors {
                    known.bot_connectors.add(k)
                }
                if item.available != "" and newer(item.available, known.available) {
                    known.available = item.available
                }
                known
            } else {
                let made = s.apps.insert(
                    App(identity: item.identity, name: item.name, summary: item.summary),
                )
                made.description = item.description
                made.developer = item.developer
                made.license = item.license
                for c in item.categories {
                    made.categories.add(c)
                }
                made.icon = item.icon
                made.screenshots = item.screenshots
                made.homepage = item.homepage
                made.source_code = item.source_code
                made.origin = item.origin
                made.registry = item.registry
                made.branch = item.branch
                made.user_managed = item.user_managed
                made.trust = item.trust
                for text in item.versions {
                    match json.decode<Version>(text) {
                        .ok(v) => { made.versions.insert(v) }
                        .err(why) => {
                            s.trouble = why
                        }
                    }
                }
                made.available = item.available
                made.installed = item.installed
                made.pinned = item.pinned
                made.fetched = item.fetched
                made.runnable = item.runnable
                made.processors = item.processors
                made.awaiting_developer = item.awaiting_developer
                made.updatable = item.updatable
                made.trouble = item.trouble
                made.featured = item.featured
                made.updated = item.updated
                made.publisher = item.publisher
                made.digest = item.digest
                made.bot = item.bot
                made.bot_appearance = item.bot_appearance
                for k in item.bot_triggers {
                    made.bot_triggers.add(k)
                }
                for k in item.bot_apps {
                    made.bot_apps.add(k)
                }
                for k in item.bot_connectors {
                    made.bot_connectors.add(k)
                }
                made
            }
            return .some(found)
        }
    }
}

/// One repository as a source wrote it, kept where it stands.
action absorb_remote_text(s: Store, text: Text) {
    match json.decode<Item>(text) {
        .err(why) => {
            s.trouble = why
        }
        .ok(item) => {
            if let known = s.remotes.find(r -> r.name == item.identity) {
                known.title = item.name
                known.url = item.homepage
                known.count = item.featured
                known.trouble = ""
            } else {
                s.remotes.insert(
                    Remote(
                        name: item.identity,
                        title: item.name,
                        url: item.homepage,
                        scope: if item.registry == "device" { "device" } else { "account" },
                        enabled: true,
                        count: item.featured,
                    ),
                )
            }
        }
    }
}

/// Reads this computer's Flatpak repositories again.
action refresh_remotes(s: Store) (icon: .refresh) {
    for r in s.remotes {
        s.remotes.remove(r)
    }
    match engine.call("remotes", "") {
        .err(why) => {
            s.flatpak_trouble = why
        }
        .ok(text) => match json.decode<RemoteReads>(text) {
            .err(why) => {
                s.flatpak_trouble = why
            }
            .ok(read) => {
                s.flatpak = read.flatpak
                s.flatpak_trouble = read.trouble
                for text in read.remotes {
                    match json.decode<RemoteRead>(text) {
                        .ok(r) => {
                            s.remotes.insert(
                                Remote(
                                    name: r.name,
                                    title: if r.title == "" { r.name } else { r.title },
                                    url: r.url,
                                    scope: r.scope,
                                    enabled: r.enabled,
                                    trouble: r.trouble,
                                ),
                            )
                        }
                        .err(why) => {
                            s.flatpak_trouble = why
                        }
                    }
                }
            }
        },
    }
}

/// Adds a Flatpak repository to this workspace.
action add_remote(s: Store, name: Text, url: Text) (label: "Add repository", icon: .plus) {
    require name != "" "A repository has a name"
    require name.len() <= 128 "A repository name holds at most 128 letters"
    require url.starts_with("https://") and url.len() <= 2048 "Repositories must use HTTPS"
    require s.remotes.find(r -> r.name == name) == .none "This repository is already here"
    begin_reading(s, "remote-add", json.encode(RemoteAsk(name: name, url: url, enabled: true)))
    s.remotes.insert(Remote(name: name, title: name, url: url))
}

/// Enables one of this workspace's repositories.
action enable_remote(s: Store, r: Remote) (label: "Enable source") {
    require r.scope == "account" "This repository is the computer's. Only repositories in your workspace can be changed here."
    remote_change(s, r, true)
}

/// Disables one of this workspace's repositories.
action disable_remote(s: Store, r: Remote) (label: "Disable source") {
    require r.scope == "account" "This repository is the computer's. Only repositories in your workspace can be changed here."
    remote_change(s, r, false)
}

action remote_change(s: Store, r: Remote, enabled: Bool) {
    r.enabled = enabled
    begin_reading(
        s,
        "remote-enable",
        json.encode(RemoteAsk(name: r.name, url: r.url, enabled: enabled)),
    )
}

/// Removes one of this workspace's repositories, and its apps from the catalogue.
action remove_remote(s: Store, r: Remote) (label: "Remove repository", icon: .trash) {
    require r.scope == "account" "This repository is the computer's. Only repositories in your workspace can be changed here."
    let name = r.name
    s.remotes.remove(r)
    for a in s.apps.filter(x -> x.origin == "remote" and x.registry == name) {
        s.apps.remove(a)
    }
    begin_reading(s, "remote-remove", json.encode(RemoteAsk(name: name, url: "", enabled: false)))
}

/// Browses one repository's apps, through the system Flatpak metadata on this computer.
action browse(s: Store, r: Remote) (icon: .search) {
    match engine.call(
        "browse",
        json.encode(RemoteAsk(name: r.name, url: r.url, enabled: r.enabled)),
    ) {
        .err(why) => {
            r.trouble = why
        }
        .ok(text) => match json.decode<Items>(text) {
            .err(why) => {
                r.trouble = why
            }
            .ok(found) => {
                r.trouble = ""
                r.loaded = true
                for text in found.items {
                    absorb_text(s, text)
                }
                r.count = s.apps.count(a -> a.origin == "remote" and a.registry == r.name)
            }
        },
    }
}

/// Gets an app the workspace does not run yet: the newest version its source offers.
// ---------------------------------------------------------------- changing apps
action get(a: App) (label: "Get", icon: .download) {
    require a.trouble == "" a.trouble
    let st = root Store
    begin(st, a, "install", json.encode(change_of(a, "install")))
}

/// Updates an app the workspace runs to the newest version its source offers.
action update(a: App) (label: "Update", icon: .arrow_down) {
    require a.trouble == "" a.trouble
    let st = root Store
    begin(st, a, "update", json.encode(change_of(a, "update")))
}

/// Installs the runtime an app needs, once its page reviewed the exact signed offer.
action install_runtime(a: App) (label: "Install the signed runtime", icon: .download) {
    require a.runtime != "" "This app names no runtime"
    require a.trouble == "" a.trouble
    let st = root Store
    begin(st, a, "install-runtime", json.encode(change_of(a, "install-runtime")))
}

/// Removes an app from this workspace.
action remove(a: App) (label: "Uninstall", icon: .trash) {
    require a.fetched == "" or a.user_managed "This app belongs to the computer. Ask its administrator to remove it."
    let st = root Store
    begin(st, a, "uninstall", json.encode(change_of(a, "uninstall")))
}

/// Keeps this workspace's version of an app, so it is never cleaned up when unused.
action pin(a: App) (label: "Keep this version", icon: .pin) {
    require can_pin(a) "This app has no version to keep"
    let st = root Store
    a.pinned = true
    begin(st, a, "pin", json.encode(change_of(a, "pin")))
}

/// Lets this workspace's unused version of an app be cleaned up again.
action unpin(a: App) (label: "Allow updates", icon: .pin_off) {
    let st = root Store
    a.pinned = false
    begin(st, a, "unpin", json.encode(change_of(a, "unpin")))
}

/// Stops the change running for an app, keeping what it did until then.
action cancel_change(a: App) (label: "Stop", icon: .square) {
    if a.job != "" {
        engine.cancel(a.job)
        a.job = ""
    }
}

/// Opens an app this computer has, fetching it first when the workspace runs it and this
/// computer has not.
action open_app(a: App) uses files (label: "Open", icon: .play) {
    session.open_app(a.identity)
}

/// A change one app asks the engine for: what it is, the exact version its source offered, and
/// the reference a Flatpak installs.
type Change {
    identity: Text
    kind: Text
    version: Text = ""
    root: Text = ""
    surface: Text = ""
    publisher: Text = ""
    flatpak: Text = ""
    reference: Text = ""
    file: Text = ""
    size: Int = 0
}

fn change_of(a: App, kind: Text) -> Change {
    let version = a.versions.find(v -> v.version == a.available) ?? a.versions.first()
    Change(
        identity: a.identity,
        kind: if a.origin == "tilo" or a.origin == "local" { "tilo" } else { "flatpak" },
        version: version?.version ?? "",
        root: version?.root ?? "",
        surface: version?.surface ?? "",
        publisher: a.publisher,
        flatpak: if kind == "install" or kind == "update" { a.branch } else { "" },
        reference: a.reference,
        size: version?.size ?? a.size,
    )
}

/// Updates every app the workspace runs and its sources offer a newer version of.
action update_all(s: Store) (label: "Update all apps", icon: .arrow_down) {
    require not browsable(s).filter(a -> newer(a.available, a.installed)).is_empty() "Every app is up to date"
    let updates = browsable(s).filter(a -> newer(a.available, a.installed))
    begin_reading(
        s,
        "update-all",
        json.encode(Changes(list: updates.map(a -> change_of(a, "update")))),
    )
}

/// Adds a directory bot to this workspace as the person's own shortcut: its program is verified
/// against the signed listing and saved as the shortcut the workspace's host runs for them, under
/// the triggers the program declares.
action add_bot(s: Store, a: App) (label: "Add to this workspace…", icon: .bot) {
    require a.bot "Only a directory bot is added to a workspace"
    begin(s, a, "add-bot", json.encode(OneAsk(identity: a.identity, registry: a.registry)))
}

/// Blocks an app: it then appears nowhere in the Store or in search, and cannot be opened or
/// installed from them. Only the person who blocked it can unblock it.
// ---------------------------------------------------------------- blocking
action block_app(s: Store, a: App) (label: "Block app", icon: .ban) {
    if s.blocks.any(b -> b.identity == a.identity) {
        return
    }
    s.blocks.insert(Block(identity: a.identity))
    match engine.call("block", json.encode(BlockAsk(identity: a.identity, blocked: true))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(_) => {}
    }
}

/// Unblocks an app this person blocked. A device's or a workspace's block stays.
action unblock_app(s: Store, a: App) (label: "Unblock", icon: .circle_check) {
    require s.blocks.any(b -> b.identity == a.identity) "This app is not blocked"
    let block = s.blocks.find(b -> b.identity == a.identity)
    if let b = block { s.blocks.remove(b) }
    match engine.call("block", json.encode(BlockAsk(identity: a.identity, blocked: false))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(_) => {}
    }
}

/// Reviews a package file someone hands the Store. Opening one never installs it: its identity
/// must match a verified offer, or an attestor this computer trusts.
// ---------------------------------------------------------------- packages and references
action import_package(s: Store, file: File) (
    icon: .package,
    opens: [.tapp, "application/vnd.tilo.package"],
) {
    match engine.call(
        "review",
        json.encode(PackageAsk(file: file.url, name: file.name, size: file.size)),
    ) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => match json.decode<Review>(text) {
            .err(why) => {
                s.trouble = why
            }
            .ok(review) => {
                if review.trouble != "" {
                    s.trouble = review.trouble
                } else if let a = absorb_text(s, review.item) {
                    s.reviewed_root = review.root
                    s.reviewed_app = a.identity
                    s.reviewed_file = .some(file)
                    s.status = "Package verified. Review its details before installing."
                }
            }
        },
    }
}

/// Installs the package someone just reviewed, after the person read its details.
action install_reviewed(s: Store) (label: "Install reviewed package", icon: .download) {
    require s.reviewed_app != "" "Review this exact package before installing it"
    require s.apps.any(x -> x.identity == s.reviewed_app) "Review this exact package before installing it"
    let a = s.apps.find(x -> x.identity == s.reviewed_app)
    if let found = a {
        begin(
            s,
            found,
            "install",
            json.encode(
                Change(
                    identity: found.identity,
                    kind: "local",
                    version: found.available,
                    root: s.reviewed_root,
                    publisher: found.publisher,
                    file: if let f = s.reviewed_file { f.url } else { "" },
                ),
            ),
        )
        s.reviewed_root = ""
        s.reviewed_app = ""
        s.reviewed_file = .none
    }
}

/// Opens a `.flatpakref`: it joins the catalogue as a card, and installing it stays a separate
/// step the person takes.
action open_reference(s: Store, file: File) (opens: [.flatpakref, "application/vnd.flatpak.ref"]) {
    match engine.call(
        "reference",
        json.encode(PackageAsk(file: file.url, name: file.name, size: file.size)),
    ) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => match json.decode<Review>(text) {
            .err(why) => {
                s.trouble = why
            }
            .ok(review) => {
                if review.trouble != "" {
                    s.trouble = review.trouble
                } else if absorb_text(s, review.item) != .none {
                    s.status = "Opened a Flatpak reference."
                }
            }
        },
    }
}

/// Reads a runtime's exact signed offer, so its page says what installing it would run.
action review_runtime(s: Store, runtime: Text, registry: Text = "") -> Option<App> (
    label: "Review an app's runtime",
    icon: .package,
) {
    match engine.call("runtime", json.encode(RuntimeAsk(runtime: runtime, registry: registry))) {
        .err(why) => {
            s.trouble = why
            return .none
        }
        .ok(text) => match json.decode<One>(text) {
            .err(why) => {
                s.trouble = why
                return .none
            }
            .ok(one) => match one.item {
                .some(text) => {
                    return absorb_text(s, text)
                }
                .none => {
                    s.trouble = "No enabled repository offers the required runtime for this computer."
                    return .none
                }
            },
        },
    }
    return .none
}

// ---------------------------------------------------------------- reports and moderation
primary action draft_report(s: Store, a: App) -> Report (
    label: "Create report draft",
    icon: .flag,
) {
    if let known = s.reports.find(r -> r.identity == a.identity) {
        known
    } else {
        s.reports.insert(Report(identity: a.identity, status: "Not submitted"))
    }
}

/// Records the registry's case for an app, as a refresh read it.
action record_moderation_case(s: Store, identity: Text, case: Text, status: Text) {
    require case != "" and status != "" "A case has an id and a status"
    let report = if let known = s.reports.find(r -> r.identity == identity) {
        known
    } else {
        s.reports.insert(Report(identity: identity))
    }
    report.case = case
    report.status = status
}

/// Prepares the exact signed operation a report or an appeal sends, without sending it.
action prepare_report(s: Store, r: Report, appeal: Bool) -> Prepared {
    let registry = r.registry
    match engine.call(
        "prepare-report",
        json.encode(
            ReportAsk(
                identity: r.identity,
                note: r.note,
                case: r.case,
                registry: registry,
                appeal: appeal,
            ),
        ),
    ) {
        .err(why) => Prepared(trouble: why),
        .ok(text) => match json.decode<Prepared>(text) {
            .err(why) => Prepared(trouble: why),
            .ok(prepared) => {
                r.operation = prepared.operation
                r.registry = prepared.registry
                prepared
            }
        },
    }
}

/// Signs a report with the workspace's identity and sends it to the app's repository.
action submit_report(r: Report) (label: "Submit report", icon: .send) {
    let st = root Store
    let prepared = prepare_report(st, r, false)
    if prepared.trouble != "" or prepared.operation == "" {
        st.trouble = prepared.trouble
        return
    }
    begin_reading(
        st,
        "submit-report",
        json.encode(SendAsk(identity: r.identity, operation: prepared.operation, note: r.note)),
    )
    r.status = "Submitted, awaiting verified status"
}

/// Appeals a decision about an app.
action appeal(r: Report) (label: "Appeal decision", icon: .arrow_up) {
    require r.case != "" "Only a decision can be appealed"
    let st = root Store
    let prepared = prepare_report(st, r, true)
    if prepared.trouble != "" or prepared.operation == "" {
        st.trouble = prepared.trouble
        return
    }
    begin_reading(
        st,
        "appeal",
        json.encode(SendAsk(identity: r.identity, operation: prepared.operation, note: r.note)),
    )
}

/// Reads the registry's verified status for the report's case again. A draft stays a draft
/// until the registry answers.
action refresh_status(s: Store, r: Report) (label: "Refresh status", icon: .refresh) {
    require r.case != "" "This report has no case yet"
    match engine.call(
        "report-status",
        json.encode(StatusAsk(identity: r.identity, case: r.case, registry: r.registry)),
    ) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => match json.decode<Case>(text) {
            .err(why) => {
                s.trouble = why
            }
            .ok(read) => {
                if read.trouble != "" {
                    s.trouble = "Registry request failed: {read.trouble}"
                } else {
                    r.status = read.status
                    s.status = "Moderation status refreshed: {read.case} is {read.status}."
                }
            }
        },
    }
}

/// A read of the sources, as the engine takes it.
type Ask {
    force: Bool = false
}

/// The engine's answer when it starts a job.
type Started {
    job: Text = ""
    error: Text = ""
}

/// What one app is asked about: its id, and the registry its source is, when a bot is added from
/// it.
type OneAsk {
    identity: Text
    registry: Text = ""
}

type One {
    item: Option<Text> = .none
}

type OneRead {
    item: Item
}

type RemoteReads {
    /// Each repository, as its own JSON text.
    remotes: List<Text>
    flatpak: Bool = false
    trouble: Text = ""
}

type RemoteAsk {
    name: Text
    url: Text
    enabled: Bool = true
}

type BlockAsk {
    identity: Text
    blocked: Bool
}

type PackageAsk {
    file: Text
    name: Text
    size: Int
}

type RuntimeAsk {
    runtime: Text
    registry: Text
}

type ReportAsk {
    identity: Text
    note: Text
    case: Text
    registry: Text
    appeal: Bool
}

type SendAsk {
    identity: Text
    operation: Text
    note: Text
}

type StatusAsk {
    identity: Text
    case: Text
    registry: Text
}

type Changes {
    list: List<Change>
}

type Items {
    items: List<Text>
}

type Evidence {
    identity: Text
    note: Text
    status: Text
    case: Text
    registry: Text
    operation: Text
}
