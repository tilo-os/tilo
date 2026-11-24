app so.tilo.passwords "Passwords" (
    icon: .key,
    category: .utility,
    tagline: "Keeps your logins and passwords in a vault",
    keywords: ["password manager", "password", "passwords", "credentials", "logins", "vault", "keychain", "secrets", "passkeys", "1password", "bitwarden", "keepass", "keepassxc", "lastpass", "passwort", "passwörter", "passwortmanager"],
    splash_budget: 250,
)

use tilo.vault as vault

/// What kind of secret an item is: what it is for, which sets the fields it has and the words
/// that name them.
enum Kind {
    /// A login: a username and a password for a site.
    login (icon: .globe)
    /// A key an API takes.
    api_key (icon: .code)
    /// A note kept as a secret, with no address.
    note (icon: .file_text)
    /// The password of a Wi-Fi network.
    wifi (icon: .wifi)
    /// Any other secret: a licence code, a recovery key.
    secret (icon: .shield)
}

/// One saved secret, as the person finds it: what it is called, its kind and tags, and the entry
/// of the workspace's vault that keeps everything else.
///
/// The vault keeps who it signs in as, where, its password, its notes and the fields a person
/// added, sealed; this record never holds them. The window reads them through the vault, which
/// answers Passwords alone, and shows a secret only when the person asks. Every member the
/// workspace lets read Passwords reads the same vault.
type Item (untitled: "Untitled item") {
    /// The entry of the workspace's vault this item stands for: its id there.
    entry: Text
    /// What it is called: a site, a network, a service.
    title: Text
    kind: Kind = .login
    tags: Set<Text>
    favorite: Bool = false
    /// What an older Passwords kept in this record before its secrets moved into the vault. The
    /// vault takes each once and these are emptied as it does; nothing writes them now.
    username: Text
    password: Text
    notes: Text
    origins: Set<Text>
    fields: Map<Text>

    search {
        filter favorite "Favorites" (icon: .star)
        filter kind
        filter tags
        sort title a_z
        conceal username
        conceal password
        conceal notes
        conceal origins
        conceal fields
    }
}

/// A row a browser's CSV held, waiting for the person to say whether it comes into the vault. Its
/// password stays in the file until it does.
type ImportRow {
    title: Text
    username: Text
    origin: Text
    /// Whether the vault already holds that login, so importing skips it.
    skip: Bool
}

/// The workspace's vault as Passwords lists it: an item for each entry, and what the window
/// learned of what it tried to do.
type Vault {
    items: Collection<Item>
    /// The ids of device-vault items an older Passwords took in.
    converted: Set<Text>
    /// The rows the person is deciding about, before an import.
    pending: Collection<ImportRow>
    /// The file those rows come from, by its address.
    importing: Text = ""
    /// What the last import, export or backup did, as a line the transfer page reads.
    notice: Text
}

root vault_items: Vault

default view vault_window for Vault

/// The vault's entry `item` stands for, as the vault reads it now: none until it is read, or once
/// another device forgot it.
// ---------------------------------------------------------------- the vault
fn entry_of(item: Item) -> Option<vault.Entry> {
    vault.vault.entries.find(e -> e.id == item.entry)
}

/// The kind as the vault names it.
fn kind_word(kind: Kind) -> Text {
    if kind == .api_key {
        "api_key"
    } else if kind == .note {
        "note"
    } else if kind == .wifi { "wifi" } else if kind == .secret { "secret" } else { "login" }
}

/// The kind a vault's word names.
fn kind_of(word: Text) -> Kind {
    if word == "api_key" or word == "api-key" {
        .api_key
    } else if word == "note" {
        .note
    } else if word == "wifi" {
        .wifi
    } else if word == "secret" or word == "other" { .secret } else { .login }
}

/// Starts an item: an entry of the vault and the item that lists it, named by what is typed
/// first. Its secret is typed on the system's card, never here.
primary action new_item(v: Vault, title: Text = "") -> Item uses random (
    icon: .plus,
    label: "New item",
    key: "Ctrl+N",
    did: "added an item",
) {
    let id = random.id()
    vault.keep(vault.vault, id: id, kind: "login", title: title.trim())
    v.items.insert(Item(entry: id, title: title.trim()))
}

/// Adds a login in one step: what an agent, a script or the VM's own end-to-end check asks of the
/// vault. The person types its password on the system's card, so whoever asked never holds it,
/// and the vault keeps it. An address is checked as a web address.
action save_login(v: Vault, name: Text, origin: Text, username: Text) -> Item uses random (
    icon: .key,
    label: "Save a login",
    did: "saved a login",
    takes: [("Password", .secret)],
) {
    require not name.trim().is_empty() "A login needs a name"
    require origin.trim().is_empty() or origin_of(origin) != "" "{origin.trim()} is not a web address, such as https://example.com"
    let id = random.id()
    let places: List<Text> = if origin.trim().is_empty() { [] } else { [origin_of(origin)] }
    vault.keep(
        vault.vault,
        id: id,
        kind: "login",
        title: name.trim(),
        username: username.trim(),
        origins: places,
    )
    let why = keep_typed(id, "password")
    v.notice = why
    v.items.insert(Item(entry: id, title: name.trim(), kind: .login))
}

/// The vault's logins, by name and by the entry that holds each: what a script, an agent or
/// another device's window lists. The passwords are never part of it.
action list_logins(v: Vault) -> LoginList (icon: .list, label: "List logins") {
    LoginList(
        logins: v.items
            .filter(i -> i.kind == .login and i.entry != "")
            .sort_by(i -> i.title.lower())
            .map(i -> LoginEntry(item: i.entry, name: i.title)),
    )
}

/// What a list of logins says: each by the vault entry that holds it, and its name.
type LoginList {
    logins: List<LoginEntry>
}

/// One login in such a list.
type LoginEntry {
    item: Text
    name: Text
}

/// Hands what the person typed on the system's card to the vault as `part` of the entry `id`:
/// the host passes it on, so it never becomes a value here. Answers why it could not, or nothing.
action keep_typed(id: Text, part: Text) -> Text (icon: .key, label: "Keep what was typed") {
    match engine.call("vault-keep-typed", json.encode(TypedAsk(id: id, part: part))) {
        .ok(_) => "",
        .err(why) => why,
    }
}

/// Writes what is safe to say of an item into its vault entry: who it signs in as and where,
/// with the item's name, kind and tags.
action keep_facts(item: Item, username: Text, origins: List<Text>) (
    icon: .key,
    label: "Write the item into the vault",
) {
    vault.keep(
        vault.vault,
        id: item.entry,
        kind: kind_word(item.kind),
        title: item.title.trim(),
        username: username.trim(),
        origins: origins,
        tags: item.tags.map(t -> t),
    )
}

/// The places an item's entry belongs to now.
fn places_of(item: Item) -> List<Text> {
    if let e = entry_of(item) { e.origins } else { [] }
}

/// The name an item's entry signs in with now.
fn username_of(item: Item) -> Text {
    if let e = entry_of(item) { e.username } else { "" }
}

/// Writes who the item signs in as.
action set_username(item: Item, username: Text) (icon: .user, label: "Change the username") {
    keep_facts(item, username, places_of(item))
}

/// Changes the item's password, key or secret: the person types it on the system's card, and the
/// vault keeps it. Nothing of it reaches this app's code, its records or history.
action change_secret(item: Item) -> Text (
    icon: .key,
    label: "Change the {secret_label(item.kind).lower()}",
    did: "changed a password",
    takes: [("Password", .secret)],
) {
    keep_typed(item.entry, "password")
}

/// Writes the item's note: typed on the system's card, kept in the vault.
action write_note(item: Item) -> Text (
    icon: .file_text,
    label: "Write the note",
    did: "wrote a note",
    takes: [("Note", .secret)],
) {
    keep_typed(item.entry, "notes")
}

/// Adds a field to the item, such as a PIN: its value typed on the system's card.
action add_field(item: Item, label: Text = "") -> Text (
    icon: .plus,
    label: "Add a field",
    takes: [("Value", .secret)],
) {
    require not label.trim().is_empty() "A field needs a label"
    keep_typed(item.entry, "field:{label.trim()}")
}

/// Takes a field off the item.
action remove_field(item: Item, label: Text) -> Text (icon: .x, label: "Remove the field") {
    match engine.call(
        "vault-forget-field",
        json.encode(TypedAsk(id: item.entry, part: "field:{label}")),
    ) {
        .ok(_) => "",
        .err(why) => why,
    }
}

/// Draws a strong password for the item where the vault is: 20 characters by default, with a
/// capital, a small letter, a digit and a symbol. Nobody types or sees it.
action generate_password(
    item: Item,
    length: Int = 20,
    capitals: Bool = true,
    digits: Bool = true,
    symbols: Bool = true,
) (icon: .refresh, label: "Generate strong password", did: "generated a password") {
    require length >= 12 and length <= 128 "A password is 12 to 128 characters long"
    vault.generate(
        vault.vault,
        id: item.entry,
        length: length,
        capitals: capitals,
        digits: digits,
        symbols: symbols,
    )
}

/// Puts an item in the person's favorites, or takes it out.
action favorite(item: Item, favorite: Bool = not item.favorite) (
    label: if favorite { "Remove from favorites" } else { "Add to favorites" },
    icon: .star,
    on: item.favorite,
) {
    item.favorite = favorite
}

/// Gives an item a tag, as the words beside its tags are typed.
action add_tag(item: Item, tag: Text = "") (icon: .tag, label: "Add a tag") {
    require not tag.trim().is_empty() "A tag needs a name"
    item.tags.add(tag.trim())
    keep_facts(item, username_of(item), places_of(item))
}

/// Takes a tag off an item.
action remove_tag(item: Item, tag: Text) (icon: .x, label: "Remove the tag") {
    item.tags.remove(tag)
    keep_facts(item, username_of(item), places_of(item))
}

/// Gives an item an address it signs in to, checked as a web address, as typed beside them.
action add_origin(item: Item, origin: Text = "") (icon: .globe, label: "Add a web address") {
    require not origin.trim().is_empty() "Add the address"
    require origin_of(origin) != "" "{origin.trim()} is not a web address, such as https://example.com"
    let held = places_of(item)
    let added = origin_of(origin)
    let places = if held.contains(added) { held } else { [held, [added]].flat_map(x -> x) }
    keep_facts(item, username_of(item), places)
}

/// Takes an address off an item.
action remove_origin(item: Item, origin: Text) (icon: .x, label: "Remove the address") {
    keep_facts(item, username_of(item), places_of(item).filter(o -> o != origin))
}

/// Renames an item, in its record and its vault entry.
action rename(item: Item, title: Text) (icon: .pencil, label: "Rename") {
    require not title.trim().is_empty() "Add a name so you can find this item"
    item.title = title.trim()
    keep_facts(item, username_of(item), places_of(item))
}

/// Changes what kind of secret an item is.
action set_kind(item: Item, kind: Kind) (icon: .shield, label: "Change the kind") {
    item.kind = kind
    keep_facts(item, username_of(item), places_of(item))
}

/// Copies the item's secret to the person's clipboard, which forgets it again after a while.
/// Copying reads the vault and keeps nothing.
action copy_secret(item: Item) uses clipboard (
    icon: .copy,
    label: "Copy {secret_label(item.kind).lower()}",
) {
    if let e = entry_of(item) { clipboard.copy_secret(secret_value(e)) }
}

/// Copies what identifies the person to the account: a login's username, an API key's account,
/// a Wi-Fi network's name.
action copy_identity(item: Item) uses clipboard (
    icon: .user,
    label: "Copy {identity_label(item.kind).lower()}",
) {
    if let e = entry_of(item) {
        let who = identity(e)
        if not who.is_empty() { clipboard.copy(who) }
    }
}

/// Copies the addresses the item signs in to, one a line.
action copy_origins(item: Item) uses clipboard (icon: .copy, label: "Copy web addresses") {
    let places = places_of(item)
    if not places.is_empty() { clipboard.copy(places.join("\n")) }
}

/// Deletes an item, and the entry it stands for, from the vault.
destructive action delete(v: Vault, item: Item) (
    icon: .trash,
    key: "Delete",
    did: "deleted an item",
) {
    if item.entry != "" {
        vault.forget(vault.vault, id: item.entry)
    }
    v.items.remove(item)
}

/// Brings the vault and its items together: an entry kept elsewhere (a mail account's password,
/// a connector's token, one another device added) gets an item, an item's name, kind and tags
/// reach its entry, and what an older Passwords kept in its own records moves into the vault and
/// leaves the record. Runs while Passwords is open, and changes nothing when all is settled.
action settle(v: Vault) uses random (icon: .refresh, label: "Settle the vault") {
    if not v.pending.is_empty() and v.pending.any(r -> r.title != "" and v.importing == "") {
        clear_pending(v)
    }
    for item in v.items {
        if holds_old_secrets(item) {
            carry(item)
        } else if let e = entry_of(item) {
            if e.title != item.title or e.kind != kind_word(item.kind) or not same_tags(e, item) {
                keep_facts(item, e.username, e.origins)
            }
        }
    }
    let known = v.items.map(i -> i.entry)
    for e in vault.vault.entries {
        if not known.contains(e.id) {
            let made = v.items.insert(Item(entry: e.id, title: e.title, kind: kind_of(e.kind)))
            for tag in e.tags {
                made.tags.add(tag)
            }
        }
    }
}

/// The vault settles every few seconds while Passwords is open.
trigger settling(v: Vault) every 5s: settle(v)

/// Whether an item still keeps what an older Passwords put in its record.
fn holds_old_secrets(item: Item) -> Bool {
    not item.password.is_empty()
        or not item.notes.is_empty()
        or not item.username.is_empty()
        or not item.origins.is_empty()
        or not item.fields.keys().is_empty()
}

/// Whether an entry carries the item's tags.
fn same_tags(e: vault.Entry, item: Item) -> Bool {
    e.tags.len() == item.tags.len() and e.tags.all(t -> item.tags.contains(t))
}

/// Moves what an item's record keeps into the vault, once, and empties the record: an entry the
/// vault already keeps under the item's id stays as it is.
action carry(item: Item) uses random (icon: .key, label: "Move the secret into the vault") {
    if item.entry == "" {
        item.entry = random.id()
    }
    vault.carry(
        vault.vault,
        id: item.entry,
        kind: kind_word(item.kind),
        title: item.title,
        username: item.username,
        origins: item.origins.map(o -> o),
        tags: item.tags.map(t -> t),
        password: item.password,
        notes: item.notes,
        fields: item.fields.keys().map(k -> vault.Field(label: k, value: item.fields.get(k) ?? "")),
    )
    item.password = ""
    item.notes = ""
    item.username = ""
    for o in item.origins.map(o -> o) {
        item.origins.remove(o)
    }
    for k in item.fields.keys() {
        item.fields.remove(k)
    }
}

/// Whether an entry holds a password a person chose: a login or a Wi-Fi key, not an issued API
/// key or licence code.
// ---------------------------------------------------------------- what the window reads
fn is_password(e: vault.Entry) -> Bool {
    (e.kind == "login" or e.kind == "wifi") and not e.password.is_empty()
}

/// A password a person chose is weak when it is shorter than 12 characters or misses a capital,
/// a small letter or a digit.
fn is_weak(password: Text) -> Bool {
    password.len() < 12
        or not has_class(password, 1)
        or not has_class(password, 2)
        or not has_class(password, 3)
}

/// Whether `text` holds a character of the class `of`: 1 a capital, 2 a small letter, 3 a digit.
fn has_class(text: Text, of: Int) -> Bool {
    let seen = false
    for c in text.split("") {
        if of == 1 and c.upper() == c and c.lower() != c {
            seen = true
        } else if of == 2 and c.lower() == c and c.upper() != c {
            seen = true
        } else if of == 3 and not c.is_empty() and "0123456789".contains(c) {
            seen = true
        }
    }
    seen
}

/// Whether another entry holds the same password, so the password is reused.
fn is_reused(entries: List<vault.Entry>, e: vault.Entry) -> Bool {
    entries.any(other -> other.id != e.id and is_password(other) and other.password == e.password)
}

/// Whether the entry was last changed more than a year ago.
fn is_old(e: vault.Entry) -> Bool {
    e.stale
}

/// What is wrong with the entry's password, in one phrase: "Reused and weak", "Not changed in a
/// year". Empty for a password nothing is wrong with.
fn reasons(v: Vault, entries: List<vault.Entry>, e: vault.Entry) -> Text {
    let said: List<Text> = []
    if is_reused(entries, e) {
        said = [said, ["reused"]].flat_map(x -> x)
    }
    if is_weak(e.password) {
        said = [said, ["weak"]].flat_map(x -> x)
    }
    if is_old(e) {
        said = [said, ["not changed in a year"]].flat_map(x -> x)
    }
    let first = said.first() ?? ""
    let second = said.at(1) ?? ""
    let third = said.at(2) ?? ""
    let joined = if second == "" {
        first
    } else if third == "" { "{first} and {second}" } else { "{first}, {second} and {third}" }
    capitalized(joined)
}

/// `text` with its first letter a capital.
fn capitalized(text: Text) -> Text {
    if text.is_empty() {
        ""
    } else {
        "{text.slice(0, 1).upper()}{text.slice(1, text.len())}"
    }
}

/// What a kind is called in the window.
fn kind_label(kind: Kind) -> Text {
    if kind == .login {
        "Login"
    } else if kind == .api_key {
        "API key"
    } else if kind == .note {
        "Secure note"
    } else if kind == .wifi { "Wi-Fi network" } else { "Secret" }
}

/// What the secret of a kind is called in a sentence.
fn secret_label(kind: Kind) -> Text {
    if kind == .api_key {
        "API key"
    } else if kind == .wifi or kind == .login {
        "Password"
    } else if kind == .note { "Note" } else { "Secret" }
}

/// What identifies the person to a kind of account: a login's username, an API key's account, a
/// Wi-Fi network's name.
fn identity_label(kind: Kind) -> Text {
    if kind == .api_key { "Account" } else if kind == .wifi { "Network" } else { "Username" }
}

/// What the addresses of a kind are called, in one or several.
fn origin_label(kind: Kind, count: Int) -> Text {
    if kind == .api_key { "Service" } else if count > 1 { "Websites" } else { "Website" }
}

/// The identity a row shows: the network for a Wi-Fi password, else the username.
fn identity(e: vault.Entry) -> Text {
    if e.kind == "wifi" {
        e.fields.find(f -> f.label == "network")?.value ?? e.username
    } else { e.username }
}

/// The secret an entry hands out: its password, or a secure note's text.
fn secret_value(e: vault.Entry) -> Text {
    if e.kind == "note" { e.notes } else { e.password }
}

/// The one secondary fact a row shows: who it signs in as, else the site, else its kind.
fn summary(item: Item) -> Text {
    if let e = entry_of(item) {
        let who = identity(e)
        if not who.is_empty() {
            return who
        }
        if let site = e.origins.first() {
            return host_of(site)
        }
    }
    kind_label(item.kind)
}

/// `bank.example` for `https://bank.example`.
fn host_of(origin: Text) -> Text {
    let at = origin.find("://")
    let rest = if at == .none {
        origin
    } else {
        origin.slice((at ?? 0) + 3, origin.len())
    }
    if rest.ends_with("/") { rest.slice(0, rest.len() - 1) } else { rest }
}

/// A web address as the vault keeps it: lower case, with `https://` where none was written and
/// no trailing slash. Empty for what is no web address.
fn origin_of(value: Text) -> Text {
    let said = value.trim().lower()
    if said.is_empty() or said.contains(" ") {
        return ""
    }
    let full = if said.starts_with("https://") or said.starts_with("http://") {
        said
    } else { "https://{said}" }
    let site = if full.starts_with("https://") {
        full.slice(8, full.len())
    } else if full.starts_with("http://") {
        full.slice(7, full.len())
    } else {
        return ""
    }
    let bare = if site.ends_with("/") { site.slice(0, site.len() - 1) } else { site }
    if bare.is_empty() or not bare.contains(".") or bare.contains("/") {
        return ""
    }
    if full.starts_with("https://") { "https://{bare}" } else { "http://{bare}" }
}

/// The items in the order the window lists them, by name ignoring case: those whose entry the
/// vault reads now.
fn listed(v: Vault) -> List<Item> {
    v.items.filter(i -> entry_of(i) != .none).sort_by(i -> i.title.lower())
}

/// The vault window: its places in the sidebar, the items of the one chosen in the overview, and
/// the open item, the review or the transfers in the content.
// ---------------------------------------------------------------- the window
view vault_window(v: Vault) {
    // The place chosen: a kind of item, the favorites, or one of the tools.
    state place: Text = "all"
    state opened: Option<Item> = listed(v).first()
    state searched: Text = ""
    // What the window asks over the open item: its secret shown, or it about to be deleted.
    state showing: Option<Item> = .none
    state ask_delete: Option<Item> = .none
    let all = listed(v)
    let here = if place == "favorites" {
        all.filter(i -> i.favorite)
    } else if place == "login" {
        all.filter(i -> i.kind == .login)
    } else if place == "api_key" {
        all.filter(i -> i.kind == .api_key)
    } else if place == "note" {
        all.filter(i -> i.kind == .note)
    } else if place == "wifi" {
        all.filter(i -> i.kind == .wifi)
    } else if place == "secret" { all.filter(i -> i.kind == .secret) } else { all }
    let found = if searched.is_empty() { here } else { here.matching(searched) }
    let title = if place == "favorites" {
        "Favorites"
    } else if place == "login" {
        "Logins"
    } else if place == "api_key" {
        "API keys"
    } else if place == "note" {
        "Secure notes"
    } else if place == "wifi" {
        "Wi-Fi"
    } else if place == "secret" {
        "Other secrets"
    } else if place == "review" {
        "Password review"
    } else if place == "transfer" { "Import and export" } else { "All items" }
    ui.NavShell(
        content_kind: .page,
        sidebar_title: "Passwords",
        overview_title: title,
        overview_count: if place == "review" or place == "transfer" {
            .none
        } else { .some(found.len()) },
    ) {
        on search(q: Text): set(searched, q)
        sidebar: {
            ui.NavList(selected: place) {
                ui.NavItem("all", "All items", icon: .layout_grid, count: all.len()) {
                    on select: set(place, "all").then(set(opened, all.first()))
                }
                ui.NavItem(
                    "favorites",
                    "Favorites",
                    icon: .star,
                    count: all.filter(i -> i.favorite).len(),
                ) {
                    on select: set(place, "favorites").then(set(opened, .none))
                }
                ui.NavItem(
                    "login",
                    "Logins",
                    icon: .globe,
                    count: all.filter(i -> i.kind == .login).len(),
                ) {
                    on select: set(place, "login").then(set(opened, .none))
                }
                ui.NavItem(
                    "api_key",
                    "API keys",
                    icon: .code,
                    count: all.filter(i -> i.kind == .api_key).len(),
                ) {
                    on select: set(place, "api_key").then(set(opened, .none))
                }
                ui.NavItem(
                    "note",
                    "Secure notes",
                    icon: .file_text,
                    count: all.filter(i -> i.kind == .note).len(),
                ) {
                    on select: set(place, "note").then(set(opened, .none))
                }
                ui.NavItem(
                    "wifi",
                    "Wi-Fi",
                    icon: .wifi,
                    count: all.filter(i -> i.kind == .wifi).len(),
                ) {
                    on select: set(place, "wifi").then(set(opened, .none))
                }
                ui.NavItem(
                    "secret",
                    "Other secrets",
                    icon: .shield,
                    count: all.filter(i -> i.kind == .secret).len(),
                ) {
                    on select: set(place, "secret").then(set(opened, .none))
                }
                ui.NavSection("Tools") {
                    ui.NavItem("review", "Password review", icon: .shield_check) {
                        on select: set(place, "review").then(set(opened, .none))
                    }
                    ui.NavItem("transfer", "Import and export", icon: .arrow_up_down) {
                        on select: set(place, "transfer").then(set(opened, .none))
                    }
                }
            }
        }
        sidebar_actions: {
            ui.MenuButton("File", icon: .file_plus) {
                ui.MenuItem("Import from CSV…", icon: .file_plus) {
                    on choose: file.choose(preview_csv(v), accept: ["text/csv", ".csv"])
                }
                ui.MenuItem("Export to CSV…", icon: .download) {
                    on choose: file.save(export_csv(v), name: "passwords.csv")
                }
                ui.MenuItem("Back up vault…", icon: .lock) {
                    on choose: file.save(export_backup(v), name: "passwords-backup.tilopw")
                }
                ui.MenuItem("Restore from backup…", icon: .lock_open) {
                    on choose: file.choose(restore_backup(v), accept: [".tilopw"])
                }
                ui.MenuItem("Import this device's vault", icon: .key) {
                    on choose: absorb_device(v)
                }
            }
        }
        overview: {
            ui.Docked {
                foot: {
                    stack {
                        std.add_entry("New item", new_item(v))
                    }
                }
                if place == "review" or place == "transfer" {
                    text(
                        "The tool you chose is in the content beside the list.",
                        role: .caption,
                        secondary: true,
                    )
                } else if found.is_empty() {
                    ui.EmptyState(
                        if all.is_empty() {
                            "Your vault is empty"
                        } else if place == "favorites" {
                            "No favorites yet"
                        } else { "Nothing of this kind yet" },
                        icon: if place == "favorites" { .star } else { .shield },
                        inline: true,
                    )
                } else {
                    ui.VirtualList(selection_follows_focus: true, in_column: true) {
                        on select(x: Item): set(opened, x).then(set(showing, .none))
                        for i in found {
                            ui.ListItem(item: i, opens: false) {
                                ui.ListRow(
                                    title: i.title,
                                    secondary: summary(i),
                                    item: i,
                                    selected: if let chosen = opened { chosen == i } else { false },
                                    icon: i.kind.icon(),
                                    mark: if i.favorite { .star },
                                )
                            }
                        }
                    }
                }
            }
        }
        content: {
            if place == "review" {
                review_page(v)
            } else if place == "transfer" {
                transfer_page(v)
            } else if let item = opened {
                if let e = entry_of(item) {
                    item_page(v, item, e, showing, ask_delete)
                } else {
                    ui.EmptyState("This item is no longer in the vault", icon: .shield)
                }
            } else {
                ui.EmptyState(
                    if all.is_empty() { "Nothing saved yet" } else { "No item selected" },
                    message: .some(
                        if all.is_empty() {
                            "Logins, secure notes, Wi-Fi passwords and API keys you save are kept here."
                        } else { "" },
                    ),
                    icon: .shield,
                )
            }
        }
    }
    if let item = ask_delete {
        ui.ModalDialog(
            title: "Delete “{item.title}”?",
            open: true,
            primary: "Delete item",
            secondary: "Cancel",
            destructive: true,
        ) {
            on act_primary: delete(v, item).then(set(ask_delete, .none)).then(set(showing, .none))
            on act_secondary: set(ask_delete, .none)
            on dismiss: set(ask_delete, .none)
            body: {
                text("The item is removed from the vault and cannot be restored.", role: .body)
            }
        }
    }
}

/// An item's own page: its name, what it signs in as, its secret, the addresses it signs in to,
/// its tags, notes, custom fields and when it last changed, with the controls that write them.
/// The secret shows only while the person asked for it.
// ---------------------------------------------------------------- the open item
view item_page(
    v: Vault,
    item: Item,
    e: vault.Entry,
    showing: State<Option<Item>>,
    ask_delete: State<Option<Item>>,
) {
    state draft_title: Text = item.title
    state draft_username: Text = e.username
    let shown = if let s = showing { s == item } else { false }
    stack(gap: .group) {
        row(gap: .related, align: .center) {
            ui.TextInput(bind: draft_title, title: true, placeholder: "What it is called", fill) {
                on submit(t: Text): rename(item, t)
            }
            ui.Button(
                if item.favorite { "Remove from favorites" } else { "Add to favorites" },
                icon: .star,
                icon_only: true,
                role: .plain,
                toggled: item.favorite,
            ) {
                on press: favorite(item)
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.EnumField(bind: item.kind, label: "Kind")
        }
        std.body() {
            ui.PropertyList {
                if item.kind != .note {
                    ui.Property(identity_label(item.kind), identity(e), copyable: true)
                }
                ui.Property(
                    secret_label(item.kind),
                    if secret_value(e).is_empty() {
                        "Not set"
                    } else if shown {
                        secret_value(e)
                    } else { "••••••••••••" },
                )
                if not e.origins.is_empty() {
                    ui.Property(origin_label(item.kind, e.origins.len()), e.origins.join("\n"))
                }
                if not item.tags.is_empty() {
                    ui.Property("Tags", item.tags.map(t -> t).join(", "))
                }
                if item.kind != .note and not e.notes.trim().is_empty() {
                    ui.Property("Notes", if shown { e.notes } else { "Hidden" })
                }
                for f in e.fields {
                    ui.Property(f.label, if shown { f.value } else { "••••••" })
                }
            }
            if let changed = Time.from_wall("UTC", e.changed.slice(0, 19)) {
                row(gap: .related, align: .center) {
                    text("Last changed", role: .label)
                    ui.Time(at: changed, past: true)
                }
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Copy {secret_label(item.kind).lower()}", icon: .copy) {
                    on press: copy_secret(item)
                }
                if not identity(e).is_empty() and item.kind != .note {
                    ui.Button("Copy {identity_label(item.kind).lower()}", icon: .copy) {
                        on press: copy_identity(item)
                    }
                }
                if not e.origins.is_empty() {
                    ui.Button("Copy web addresses", icon: .copy) {
                        on press: copy_origins(item)
                    }
                }
                ui.Button(if shown { "Hide" } else { "Reveal" }, icon: .eye) {
                    on press: if shown { set(showing, .none) } else { set(showing, item) }
                }
                if item.kind == .note {
                    ui.Button("Write the note", icon: .file_text) {
                        on press: write_note(item)
                    }
                } else {
                    ui.Button("Change the {secret_label(item.kind).lower()}", icon: .key) {
                        on press: change_secret(item)
                    }
                    ui.Button("Generate strong password", icon: .refresh) {
                        on press: generate_password(item)
                    }
                }
            }
            if item.kind != .note {
                section(identity_label(item.kind)) {
                    row(gap: .related, align: .center) {
                        ui.TextInput(
                            bind: draft_username,
                            placeholder: "Who it signs in as",
                            fill,
                        ) {
                            on submit(t: Text): set_username(item, t)
                        }
                    }
                }
            }
            section("Tags") {
                if item.tags.is_empty() {
                    text("No tags yet.", role: .caption, secondary: true)
                } else {
                    row(gap: .tight, wrap: true) {
                        for tag in item.tags {
                            ui.Chip(tag, icon: .tag) {
                                on remove: remove_tag(item, tag)
                            }
                        }
                    }
                }
                std.add_entry("Add a tag", add_tag(item))
            }
            if item.kind == .login or item.kind == .api_key {
                section(origin_label(item.kind, 2)) {
                    for origin in e.origins {
                        row(gap: .tight, align: .center) {
                            ui.Link(origin, label: host_of(origin))
                            spacer
                            ui.Button(
                                "Remove the address",
                                icon: .x,
                                icon_only: true,
                                role: .plain,
                            ) {
                                on press: remove_origin(item, origin)
                            }
                        }
                    }
                    std.add_entry("Add a web address", add_origin(item))
                }
            }
            section("Fields") {
                for f in e.fields {
                    row(gap: .tight, align: .center) {
                        text(f.label, role: .label)
                        spacer
                        ui.Button("Remove the field", icon: .x, icon_only: true, role: .plain) {
                            on press: remove_field(item, f.label)
                        }
                    }
                }
                std.add_entry("Add a field, such as PIN", add_field(item))
            }
            row(gap: .related, wrap: true) {
                ui.Button("Delete item", icon: .trash, role: .destructive) {
                    on press: set(ask_delete, item)
                }
            }
        }
    }
}

/// The review: the passwords that are reused, weak or old, each opening its item.
// ---------------------------------------------------------------- the review
view review_page(v: Vault) {
    let entries = vault.vault.entries
    let checked = entries.filter(e -> is_password(e))
    let total = checked.len()
    let reused = checked.filter(e -> is_reused(entries, e)).len()
    let weak = checked.filter(e -> is_weak(e.password)).len()
    let old = checked.filter(e -> is_old(e)).len()
    let troubled = checked.filter(e -> is_reused(entries, e) or is_weak(e.password) or is_old(e))
    ui.Scroll {
        page {
            stack(gap: .group) {
                text("Password review", role: .title)
                if total > 0 {
                    ui.StatGroup {
                        ui.StatTile(
                            "Reused",
                            reused.to_text(),
                            tone: if reused == 0 { .success } else { .warning },
                        )
                        ui.StatTile(
                            "Weak",
                            weak.to_text(),
                            tone: if weak == 0 { .success } else { .warning },
                        )
                        ui.StatTile(
                            "Not changed in a year",
                            old.to_text(),
                            tone: if old == 0 { .success } else { .warning },
                        )
                    }
                }
                if total == 0 {
                    ui.Section("Nothing to check yet") {
                        text(
                            "Save a password and the review checks it for reuse, weakness and age.",
                            role: .body,
                        )
                    }
                } else if troubled.is_empty() {
                    ui.Section("Nothing to fix") {
                        text("Every saved password is unique, strong and recent.", role: .body)
                    }
                } else {
                    ui.Section("Needs attention") {
                        for e in troubled {
                            if let item = v.items.find(i -> i.entry == e.id) {
                                ui.ListRow(
                                    title: item.title,
                                    secondary: reasons(v, entries, e),
                                    item: item,
                                    icon: item.kind.icon(),
                                )
                            }
                        }
                    }
                }
                ui.Section("About this review") {
                    ui.PropertyList {
                        ui.Property("Passwords checked", total.to_text())
                        ui.Property("Checked against", "Only the passwords in this vault")
                    }
                }
            }
        }
    }
}

/// The transfer page: an encrypted backup, the browser's CSV, and what this device's own vault
/// holds.
// ---------------------------------------------------------------- import and export
view transfer_page(v: Vault) {
    ui.Scroll {
        page {
            stack(gap: .group) {
                text("Import and export", role: .title)
                if v.notice != "" {
                    ui.Banner(v.notice, kind: .info)
                }
                ui.Section("Encrypted backup") {
                    text(
                        "A backup keeps every item, note and field, encrypted with a password you type on the system's card: at least 12 characters, typed twice.",
                        role: .body,
                    )
                    row(gap: .related, wrap: true) {
                        ui.Button("Back up vault…", icon: .lock) {
                            on press: file.save(export_backup(v), name: "passwords-backup.tilopw")
                        }
                        ui.Button("Restore from backup…", icon: .lock_open) {
                            on press: file.choose(restore_backup(v), accept: [".tilopw"])
                        }
                    }
                }
                ui.Section("Browser passwords") {
                    text(
                        "Move logins between Passwords and a web browser with a CSV file. A CSV file holds every password unencrypted: keep it only as long as the move takes.",
                        role: .body,
                    )
                    row(gap: .related, wrap: true) {
                        ui.Button("Import from CSV…", icon: .file_plus) {
                            on press: file.choose(preview_csv(v), accept: ["text/csv", ".csv"])
                        }
                        ui.Button("Export to CSV…", icon: .download) {
                            on press: file.save(export_csv(v), name: "passwords.csv")
                        }
                    }
                }
                ui.Section("This device's vault") {
                    text(
                        "Logins this device kept before the vault was shared come into the workspace once. The authentication material of this machine and its Wi-Fi networks stay on it.",
                        role: .body,
                    )
                    ui.Button("Import this device's vault", icon: .key) {
                        on press: absorb_device(v)
                    }
                }
                if not v.pending.is_empty() {
                    import_preview(v)
                }
            }
        }
    }
}

/// The rows an import is about to bring, the ones already in the vault, and the two answers.
view import_preview(v: Vault) {
    let fresh = v.pending.filter(r -> not r.skip)
    let known = v.pending.filter(r -> r.skip)
    ui.ModalDialog(
        title: if fresh.is_empty() {
            "Nothing new to import"
        } else if fresh.len() == 1 { "Import 1 login?" } else { "Import {fresh.len()} logins?" },
        open: true,
        primary: if fresh.is_empty() { "Close" } else { "Import" },
        secondary: if fresh.is_empty() { .none } else { .some("Cancel") },
    ) {
        on act_primary: if fresh.is_empty() { clear_import(v) } else { apply_import(v) }
        on act_secondary: clear_import(v)
        on dismiss: clear_import(v)
        body: {
            if not known.is_empty() {
                text(
                    if known.len() == 1 {
                        "1 login is already in your vault and will be skipped."
                    } else {
                        "{known.len()} logins are already in your vault and will be skipped."
                    },
                    role: .body,
                )
            }
            for row in fresh.take(5) {
                ui.ListRow(title: row.title, secondary: row.username, icon: .globe)
            }
            if fresh.len() > 5 {
                text("And {fresh.len() - 5} more", role: .caption, secondary: true)
            }
        }
    }
}

/// Reads the browser's CSV the person chose into the vault's waiting rows, each marked when the
/// vault already holds that login. The passwords stay in the file.
action preview_csv(v: Vault, file: File) (icon: .file_plus, label: "Import from CSV") {
    clear_pending(v)
    v.notice = ""
    v.importing = ""
    match engine.call("vault-preview-csv", json.encode(AskedFile(url: file.url))) {
        .ok(text) => match json.decode<RowsAnswer>(text) {
            .ok(answer) => {
                for row in answer.rows {
                    v.pending.insert(
                        ImportRow(
                            title: row.title,
                            username: row.username,
                            origin: row.origin,
                            skip: holds_login(row.title, row.username, row.origin),
                        ),
                    )
                }
                v.importing = file.url
                if v.pending.is_empty() {
                    v.notice = "No logins with passwords were found"
                }
            }
            .err(why) => {
                v.notice = why
            }
        },
        .err(why) => {
            v.notice = why
        }
    }
}

/// Whether the vault already holds that login: the same name and username, and the same address.
fn holds_login(title: Text, username: Text, origin: Text) -> Bool {
    let wanted = origin_of(origin)
    vault.vault.entries.any(
        e -> e.title.lower() == title.trim().lower()
            and e.username.lower() == username.trim().lower()
            and if wanted == "" { e.origins.is_empty() } else { e.origins.contains(wanted) },
    )
}

/// Brings the rows the person agreed to into the vault, skipping the ones the vault holds. The
/// vault reads their passwords from the file.
action apply_import(v: Vault) (icon: .check, label: "Import the logins") {
    let rows = v.pending.map(r -> r)
    let skip = rows
        .filter(r -> r.skip or holds_login(r.title, r.username, r.origin))
        .map(r -> rows.index_of(r) ?? 0)
    match engine.call("vault-import-csv", json.encode(ImportAsk(url: v.importing, skip: skip))) {
        .ok(text) => match json.decode<CountAnswer>(text) {
            .ok(answer) => {
                v.notice = if skip.is_empty() {
                    if answer.imported == 1 {
                        "Imported 1 login"
                    } else { "Imported {answer.imported} logins" }
                } else {
                    "Imported {answer.imported} logins and skipped {skip.len()}"
                }
            }
            .err(why) => {
                v.notice = why
            }
        },
        .err(why) => {
            v.notice = why
        }
    }
    clear_pending(v)
}

/// Puts the waiting rows away.
action clear_import(v: Vault) (icon: .x, label: "Cancel the import") {
    clear_pending(v)
}

/// Empties the vault's waiting rows.
action clear_pending(v: Vault) -> Bool (icon: .x, label: "Clear the waiting rows") {
    let rows = v.pending.map(r -> r)
    for row in rows {
        v.pending.remove(row)
    }
    v.importing = ""
    true
}

/// The vault as a browser's CSV: the person chooses where it goes.
action export_csv(v: Vault) -> File (icon: .download, label: "Export to CSV") {
    match engine.call("vault-export-csv", "") {
        .ok(text) => match json.decode<UrlAnswer>(text) {
            .ok(answer) => File.at("passwords.csv", answer.url),
            .err(why) => File.of_text("The export could not be made.txt", why),
        },
        .err(why) => File.of_text("The export could not be made.txt", why),
    }
}

/// The vault as an encrypted backup under the password the person types twice on the system's
/// card: the person chooses where it goes.
action export_backup(v: Vault) -> File (
    icon: .lock,
    label: "Back up the vault",
    takes: [("Backup password", .secret), ("The backup password again", .secret)],
) {
    match engine.call("vault-export-backup", "") {
        .ok(text) => match json.decode<UrlAnswer>(text) {
            .ok(answer) => File.at("passwords-backup.tilopw", answer.url),
            .err(why) => File.of_text("The backup could not be made.txt", why),
        },
        .err(why) => File.of_text("The backup could not be made.txt", why),
    }
}

/// Brings the entries a backup holds into the vault with the password the person types on the
/// system's card, and answers how many came in. An entry the vault already keeps is left as it
/// is, so restoring twice adds nothing.
action restore_backup(v: Vault, file: File) -> Int (
    icon: .lock_open,
    label: "Restore from a backup",
    takes: [("Backup password", .secret)],
) {
    match engine.call("vault-restore-backup", json.encode(AskedFile(url: file.url))) {
        .ok(text) => match json.decode<RestoredAnswer>(text) {
            .ok(answer) => {
                v.notice = if answer.restored == 1 {
                    "Restored 1 item"
                } else { "Restored {answer.restored} items" }
                answer.restored
            }
            .err(why) => {
                v.notice = why
                0
            }
        },
        .err(why) => {
            v.notice = why
            0
        }
    }
}

/// Takes into the vault what this device's secret service holds and a workspace may share. The
/// device's authentication material and its Wi-Fi networks stay where they are, and each entry
/// leaves the device's service once the vault keeps it.
// ---------------------------------------------------------------- this device's vault
action absorb_device(v: Vault) (icon: .key, label: "Import this device's vault") {
    match engine.call("vault-absorb-device", "") {
        .ok(text) => match json.decode<CarriedAnswer>(text) {
            .ok(answer) => {
                v.notice = if answer.error != "" {
                    answer.error
                } else if answer.carried == 1 {
                    "Imported 1 login from this device"
                } else if answer.carried > 1 {
                    "Imported {answer.carried} logins from this device"
                } else { "" }
            }
            .err(why) => {
                v.notice = why
            }
        },
        .err(why) => {
            v.notice = why
        }
    }
}

/// Which secret of which entry the host hands the vault what the person typed for.
// ---------------------------------------------------------------- what the host is asked
type TypedAsk {
    id: Text
    part: Text
}

/// What the CSV engine is asked: the file the person chose.
type AskedFile {
    url: Text
}

/// Which rows of the chosen file come into the vault: all but those skipped, by their place.
type ImportAsk {
    url: Text
    skip: List<Int>
}

/// One row of a browser's CSV, without its secrets.
type CsvRow {
    title: Text
    username: Text
    origin: Text
}

/// The rows a CSV holds.
type RowsAnswer {
    rows: List<CsvRow>
}

/// How many rows an import brought.
type CountAnswer {
    imported: Int
}

/// How many entries a restore brought.
type RestoredAnswer {
    restored: Int
}

/// How many entries the device's own vault carried in, or why it could not be read.
type CarriedAnswer {
    carried: Int
    error: Text
}

/// Where an engine kept what it made.
type UrlAnswer {
    url: Text
}
