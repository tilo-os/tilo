app so.tilo.store "App Store" (
    icon: .store,
    category: .system,
    tagline: "The apps of your workspace, and what they run on this computer",
    keywords: ["store", "apps", "install", "update", "uninstall", "get", "flatpak", "registry", "repositories", "bots"],
    web: ["flathub.org"],
)

use so.tilo.session as session
use so.tilo.device as device
use so.tilo.settings.sections as settings

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
    /// The bytes it takes once installed, when its source says.
    installed_size: Int = 0
    /// The languages it is written in, as its source says.
    languages: Set<Text>
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
    /// The languages its source says it is written in.
    supported_languages: Option<List<Text>> = .none
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
    installed_size: Int = 0
    screenshots: List<Text>
    permissions: List<Text>
    releases: List<Text>
    runtime: Text = ""
    runtime_registry: Text = ""
    digest: Text = ""
    supported_languages: Option<List<Text>> = .none
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

/// The engine's own line about a job, once it moved.
type JobWords {
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
    match engine.start(
        "catalogue",
        json.encode(Ask(force: false)),
        done: j -> reading_done(s, j),
        progress: j -> reading_moved(s, j),
    ) {
        .ok(job) => {
            s.reading = job
            s.trouble = ""
        }
        .err(why) => {
            s.trouble = why
        }
    }
}

/// The read moved on: the line the window shows while it runs.
action reading_moved(s: Store, j: host.Job) {
    if j.total > 0 {
        match json.decode<JobWords>(j.detail) {
            .ok(words) => {
                s.status = words.status
            }
            .err(_) => {}
        }
    }
}

/// The read ended: the catalogue it found, or why it found none.
action reading_done(s: Store, j: host.Job) uses web {
    s.reading = ""
    if j.state == .done {
        match json.decode<JobWords>(j.detail) {
            .ok(words) => {
                s.status = words.status
                take_catalogue(s, words.status)
            }
            .err(why) => {
                s.trouble = why
            }
        }
    } else if j.state == .failed {
        s.trouble = j.error
    }
}

/// The job an app's change is, as far as the engine has got: its row says where it stands, and
/// what it made is read once it ends.
action change_done(s: Store, a: App, j: host.Job) {
    a.job = ""
    if j.state == .done {
        match json.decode<JobWords>(j.detail) {
            .ok(words) => {
                s.status = words.status
                read_app(s, a)
            }
            .err(why) => {
                s.trouble = why
            }
        }
    } else if j.state == .failed {
        s.trouble = "{a.name}: {j.error}"
        read_app(s, a)
    }
}

/// A job that changes the catalogue itself rather than one app, followed as the read is: its end
/// reads the sources again, so a new repository or an update of everything lands.
action begin_reading(s: Store, op: Text, ask: Text) {
    if s.reading != "" {
        return
    }
    match engine.start(op, ask, done: j -> reading_done(s, j), progress: j -> reading_moved(s, j)) {
        .ok(job) => {
            s.reading = job
        }
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
    match engine.start(op, ask, done: j -> change_done(s, a, j)) {
        .ok(job) => {
            a.job = job
        }
        .err(why) => {
            s.trouble = why
        }
    }
}

/// Reads one app as it stands now: what the workspace runs, what the computer fetched, and the
/// newest offer its source makes.
action read_app(s: Store, a: App) {
    if a.origin == "flathub" {
        // Flathub's catalogue is read here, so its row is what the app already holds.
        return
    }
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
action show_details(s: Store, a: App) uses web {
    if a.origin == "flathub" {
        read_flathub_details(s, a)
        return
    }
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
                a.installed_size = d.installed_size
                if let langs = d.supported_languages {
                    for l in a.languages {
                        a.languages.remove(l)
                    }
                    for l in langs {
                        a.languages.add(l)
                    }
                }
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
action take_catalogue(s: Store, status: Text) uses web {
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
    // Flathub's catalogue is read after the registries', so a Tilo app of the same name is the
    // one a person gets.
    read_flathub(s)
}

/// Reads Flathub's catalogue over its own search API, a page at a time, and keeps every app
/// beside the registry's. A person's language names the rows, as every other source's does.
// ---------------------------------------------------------------- Flathub
action read_flathub(s: Store) uses web {
    let language = locale_language()
    let last = 1
    for page in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
        if page > last {
            return
        }
        let ask = json.encode(FlathubAsk(page: page))
        let answer = web.post(
            "https://flathub.org/api/v2/search?locale={language}",
            body: ask,
            headers: ["Content-Type: application/json"],
        )
        if not answer.ok {
            if page == 1 {
                s.status = "The apps of your workspace are read; Flathub could not be reached."
            }
            return
        }
        match json.parse(answer.text()) {
            .err(why) => {
                s.trouble = std.say("Flathub's catalogue could not be read: {why}")
                return
            }
            .ok(root) => {
                last = number_at(root, "totalPages").to_int()
                for hit in list_at(root, "hits") {
                    if let item = flathub_item(s, hit) { absorb_text(s, json.encode(item)) }
                }
            }
        }
    }
}

/// One app of Flathub's catalogue, as the app keeps it: nothing for a hit of no app, or one a
/// Tilo app of the same name stands in for.
fn flathub_item(s: Store, hit: json.Value) -> Option<Item> {
    let identity = text_of(hit, "app_id")
    if identity == "" {
        return .none
    }
    let name = text_of(hit, "name")
    if name == "" {
        name = identity
    }
    if s.apps.any(a -> a.origin == "tilo" and a.name.lower() == name.lower()) {
        return .none
    }
    let known = s.apps.find(a -> a.identity == identity)
    let installed = if let a = known { a.installed } else { "" }
    let fetched = if let a = known { a.fetched } else { "" }
    let arches = strings_of(list_at(hit, "arches"))
    let built = arches.is_empty() or arches.contains(device.host_info.arch)
    return .some(
        Item(
            identity: identity,
            name: name,
            summary: if text_of(hit, "summary") != "" {
                text_of(hit, "summary")
            } else { text_of(hit, "description") },
            description: text_of(hit, "description"),
            developer: text_of(hit, "developer_name"),
            license: text_of(hit, "project_license"),
            categories: flathub_categories(hit),
            icon: text_of(hit, "icon"),
            screenshots: [],
            bot_triggers: [],
            bot_apps: [],
            bot_connectors: [],
            origin: "flathub",
            registry: "Flathub",
            versions: [json.encode(Version(version: ""))],
            installed: installed,
            fetched: fetched,
            runnable: installed != "" or built,
            processors: processors_of(arches),
            trouble: if built or installed != "" {
                ""
            } else {
                "Not available for this computer: {name} is only built for {processors_of(arches)}."
            },
            trust: "flathub",
        ),
    )
}

/// What one Flathub app's page reads: its own AppStream document and what its build asks for.
action read_flathub_details(s: Store, a: App) uses web {
    let language = locale_language()
    let appstream = web.get("https://flathub.org/api/v2/appstream/{a.identity}?locale={language}")
    if not appstream.ok {
        s.trouble = std.say("Flathub did not answer for {a.name}")
        return
    }
    let summary = web.get("https://flathub.org/api/v2/summary/{a.identity}")
    let root = object_of(appstream.text())
    if let root = root {
        let build = if summary.ok { object_of(summary.text()) } else { .none }
        a.description = text_of(root, "description")
        let developer = text_of(root, "developer_name")
        a.developer = if developer != "" {
            developer
        } else { nested_text(root, "developer", "name") }
        let license = text_of(root, "project_license")
        a.license = if license != "" { license } else { text_of(root, "license") }
        a.homepage = nested_text(root, "urls", "homepage")
        a.screenshots = screenshots_of(root)
        if let build = build {
            a.size = number_at(build, "download_size").to_int()
            a.installed_size = number_at(build, "installed_size").to_int()
        }
        for l in a.languages {
            a.languages.remove(l)
        }
        for l in strings_of(list_at(root, "languages")) {
            if l != "" { a.languages.add(l) }
        }
        for p in a.permissions {
            a.permissions.remove(p)
        }
        for text in permissions_of(build) {
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
        for text in releases_of(root) {
            match json.decode<Release>(text) {
                .ok(r) => { a.releases.insert(r) }
                .err(why) => {
                    s.trouble = why
                }
            }
        }
        a.details_read = true
    } else {
        s.trouble = std.say("Flathub's answer for {a.name} could not be read")
    }
}

/// The language Flathub is read in: the person's own, without its region.
fn locale_language() -> Text {
    let language = settings.appearance.language ?? "en"
    let base = language.split("-").first() ?? "en"
    if base == "" { "en" } else { base }
}

/// The value at `key` of a JSON object, and nothing for anything else.
fn field_of(value: json.Value, key: Text) -> Option<json.Value> {
    match value {
        .object(fields) => fields.get(key),
        .array(items) => .none,
        .string(text) => .none,
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The text at `key` of a JSON object, or empty.
fn text_of(value: json.Value, key: Text) -> Text {
    match field_of(value, key) {
        .some(held) => match held {
            .string(text) => text,
            .object(fields) => "",
            .array(items) => "",
            .number(number) => "",
            .bool(flag) => "",
            .null => "",
        },
        .none => "",
    }
}

/// The number at `key` of a JSON object, or zero.
fn number_at(value: json.Value, key: Text) -> Float {
    match field_of(value, key) {
        .some(held) => match held {
            .number(number) => number,
            .string(text) => text.to_float() ?? 0.0,
            .object(fields) => 0.0,
            .array(items) => 0.0,
            .bool(flag) => 0.0,
            .null => 0.0,
        },
        .none => 0.0,
    }
}

/// The text inside a JSON object's object, as `urls.homepage` reads.
fn nested_text(value: json.Value, key: Text, name: Text) -> Text {
    match field_of(value, key) {
        .some(held) => text_of(held, name),
        .none => "",
    }
}

/// The array at `key` of a JSON object, or nothing.
fn list_at(value: json.Value, key: Text) -> List<json.Value> {
    match field_of(value, key) {
        .some(held) => match held {
            .array(items) => items,
            .object(fields) => [],
            .string(text) => [],
            .number(number) => [],
            .bool(flag) => [],
            .null => [],
        },
        .none => [],
    }
}

/// The texts a JSON array holds, a value that is no text reading as empty.
fn strings_of(items: List<json.Value>) -> List<Text> {
    items.map(
        item -> match item {
            .string(text) => text,
            .object(fields) => "",
            .array(inside) => "",
            .number(number) => "",
            .bool(flag) => "",
            .null => "",
        },
    )
}

/// A body as the JSON object it holds, when it holds one.
fn object_of(body: Text) -> Option<json.Value> {
    match json.parse(body) {
        .ok(value) => .some(value),
        .err(why) => .none,
    }
}

/// The categories an app is listed under: Flathub names an app's main category as a text when it
/// has one and as a list when it has more.
fn flathub_categories(hit: json.Value) -> List<Text> {
    match field_of(hit, "main_categories") {
        .some(held) => match held {
            .string(text) => if text == "" { [] } else { [text] },
            .array(items) => strings_of(items),
            .object(fields) => [],
            .number(number) => [],
            .bool(flag) => [],
            .null => [],
        },
        .none => [],
    }
}

/// The processors an app is built for, as people read them.
fn processors_of(arches: List<Text>) -> Text {
    arches.join(", ")
}

/// Whether a value a document holds is there at all: a permission written as `null` is not.
fn present(value: Option<json.Value>) -> Bool {
    match value {
        .none => false,
        .some(held) => match held {
            .null => false,
            .object(fields) => true,
            .array(items) => true,
            .string(text) => true,
            .number(number) => true,
            .bool(flag) => true,
        },
    }
}

/// The screenshots an app's page shows, at most eight, each a web address.
fn screenshots_of(root: json.Value) -> List<Text> {
    let out: List<Text> = []
    for shot in list_at(root, "screenshots") {
        let url = first_text(shot, ["imgDesktopUrl", "url", "src"])
        if url.starts_with("https://") and out.len() < 8 {
            out = out + [url]
        }
    }
    out
}

/// The first of `names` an object holds as text, or empty.
fn first_text(value: json.Value, names: List<Text>) -> Text {
    for name in names {
        let held = text_of(value, name)
        if held != "" {
            return held
        }
    }
    ""
}

/// The newest releases an app's page lists, at most six, each as JSON the app reads.
fn releases_of(root: json.Value) -> List<Text> {
    let out: List<Text> = []
    for release in list_at(root, "releases") {
        let version = text_of(release, "version")
        if version != "" and out.len() < 6 {
            out = out
                + [json.encode(Release(version: version, notes: text_of(release, "description")))]
        }
    }
    out
}

/// What a build asks for, each as JSON a permission reads: what it reaches and whether that
/// reaches everything.
fn permissions_of(build: Option<json.Value>) -> List<Text> {
    let out: List<Text> = []
    if let root = build {
        let permissions = match field_of(root, "metadata") {
            .some(metadata) => field_of(metadata, "permissions") ?? field_of(root, "permissions"),
            .none => field_of(root, "permissions"),
        }
        if let p = permissions {
            let filesystems = strings_of(list_at(p, "filesystems"))
            if filesystems.contains("host") or filesystems.contains("host:ro") {
                out = out + [permission_text("files:all", true)]
            } else if filesystems.contains("home") or filesystems.contains("home:ro") {
                out = out + [permission_text("home", true)]
            }
            if present(field_of(p, "network")) {
                out = out + [permission_text("network", false)]
            }
            if strings_of(list_at(p, "devices")).contains("all") {
                out = out + [permission_text("devices", true)]
            }
            for name in strings_of(list_at(p, "shared")) {
                if name == "network" {
                    out = out + [permission_text("network", false)]
                } else if name == "ipc" {
                    out = out + [permission_text("system", false)]
                }
            }
            if strings_of(list_at(p, "sockets")).contains("x11") {
                out = out + [permission_text("system", false)]
            }
            if not strings_of(list_at(p, "talk-name")).is_empty() {
                out = out + [permission_text("system", false)]
            }
        }
    }
    let seen: List<Text> = []
    let kept: List<Text> = []
    for text in out.sort_by(x -> x) {
        if not seen.contains(text) {
            seen = seen + [text]
            kept = kept + [text]
        }
    }
    kept
}

/// One permission, as JSON a permission reads.
fn permission_text(what: Text, broad: Bool) -> Text {
    json.encode(Permission(what: what, broad: broad))
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
                if let langs = item.supported_languages {
                    for l in known.languages {
                        known.languages.remove(l)
                    }
                    for l in langs {
                        known.languages.add(l)
                    }
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
                if let langs = item.supported_languages {
                    for l in langs {
                        made.languages.add(l)
                    }
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
    opens: [.tapp, .flatpakref, "application/vnd.tilo.package", "application/vnd.flatpak.ref"],
) {
    // A `.flatpakref` names a repository's app, which the source installs itself; only a
    // package file is verified here.
    if file.name.ends_with(".flatpakref") {
        open_reference(s, file)
        return
    }
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
/// Opens a `.flatpakref`: a repository's app, which the source installs itself. The package
/// review reaches it through `import_package`, which opens both kinds of file.
action open_reference(s: Store, file: File) {
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

/// What a page of Flathub's catalogue is asked for.
type FlathubAsk {
    query: Text = ""
    hits_per_page: Int = 500
    page: Int = 1
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
