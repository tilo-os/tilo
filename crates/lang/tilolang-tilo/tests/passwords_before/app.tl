app so.tilo.passwords "Passwords" (
    icon: .key,
    category: .utility,
    tagline: "Keeps your logins and passwords in a vault",
    keywords: ["password manager", "password", "passwords", "credentials", "logins", "vault", "keychain", "secrets", "passkeys", "1password", "bitwarden", "keepass", "keepassxc", "lastpass", "passwort", "passwörter", "passwortmanager"],
    splash_budget: 250,
)

op "csv-import"
op "csv-export"
op "backup-export"
op "backup-import"
op "device-items"
op "device-remove"

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

/// One saved secret: what it signs in to, who it signs in as and what it takes.
///
/// Every member the workspace lets read Passwords reads the same vault, and a change to an item
/// is everyone's to see, take back and find. The password, the notes and the fields a person
/// added are concealed: search never holds or finds them.
type Item (untitled: "Untitled item") {
    /// What it is called: a site, a network, a service.
    title: Text
    kind: Kind = .login
    /// What identifies the person to the account: a username, an account, a network's name.
    username: Text
    /// The secret itself.
    password: Text
    /// Anything worth keeping beside it, in the person's own words.
    notes: Text
    /// The addresses it signs in to, each one a web address.
    origins: Set<Text>
    tags: Set<Text>
    /// Fields the person added, by their label: a PIN, an account number.
    fields: Map<Text>
    favorite: Bool = false
    /// Whether the password was last changed more than a year ago, as the daily check writes it.
    old: Bool = false

    search {
        filter favorite "Favorites" (icon: .star)
        filter kind
        filter tags
        sort title a_z
        conceal password
        conceal notes
        conceal fields
    }
}

/// A row a browser's CSV held, waiting for the person to say whether it comes into the vault.
type ImportRow {
    title: Text
    username: Text
    origin: Text
    password: Text
    notes: Text
    /// Whether the vault already holds that login, so importing skips it.
    skip: Bool
}

/// The workspace's vault: every secret its members share, and what the window learned from the
/// device of what it tried to do.
type Vault {
    items: Collection<Item>
    /// The ids of device-vault items already taken into this document, so a second opening takes
    /// none of them twice.
    converted: Set<Text>
    /// The rows the person is deciding about, before an import.
    pending: Collection<ImportRow>
    /// What the last import, export or backup did, as a line the transfer page reads.
    notice: Text
}

root vault: Vault

default view vault_window for Vault

/// Starts an item: it opens in the window, named by what is typed first.
primary action new_item(v: Vault, title: Text = "") -> Item (
    icon: .plus,
    label: "New item",
    key: "Ctrl+N",
    did: "added an item",
) {
    v.items.insert(Item(title: title.trim()))
}

/// Adds a login in one step: what an agent, a script or the VM's own end-to-end check asks of
/// the vault. The name is required, the password is required, and an address is checked as a web
/// address.
action save_login(v: Vault, name: Text, origin: Text, username: Text, password: Text) -> Item (
    icon: .key,
    label: "Save a login",
    did: "saved a login",
) {
    require not name.trim().is_empty() "A login needs a name"
    require not password.is_empty() "A login needs its password"
    require origin.trim().is_empty() or origin_of(origin) != "" "{origin.trim()} is not a web address, such as https://example.com"
    let made = v.items.insert(
        Item(title: name.trim(), kind: .login, username: username.trim(), password: password),
    )
    if not origin.trim().is_empty() { made.origins.add(origin_of(origin)) }
    made
}

/// The vault's logins, by name and by the item that holds each: what a script, an agent or
/// another device's window lists. The passwords are never part of it.
action list_logins(v: Vault) -> LoginList (icon: .list, label: "List logins") {
    LoginList(
        logins: v.items
            .filter(i -> i.kind == .login)
            .sort_by(i -> i.title.lower())
            .map(i -> LoginEntry(item: i.id, name: i.title)),
    )
}

/// What a list of logins says: each by the item that holds it, and its name.
type LoginList {
    logins: List<LoginEntry>
}

/// One login in such a list.
type LoginEntry {
    item: Text
    name: Text
}

/// Writes what the editor holds onto an item: the password and notes it was given, and the
/// custom fields it typed. An empty password keeps the one the item had.
action save_editor(item: Item, password: Text, notes: Text, fields: Text) (
    icon: .check,
    label: "Save item",
    did: "saved an item",
) {
    require fields_problem(fields) == "" "Write a custom field as label = value"
    require not item.title.trim().is_empty() "Add a name so you can find this item"
    require item.kind != .note or not notes.trim().is_empty() "Add something to the note"
    require item.kind == .note or not password.is_empty() or not item.password.is_empty() "Add the {secret_label(item.kind).lower()}"
    if item.kind == .note {
        item.password = ""
    } else if not password.is_empty() {
        item.password = password
    }
    item.title = item.title.trim()
    item.username = item.username.trim()
    item.notes = notes
    write_fields(item, fields_of(fields))
}

/// Draws a strong password for the item: 20 characters, at least one capital, one small letter,
/// one digit and one symbol.
action generate_password(item: Item) uses random (
    icon: .refresh,
    label: "Generate strong password",
    did: "generated a password",
) {
    item.password = generated()
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
}

/// Takes a tag off an item.
action remove_tag(item: Item, tag: Text) (icon: .x, label: "Remove the tag") {
    item.tags.remove(tag)
}

/// Gives an item an address it signs in to, checked as a web address, as typed beside them.
action add_origin(item: Item, origin: Text = "") (icon: .globe, label: "Add a web address") {
    require not origin.trim().is_empty() "Add the address"
    require origin_of(origin) != "" "{origin.trim()} is not a web address, such as https://example.com"
    item.origins.add(origin_of(origin))
}

/// Takes an address off an item.
action remove_origin(item: Item, origin: Text) (icon: .x, label: "Remove the address") {
    item.origins.remove(origin)
}

/// Copies the item's secret to the person's clipboard.
action copy_secret(item: Item) uses clipboard (
    icon: .copy,
    label: "Copy {secret_label(item.kind).lower()}",
) {
    clipboard.copy(secret_value(item))
}

/// Copies what identifies the person to the account: a login's username, an API key's account,
/// a Wi-Fi network's name.
action copy_identity(item: Item) uses clipboard (
    icon: .user,
    label: "Copy {identity_label(item.kind).lower()}",
) {
    let who = identity(item)
    if not who.is_empty() { clipboard.copy(who) }
}

/// Copies the addresses the item signs in to, one a line.
action copy_origins(item: Item) uses clipboard (icon: .copy, label: "Copy web addresses") {
    if not item.origins.is_empty() {
        clipboard.copy(item.origins.map(o -> o).join("\n"))
    }
}

/// Deletes an item from the vault.
destructive action delete(v: Vault, item: Item) (
    icon: .trash,
    key: "Delete",
    did: "deleted an item",
) {
    v.items.remove(item)
}

/// Whether an item holds a password a person chose: a login or a Wi-Fi key, not an issued API
/// key or licence code.
fn is_password(item: Item) -> Bool {
    (item.kind == .login or item.kind == .wifi) and not item.password.is_empty()
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

/// Whether another item holds the same password, so the password is reused.
fn is_reused(v: Vault, item: Item) -> Bool {
    v.items.any(other -> other != item and is_password(other) and other.password == item.password)
}

/// Whether the item's password was last changed more than a year ago.
fn is_old(item: Item) -> Bool {
    item.old
}

/// Notes an item whose password was last changed more than a year ago, as the daily check runs.
action notice_age(item: Item) uses time (icon: .clock, label: "Check the password's age") {
    let stale = if let last = updated(item)?.at { (now - last).seconds() > 31536000 } else { false }
    if item.old != stale {
        item.old = stale
    }
}

/// Every item's age is checked once a day, so the review reads what the check wrote.
trigger age(item: Item) every 24h: notice_age(item)

/// What is wrong with the item's password, in one phrase: "Reused and weak", "Not changed in a
/// year". Empty for a password nothing is wrong with.
fn reasons(v: Vault, item: Item) -> Text {
    let said: List<Text> = []
    if is_reused(v, item) {
        said = [said, ["reused"]].flat_map(x -> x)
    }
    if is_weak(item.password) {
        said = [said, ["weak"]].flat_map(x -> x)
    }
    if is_old(item) {
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

/// How many items hold a password: what the review checks.
fn checked_count(v: Vault) -> Int {
    v.items.filter(i -> is_password(i)).count()
}

/// How many checked items have the flaw `of`: 1 reused, 2 weak, 3 old.
fn trouble_count(v: Vault, of: Int) -> Int {
    v.items
        .filter(i -> is_password(i))
        .filter(
            i -> if of == 1 {
                is_reused(v, i)
            } else if of == 2 { is_weak(i.password) } else { is_old(i) },
        )
        .count()
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
fn identity(item: Item) -> Text {
    if item.kind == .wifi {
        item.fields.get("network") ?? item.username
    } else { item.username }
}

/// The secret an item hands out: its password, or a secure note's text.
fn secret_value(item: Item) -> Text {
    if item.kind == .note { item.notes } else { item.password }
}

/// The one secondary fact a row shows: who it signs in as, else the site, else its kind.
fn summary(item: Item) -> Text {
    let who = identity(item)
    if not who.is_empty() {
        return who
    }
    if let site = item.origins.first() {
        return host_of(site)
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

/// The addresses the editor's text names, one a line or after a comma, each as the vault keeps
/// it.
fn origins_of(text: Text) -> List<Text> {
    let out: List<Text> = []
    for part in text.split("\n") {
        for piece in part.split(",") {
            let one = origin_of(piece)
            if one != "" and not out.any(o -> o == one) {
                out = [out, [one]].flat_map(x -> x)
            }
        }
    }
    out
}

/// Whether every address in the editor's text reads as a web address.
fn origins_read(text: Text) -> Bool {
    let ok = true
    for part in text.split("\n") {
        for piece in part.split(",") {
            if piece.trim() != "" and origin_of(piece) == "" {
                ok = false
            }
        }
    }
    ok
}

/// The tags the editor's text names, one after a comma or a line.
fn tags_of(text: Text) -> List<Text> {
    let out: List<Text> = []
    for part in text.split("\n") {
        for piece in part.split(",") {
            let tag = piece.trim()
            if not tag.is_empty() and not out.any(t -> t == tag) {
                out = [out, [tag]].flat_map(x -> x)
            }
        }
    }
    out
}

/// The custom fields the editor's text names, one a line as `label = value`.
fn fields_of(text: Text) -> Map<Text> {
    let out: Map<Text> = Map<Text>()
    for line in text.split("\n") {
        let trimmed = line.trim()
        if trimmed != "" {
            let at = trimmed.find("=")
            if at != .none {
                let label = trimmed.slice(0, at ?? 0).trim()
                if not label.is_empty() and label.lower() != "network" and out.get(label) == .none {
                    out.put(label, trimmed.slice((at ?? 0) + 1, trimmed.len()).trim())
                }
            }
        }
    }
    out
}

/// Whether the editor's custom fields read: a line without `label = value`, or a label the vault
/// keeps itself, is refused with a reason so the person fixes it before saving.
fn fields_problem(text: Text) -> Text {
    for line in text.split("\n") {
        let trimmed = line.trim()
        if trimmed != "" {
            let at = trimmed.find("=")
            if at == .none {
                return "Write a custom field as label = value"
            }
            let label = trimmed.slice(0, at ?? 0).trim()
            if label.is_empty() or label.lower() == "network" {
                return "Choose another label for the custom field"
            }
        }
    }
    ""
}

/// The custom fields as the editor's text writes them, one a line as `label = value`.
fn fields_text(item: Item) -> Text {
    item.fields.keys().sort_by(k -> k).map(k -> "{k} = {item.fields.get(k) ?? ""}").join("\n")
}

/// Replaces an item's addresses with `values`, one change.
action write_origins(item: Item, values: List<Text>) -> Bool (
    icon: .globe,
    label: "Write the addresses",
) {
    let held = item.origins.map(o -> o)
    for o in held {
        item.origins.remove(o)
    }
    for o in values {
        item.origins.add(o)
    }
    true
}

/// Replaces an item's tags with `values`, one change.
action write_tags(item: Item, values: List<Text>) -> Bool (icon: .tag, label: "Write the tags") {
    let held = item.tags.map(t -> t)
    for t in held {
        item.tags.remove(t)
    }
    for t in values {
        item.tags.add(t)
    }
    true
}

/// Replaces an item's custom fields with `values`, one change.
action write_fields(item: Item, values: Map<Text>) -> Bool (
    icon: .list,
    label: "Write the custom fields",
) {
    let held = item.fields.keys()
    for key in held {
        item.fields.remove(key)
    }
    for key in values.keys() {
        item.fields.put(key, values.get(key) ?? "")
    }
    true
}

/// A strong password: 20 characters drawn from capitals, small letters, digits and symbols, with
/// at least one of each.
fn generated() -> Text {
    let pool = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!@#%^&*-_=+?"
    let needed = [
        draw("ABCDEFGHJKLMNPQRSTUVWXYZ"),
        draw("abcdefghijkmnopqrstuvwxyz"),
        draw("23456789"),
        draw("!@#%^&*-_=+?"),
    ]
    let rest = "0123456789abcdef".split("").map(c -> draw(pool))
    random.shuffle([needed, rest].flat_map(part -> part)).join("")
}

/// One character drawn from `pool`.
fn draw(pool: Text) -> Text {
    let at = random.int(0, pool.len() - 1)
    pool.slice(at, at + 1)
}

/// The vault's items in the order the window lists them: by name, ignoring case.
fn listed(v: Vault) -> List<Item> {
    v.items.sort_by(i -> i.title.lower())
}

/// The vault window: its places in the sidebar, the items of the one chosen in the overview, and
/// the open item, the review, the apps or the transfers in the content.
// ---------------------------------------------------------------- the window
view vault_window(v: Vault) {
    // The place chosen: a kind of item, the favorites, or one of the tools.
    state place: Text = "all"
    state opened: Option<Item> = listed(v).first()
    state searched: Text = ""
    // What the window asks over the open item: its secret revealed, its password or notes being
    // written, or it about to be deleted.
    state showing: Option<Item> = .none
    state ask_reveal: Option<Item> = .none
    state ask_delete: Option<Item> = .none
    state edit_password: Option<Item> = .none
    state edit_notes: Option<Item> = .none
    state in_editor: Bool = false
    // What is being typed into the open item: its password, its notes, and its custom fields
    // with the drafts the editor holds.
    state password_text: Text = ""
    state notes_text: Text = ""
    state draft_password: Text = ""
    state draft_notes: Text = ""
    state draft_fields: Text = ""
    // The backup and restore forms.
    state backup_password: Text = ""
    state backup_again: Text = ""
    state restore_password: Text = ""
    state ask_backup: Bool = false
    state ask_restore: Bool = false
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
                        on select: set(place, "review")
                            .then(set(opened, .none))
                            .then(set(in_editor, false))
                    }
                    ui.NavItem("transfer", "Import and export", icon: .arrow_up_down) {
                        on select: set(place, "transfer")
                            .then(set(opened, .none))
                            .then(set(in_editor, false))
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
                    on choose: set(backup_password, "")
                        .then(set(backup_again, ""))
                        .then(set(ask_backup, true))
                }
                ui.MenuItem("Restore from backup…", icon: .lock_open) {
                    on choose: set(restore_password, "").then(set(ask_restore, true))
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
                        on select(x: Item): set(opened, x)
                            .then(set(in_editor, false))
                            .then(set(showing, .none))
                        for i in found {
                            ui.ListItem(item: i, opens: false) {
                                ui.ListRow(
                                    title: i.title,
                                    secondary: summary(i),
                                    item: i,
                                    selected: if let chosen = opened { chosen == i } else { false },
                                    icon: if i.kind == .login {
                                        .globe
                                    } else if i.kind == .api_key {
                                        .code
                                    } else if i.kind == .note {
                                        .file_text
                                    } else if i.kind == .wifi { .wifi } else { .shield },
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
                transfer_page(
                    v,
                    backup_password,
                    backup_again,
                    ask_backup,
                    restore_password,
                    ask_restore,
                )
            } else if let item = opened {
                if in_editor {
                    item_editor(item, draft_password, draft_notes, draft_fields, in_editor)
                } else {
                    item_page(
                        v,
                        item,
                        showing,
                        ask_reveal,
                        ask_delete,
                        edit_password,
                        password_text,
                        edit_notes,
                        notes_text,
                        in_editor,
                        draft_password,
                        draft_notes,
                        draft_fields,
                    )
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
    dialogs(
        v,
        ask_reveal,
        showing,
        ask_delete,
        edit_password,
        password_text,
        edit_notes,
        notes_text,
        ask_backup,
        ask_restore,
        backup_password,
        backup_again,
        restore_password,
    )
}

/// An item's own page: its name, what it signs in as, its secret, the addresses it signs in to,
/// its tags, notes, custom fields and when it last changed, with the controls that write them.
// ---------------------------------------------------------------- the open item
view item_page(
    v: Vault,
    item: Item,
    showing: State<Option<Item>>,
    ask_reveal: State<Option<Item>>,
    ask_delete: State<Option<Item>>,
    edit_password: State<Option<Item>>,
    password_text: State<Text>,
    edit_notes: State<Option<Item>>,
    notes_text: State<Text>,
    in_editor: State<Bool>,
    draft_password: State<Text>,
    draft_notes: State<Text>,
    draft_fields: State<Text>,
) {
    stack(gap: .group) {
        row(gap: .related, align: .center) {
            ui.TextInput(bind: item.title, title: true, placeholder: "What it is called", fill)
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
            text(kind_label(item.kind), role: .caption, secondary: true)
            spacer
            ui.Button("Edit details", icon: .pencil, role: .primary) {
                on press: set(draft_password, "")
                    .then(set(draft_notes, item.notes))
                    .then(set(draft_fields, fields_text(item)))
                    .then(set(in_editor, true))
            }
        }
        std.body() {
            ui.PropertyList {
                if not identity(item).is_empty() and item.kind != .note {
                    ui.Property(identity_label(item.kind), identity(item), copyable: true)
                }
                ui.Property(
                    secret_label(item.kind),
                    if let shown = showing {
                        if shown == item {
                            secret_value(item)
                        } else { "••••••••••••" }
                    } else { "••••••••••••" },
                )
                if not item.origins.is_empty() {
                    ui.Property(
                        origin_label(item.kind, item.origins.len()),
                        item.origins.map(o -> o).join("\n"),
                    )
                }
                if not item.tags.is_empty() {
                    ui.Property("Tags", item.tags.map(t -> t).join(", "))
                }
                if item.kind != .note and not item.notes.trim().is_empty() {
                    ui.Property("Notes", "Hidden until you edit the item")
                }
                if not item.fields.keys().is_empty() {
                    ui.Property("Custom fields", "{item.fields.keys().len()} kept")
                }
            }
            if let last = updated(item)?.at {
                row(gap: .related, align: .center) {
                    text("Last changed", role: .label)
                    ui.Time(at: last, past: true)
                }
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Copy {secret_label(item.kind).lower()}", icon: .copy) {
                    on press: copy_secret(item)
                }
                if not identity(item).is_empty() and item.kind != .note {
                    ui.Button("Copy {identity_label(item.kind).lower()}", icon: .copy) {
                        on press: copy_identity(item)
                    }
                }
                if not item.origins.is_empty() {
                    ui.Button("Copy web addresses", icon: .copy) {
                        on press: copy_origins(item)
                    }
                }
                ui.Button(
                    if let shown = showing {
                        if shown == item { "Hide" } else { "Reveal secret" }
                    } else { "Reveal secret" },
                    icon: .eye,
                ) {
                    on press: if let shown = showing {
                        if shown == item { set(showing, .none) } else { set(ask_reveal, item) }
                    } else { set(ask_reveal, item) }
                }
                if item.kind != .note {
                    ui.Button("Change the password", icon: .refresh) {
                        on press: set(password_text, "").then(set(edit_password, item))
                    }
                }
                if item.kind == .note {
                    ui.Button("Open the note", icon: .file_text) {
                        on press: set(notes_text, item.notes).then(set(edit_notes, item))
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
                    for origin in item.origins {
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
            row(gap: .related, wrap: true) {
                ui.Button("Delete item", icon: .trash, role: .destructive) {
                    on press: set(ask_delete, item)
                }
            }
        }
    }
}

/// The editor: an item's kind, name, identity, addresses, secret, notes, tags and custom fields,
/// with the fields the kind has.
view item_editor(
    item: Item,
    draft_password: State<Text>,
    draft_notes: State<Text>,
    draft_fields: State<Text>,
    in_editor: State<Bool>,
) {
    stack(gap: .group) {
        row(gap: .related, align: .center) {
            text(if item.title == "" { "New item" } else { "Edit item" }, role: .title)
            spacer
            ui.Button("Save", icon: .check, role: .primary) {
                on press: save_editor(item, draft_password, draft_notes, draft_fields)
                    .then(set(in_editor, false))
            }
            ui.Button("Cancel", icon: .x) {
                on press: set(in_editor, false)
            }
        }
        form {
            field("Kind") {
                ui.EnumField(bind: item.kind, label: "Kind")
            }
            field("Name") {
                ui.TextInput(
                    bind: item.title,
                    placeholder: "What it is called",
                    autofocus: true,
                    fill,
                )
            }
            if item.kind != .note {
                field(identity_label(item.kind)) {
                    ui.TextInput(bind: item.username, placeholder: "Who it signs in as", fill)
                }
            }
            if item.kind != .note {
                field(secret_label(item.kind), help: "Leave it empty to keep the current one") {
                    ui.TextInput(
                        bind: draft_password,
                        secret: true,
                        placeholder: "The {secret_label(item.kind).lower()}",
                    )
                    row(gap: .related) {
                        ui.Button("Generate strong password", icon: .refresh) {
                            on press: generate_password(item)
                        }
                    }
                }
            }
            field("Notes") {
                ui.TextInput(bind: draft_notes, lines: 4, placeholder: "Anything worth keeping")
            }
            if item.kind == .wifi {
                field("Network") {
                    text(identity(item), role: .caption, secondary: true)
                }
            }
            field("Tags") {
                if not item.tags.is_empty() {
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
                field(origin_label(item.kind, 2)) {
                    for origin in item.origins {
                        row(gap: .tight, align: .center) {
                            text(origin, role: .caption)
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
            field("Custom fields", help: "One a line, as PIN = 1234") {
                ui.TextInput(bind: draft_fields, lines: 3, placeholder: "PIN = 1234")
                if fields_problem(draft_fields) != "" {
                    text(fields_problem(draft_fields), role: .caption, secondary: true)
                }
            }
        }
    }
}

/// The review: the passwords that are reused, weak or old, each opening its item.
// ---------------------------------------------------------------- the review
view review_page(v: Vault) {
    let total = checked_count(v)
    let reused = trouble_count(v, 1)
    let weak = trouble_count(v, 2)
    let old = trouble_count(v, 3)
    let troubled = v.items
        .filter(i -> is_password(i))
        .filter(i -> is_reused(v, i) or is_weak(i.password) or is_old(i))
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
                        for i in troubled {
                            ui.ListRow(
                                title: i.title,
                                secondary: reasons(v, i),
                                item: i,
                                icon: if i.kind == .login {
                                    .globe
                                } else if i.kind == .api_key {
                                    .code
                                } else if i.kind == .note {
                                    .file_text
                                } else if i.kind == .wifi { .wifi } else { .shield },
                            )
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
view transfer_page(
    v: Vault,
    backup_password: State<Text>,
    backup_again: State<Text>,
    ask_backup: State<Bool>,
    restore_password: State<Text>,
    ask_restore: State<Bool>,
) {
    ui.Scroll {
        page {
            stack(gap: .group) {
                text("Import and export", role: .title)
                if v.notice != "" {
                    ui.Banner(v.notice, kind: .info)
                }
                ui.Section("Encrypted backup") {
                    text(
                        "A backup keeps every item, note and custom field, encrypted with a password you choose.",
                        role: .body,
                    )
                    row(gap: .related, wrap: true) {
                        ui.Button("Back up vault…", icon: .lock) {
                            on press: set(backup_password, "")
                                .then(set(backup_again, ""))
                                .then(set(ask_backup, true))
                        }
                        ui.Button("Restore from backup…", icon: .lock_open) {
                            on press: set(restore_password, "").then(set(ask_restore, true))
                        }
                    }
                }
                ui.Section("Browser passwords") {
                    text(
                        "Move logins between Passwords and a web browser with a CSV file.",
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
/// vault already holds that login.
action preview_csv(v: Vault, file: File) (icon: .file_plus, label: "Import from CSV") {
    clear_pending(v)
    v.notice = ""
    match engine.call("csv-import", json.encode(AskedFile(url: file.url))) {
        .ok(text) => match json.decode<RowsAnswer>(text) {
            .ok(answer) => {
                for row in answer.rows {
                    v.pending.insert(
                        ImportRow(
                            title: row.title,
                            username: row.username,
                            origin: row.origin,
                            password: row.password,
                            notes: row.notes,
                            skip: holds_login(v, row.title, row.username, row.origin),
                        ),
                    )
                }
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
fn holds_login(v: Vault, title: Text, username: Text, origin: Text) -> Bool {
    v.items.any(
        i -> i.title.lower() == title.trim().lower()
            and i.username.lower() == username.trim().lower()
            and same_origin(i, origin),
    )
}

/// Whether an item signs in at `origin`: at none when the address is empty.
fn same_origin(item: Item, origin: Text) -> Bool {
    let wanted = origin_of(origin)
    if wanted == "" {
        return item.origins.is_empty()
    }
    item.origins.any(o -> o == wanted)
}

/// Brings the rows the person agreed to into the vault, skipping the ones the vault holds.
action apply_import(v: Vault) (icon: .check, label: "Import the logins") {
    let brought = 0
    let skipped = 0
    let rows = v.pending.map(r -> r)
    for row in rows {
        if row.skip or holds_login(v, row.title, row.username, row.origin) {
            skipped = skipped + 1
        } else {
            let made = v.items.insert(
                Item(
                    title: row.title.trim(),
                    kind: .login,
                    username: row.username,
                    password: row.password,
                    notes: row.notes,
                ),
            )
            if not row.origin.trim().is_empty() {
                write_origins(made, origins_of(row.origin))
            }
            brought = brought + 1
        }
    }
    clear_pending(v)
    v.notice = if skipped == 0 {
        if brought == 1 { "Imported 1 login" } else { "Imported {brought} logins" }
    } else {
        "Imported {brought} logins and skipped {skipped}"
    }
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
    true
}

/// The vault as a browser's CSV, named for what it holds: the person chooses where it goes.
action export_csv(v: Vault) -> File (icon: .download, label: "Export to CSV") {
    let asked = json.encode(ItemsAsk(items: asked_items(v)))
    match engine.call("csv-export", asked) {
        .ok(text) => match json.decode<UrlAnswer>(text) {
            .ok(answer) => File.at("passwords.csv", answer.url),
            .err(why) => File.of_text("The export could not be made.txt", why),
        },
        .err(why) => File.of_text("The export could not be made.txt", why),
    }
}

/// The vault as an encrypted backup under `password`: the person chooses where it goes.
action export_backup(v: Vault, password: Text) -> File (icon: .lock, label: "Back up the vault") {
    require password.len() >= 12 "Use at least 12 characters for the backup password"
    let asked = json.encode(BackupAsk(items: asked_items(v), passphrase: password))
    match engine.call("backup-export", asked) {
        .ok(text) => match json.decode<UrlAnswer>(text) {
            .ok(answer) => File.at("passwords-backup.tilopw", answer.url),
            .err(why) => File.of_text("The backup could not be made.txt", why),
        },
        .err(why) => File.of_text("The backup could not be made.txt", why),
    }
}

/// Brings the items a backup holds into the vault, and answers how many came in.
action restore_backup(v: Vault, password: Text, file: File) -> Int (
    icon: .lock_open,
    label: "Restore from a backup",
) {
    require password.len() >= 12 "Use at least 12 characters for the backup password"
    let asked = json.encode(RestoreAsk(url: file.url, passphrase: password))
    match engine.call("backup-import", asked) {
        .ok(text) => match json.decode<DeviceAnswer>(text) {
            .ok(answer) => {
                let restored = 0
                for one in answer.items {
                    let made = v.items.insert(
                        Item(
                            title: one.title,
                            kind: kind_of(one.kind),
                            username: one.username,
                            password: one.password,
                            notes: one.notes,
                        ),
                    )
                    write_origins(made, one.origins)
                    write_tags(made, one.tags)
                    write_fields(made, one.fields)
                    restored = restored + 1
                }
                v.notice = if restored == 1 {
                    "Restored 1 item"
                } else { "Restored {restored} items" }
                restored
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

/// The vault's items as JSON the device's engines read.
fn asked_items(v: Vault) -> List<AskedItem> {
    v.items
        .sort_by(i -> i.title.lower())
        .map(
            i -> AskedItem(
                title: i.title,
                kind: kind_word(i.kind),
                username: i.username,
                password: i.password,
                notes: i.notes,
                origins: i.origins.map(o -> o),
                tags: i.tags.map(t -> t),
                fields: i.fields,
            ),
        )
}

/// The text a kind is written as in JSON.
fn kind_word(kind: Kind) -> Text {
    if kind == .api_key {
        "api-key"
    } else if kind == .note {
        "note"
    } else if kind == .wifi { "wifi" } else if kind == .secret { "other" } else { "login" }
}

/// The kind a JSON's text names.
fn kind_of(word: Text) -> Kind {
    if word == "api-key" or word == "api_key" {
        .api_key
    } else if word == "note" {
        .note
    } else if word == "wifi" {
        .wifi
    } else if word == "other" or word == "secret" { .secret } else { .login }
}

/// Takes into the vault what this device's secret service holds and a workspace may share. Each
/// entry is sealed into the vault; the device's authentication material and its Wi-Fi networks
/// stay where they are. An entry already taken in is not taken again.
// ---------------------------------------------------------------- this device's vault
action absorb_device(v: Vault) (icon: .key, label: "Import this device's vault") {
    match engine.call("device-items", "") {
        .ok(text) => match json.decode<DeviceAnswer>(text) {
            .ok(answer) => {
                if answer.error != "" {
                    v.notice = answer.error
                    return
                }
                let taken: List<Text> = []
                let brought = 0
                for one in answer.items {
                    if not v.converted.contains(one.id) and not holds_device_login(v, one) {
                        let made = v.items.insert(
                            Item(
                                title: one.title,
                                kind: kind_of(one.kind),
                                username: one.username,
                                password: one.password,
                                notes: one.notes,
                            ),
                        )
                        write_origins(made, one.origins)
                        write_tags(made, one.tags)
                        write_fields(made, one.fields)
                        brought = brought + 1
                    }
                    v.converted.add(one.id)
                    taken = [taken, [one.id]].flat_map(x -> x)
                }
                if not taken.is_empty() {
                    engine.call("device-remove", json.encode(RemoveAsk(ids: taken)))
                }
                if brought == 1 {
                    v.notice = "Imported 1 login from this device"
                } else if brought > 1 {
                    v.notice = "Imported {brought} logins from this device"
                } else {
                    v.notice = ""
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

/// Whether the vault already holds what the device's entry says: the same name and username,
/// and an address it signs in to.
fn holds_device_login(v: Vault, one: DeviceItem) -> Bool {
    v.items.any(
        i -> i.title.lower() == one.title.trim().lower()
            and i.username.lower() == one.username.trim().lower()
            and one.origins.any(o -> i.origins.any(k -> k == o)),
    )
}

/// The questions the window asks over its page: revealing a secret, deleting an item, writing its
/// password or notes, and the backup and restore forms.
// ---------------------------------------------------------------- the window's dialogs
view dialogs(
    v: Vault,
    ask_reveal: State<Option<Item>>,
    showing: State<Option<Item>>,
    ask_delete: State<Option<Item>>,
    edit_password: State<Option<Item>>,
    password_text: State<Text>,
    edit_notes: State<Option<Item>>,
    notes_text: State<Text>,
    ask_backup: State<Bool>,
    ask_restore: State<Bool>,
    backup_password: State<Text>,
    backup_again: State<Text>,
    restore_password: State<Text>,
) {
    if let item = ask_reveal {
        ui.ModalDialog(
            title: "Reveal the {secret_label(item.kind).lower()} of “{item.title}”?",
            open: true,
            primary: "Reveal",
            secondary: "Cancel",
        ) {
            on act_primary: set(showing, item).then(set(ask_reveal, .none))
            on act_secondary: set(ask_reveal, .none)
            on dismiss: set(ask_reveal, .none)
            body: {
                text("It shows for 10 seconds, then hides again.", role: .body)
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
    if let item = edit_password {
        ui.ModalDialog(
            title: "Change the {secret_label(item.kind).lower()} of “{item.title}”",
            open: true,
            primary: "Keep",
            secondary: "Cancel",
            primary_enabled: password_text != "" or item.password != "",
        ) {
            on act_primary: save_editor(item, password_text, item.notes, fields_text(item))
                .then(set(password_text, ""))
                .then(set(edit_password, .none))
            on act_secondary: set(password_text, "").then(set(edit_password, .none))
            on dismiss: set(password_text, "").then(set(edit_password, .none))
            body: {
                ui.TextInput(
                    bind: password_text,
                    secret: true,
                    autofocus: true,
                    placeholder: "The password",
                )
                row(gap: .related) {
                    ui.Button("Generate strong password", icon: .refresh) {
                        on press: generate_password(item).then(set(password_text, ""))
                    }
                }
            }
        }
    }
    if let item = edit_notes {
        ui.ModalDialog(
            title: "Write the note of “{item.title}”",
            open: true,
            primary: "Keep",
            secondary: "Cancel",
        ) {
            on act_primary: save_editor(item, "", notes_text, fields_text(item))
                .then(set(edit_notes, .none))
            on act_secondary: set(edit_notes, .none)
            on dismiss: set(edit_notes, .none)
            body: {
                ui.TextInput(bind: notes_text, lines: 6, autofocus: true, placeholder: "The note")
            }
        }
    }
    if ask_backup {
        ui.ModalDialog(
            title: "Back up vault",
            open: true,
            primary: "Choose file…",
            secondary: "Cancel",
            primary_enabled: backup_password.len() >= 12 and backup_password == backup_again,
        ) {
            on act_primary: file
                .save(
                    export_backup(v, backup_password),
                    name: "passwords-backup.tilopw",
                )
                .then(set(backup_password, ""))
                .then(set(backup_again, ""))
                .then(set(ask_backup, false))
            on act_secondary: set(backup_password, "")
                .then(set(backup_again, ""))
                .then(set(ask_backup, false))
            on dismiss: set(backup_password, "")
                .then(set(backup_again, ""))
                .then(set(ask_backup, false))
            body: {
                text(
                    "A backup keeps every item, note and custom field, encrypted with a password you choose.",
                    role: .body,
                )
                ui.TextInput(
                    bind: backup_password,
                    secret: true,
                    autofocus: true,
                    placeholder: "Backup password",
                )
                text("Use at least 12 characters", role: .caption, secondary: true)
                ui.TextInput(
                    bind: backup_again,
                    secret: true,
                    placeholder: "Confirm backup password",
                )
                if backup_again != "" and backup_password != backup_again {
                    text("The passwords do not match", role: .caption, secondary: true)
                }
            }
        }
    }
    if ask_restore {
        ui.ModalDialog(
            title: "Restore from backup",
            open: true,
            primary: "Choose file…",
            secondary: "Cancel",
            primary_enabled: restore_password.len() >= 12,
        ) {
            on act_primary: file
                .choose(restore_backup(v, restore_password), accept: [".tilopw"])
                .then(set(restore_password, ""))
                .then(set(ask_restore, false))
            on act_secondary: set(restore_password, "").then(set(ask_restore, false))
            on dismiss: set(restore_password, "").then(set(ask_restore, false))
            body: {
                ui.TextInput(
                    bind: restore_password,
                    secret: true,
                    autofocus: true,
                    placeholder: "Backup password",
                )
            }
        }
    }
}

/// An item as the device's engines read and write it.
// ---------------------------------------------------------------- the device's answers
type AskedItem {
    title: Text
    kind: Text
    username: Text
    password: Text
    notes: Text
    origins: List<Text>
    tags: List<Text>
    fields: Map<Text>
}

/// What the export engines are asked: the vault's items.
type ItemsAsk {
    items: List<AskedItem>
}

/// What the backup engine is asked: the vault's items under a password.
type BackupAsk {
    items: List<AskedItem>
    passphrase: Text
}

/// What the restore engine is asked: where the backup is, and the password it takes.
type RestoreAsk {
    url: Text
    passphrase: Text
}

/// What the CSV engine is asked: the file the person chose.
type AskedFile {
    url: Text
}

/// What the device's vault is asked to forget: the ids of the entries the vault took in.
type RemoveAsk {
    ids: List<Text>
}

/// One item as a device or a backup hands it over.
type DeviceItem {
    id: Text
    title: Text
    kind: Text
    username: Text
    password: Text
    notes: Text
    origins: List<Text>
    tags: List<Text>
    fields: Map<Text>
}

/// What the device's vault answers: its shareable entries, or why it could not be read.
type DeviceAnswer {
    items: List<DeviceItem>
    error: Text
}

/// One row of a browser's CSV.
type CsvRow {
    title: Text
    username: Text
    origin: Text
    password: Text
    notes: Text
}

/// The rows a CSV holds.
type RowsAnswer {
    rows: List<CsvRow>
}

/// Where an engine kept what it made.
type UrlAnswer {
    url: Text
}

/// A vault of eight items: a bank, a mailbox, a code host, a cloud console key, a streaming
/// login, a home network, a licence and a note, with the passwords the review finds.
// ---------------------------------------------------------------- scenarios
scenario "a vault" {
    let v = root Vault
    let bank = v.items.insert(
        Item(
            title: "Northstar Bank",
            kind: .login,
            username: "ada@example.com",
            password: "Harbor-Lights-2019",
            favorite: true,
        ),
    )
    bank.origins.add("https://bank.example")
    bank.origins.add("https://login.bank.example")
    let mail = v.items.insert(
        Item(
            title: "Personal mail",
            kind: .login,
            username: "ada@mail.example",
            password: "Harbor-Lights-2019",
        ),
    )
    mail.origins.add("https://mail.example")
    let code = v.items.insert(
        Item(
            title: "Code hosting",
            kind: .login,
            username: "adalovelace",
            password: "q7Lm-Vt2x-Rk9p-Hw4c",
            favorite: true,
        ),
    )
    code.origins.add("https://code.example")
    let console = v.items.insert(
        Item(
            title: "Cloud console",
            kind: .api_key,
            username: "lovelace-prod-0Il1O",
            password: "ck_live_5bT9wQ2mXc8LrV4n",
        ),
    )
    console.origins.add("https://console.cloud.example")
    let stream = v.items.insert(
        Item(title: "Streaming", kind: .login, username: "ada.l@example.com", password: "tv1234"),
    )
    stream.origins.add("https://watch.example")
    let home = v.items.insert(
        Item(title: "Home Wi-Fi", kind: .wifi, username: "", password: "harbourlights"),
    )
    home.fields.put("network", "Lovelace home")
    v.items.insert(
        Item(
            title: "Design library licence",
            kind: .secret,
            username: "ada@studio.example",
            password: "DL-4K7Q-92XM-PL3T",
        ),
    )
    v.items.insert(
        Item(
            title: "Recovery codes",
            kind: .note,
            username: "",
            password: "",
            notes: "Code hosting: 4821-7730, 1956-0442, 7303-9185",
        ),
    )
}
