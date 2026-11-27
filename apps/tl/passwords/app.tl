app so.tilo.passwords "Passwords" (
    icon: .key,
    category: .utility,
    tagline: "Keeps your logins and passwords in a vault",
    keywords: ["password manager", "password", "passwords", "credentials", "logins", "vault", "keychain", "secrets", "passkeys", "1password", "bitwarden", "keepass", "keepassxc", "lastpass", "passwort", "passwörter", "passwortmanager"],
    splash_budget: 250,
)

use tilo.actions as actions
use tilo.invocations as invocations

op "vault-preview-csv"
op "vault-read-csv"
op "vault-export-csv"
op "vault-hold-typed"
op "vault-export-backup"
op "vault-read-backup"
op "vault-device-items"
op "vault-device-remove"

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

/// One saved secret, as the person finds it: what it is called, its kind and tags, and the item
/// of the workspace's vault that keeps everything else.
///
/// The vault keeps who it signs in as, its password, its notes and the values of the fields a
/// person added, sealed; this record never holds them. It holds what the vault says of the item
/// that is safe to say, as the vault last listed it, and the window shows a secret only when the
/// person asks, for a while, where no capture reaches. Every member the workspace lets read
/// Passwords reads the same vault.
type Item (untitled: "Untitled item") {
    /// The item of the workspace's vault this record stands for: its id there, which the
    /// reference `vault.item:<id>` names.
    entry: Text
    /// What it is called: a site, a network, a service.
    title: Text
    kind: Kind = .login
    tags: Set<Text>
    favorite: Bool = false
    /// Where it signs in, as the vault lists it.
    places: Set<Text>
    /// The labels of the fields it keeps, as the vault lists them: never their values.
    labels: Set<Text>
    /// Whether it keeps a note.
    noted: Bool = false
    /// When it last changed, in seconds since 1970.
    changed: Int = 0
    /// What the vault finds wrong with its password: `weak`, `reused`, `stale`.
    concerns: Set<Text>
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

/// The workspace's vault as Passwords lists it: an item for each of the vault's, and what the
/// window learned of what it tried to do.
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

// ---------------------------------------------------------------- the vault's calls
/// The address of the vault's own call `verb`.
fn vault_at(verb: Text) -> Text {
    "tilo.vault/vault#{verb}"
}

/// The reference the vault names an item by.
fn reference(item: Item) -> Text {
    "vault.item:{item.entry}"
}

/// The id a vault's reference names.
fn id_of(reference: Text) -> Text {
    if reference.starts_with("vault.item:") { reference.slice(11, reference.len()) } else { "" }
}

/// The kind as the vault names it.
fn kind_word(kind: Kind) -> Text {
    if kind == .api_key {
        "key"
    } else if kind == .note {
        "note"
    } else if kind == .wifi { "wifi" } else if kind == .secret { "value" } else { "login" }
}

/// The kind as a file a browser or a backup holds names it.
fn file_kind(kind: Kind) -> Text {
    if kind == .api_key {
        "api_key"
    } else if kind == .note {
        "note"
    } else if kind == .wifi { "wifi" } else if kind == .secret { "secret" } else { "login" }
}

/// The kind a word names: the vault's, or what a file an older Passwords or a browser wrote says.
fn kind_of(word: Text) -> Kind {
    if word == "key" or word == "api_key" or word == "api-key" {
        .api_key
    } else if word == "note" {
        .note
    } else if word == "wifi" { .wifi } else if word == "login" { .login } else { .secret }
}

/// `text` as JSON.
fn js(text: Text) -> Text {
    json.encode(text)
}

/// `texts` as a JSON list.
fn js_list(texts: List<Text>) -> Text {
    "[" + texts.map(t -> js(t)).join(",") + "]"
}

fn brace_open() -> Text {
    "\{"
}

fn brace_close() -> Text {
    "\}"
}

/// A JSON object of `pairs`, each already written as `"name":value`.
fn object(pairs: List<Text>) -> Text {
    brace_open() + pairs.filter(p -> p != "").join(",") + brace_close()
}

/// What the person typed in place `place` on the system's card for the call being run, named
/// where it is: the host fills it in on its way to the vault.
fn typed(place: Int) -> Text {
    object(["\"$typed\":{place}"])
}

/// What the host holds under `token`, named where it is.
fn held(token: Text) -> Text {
    object(["\"$held\":" + js(token)])
}

/// A field of an add: its label, and its value as written (a name for a secret, or `""` to keep
/// the one the item has).
fn field_of(label: Text, value: Text) -> Text {
    object(["\"label\":" + js(label), "\"value\":" + value])
}

/// The vault's add for `item` as it is now, with `kind`, `title`, `places` and `labels` in place
/// of its own and `more` (`"name":value` pairs) beside them: the item changed in place, every
/// secret it does not name kept as it was.
fn change_of(
    item: Item,
    kind: Kind,
    title: Text,
    places: List<Text>,
    labels: List<Text>,
    more: List<Text>,
) -> Text {
    object(
        [
            [
                "\"item\":" + js(reference(item)),
                "\"kind\":" + js(kind_word(kind)),
                "\"title\":" + js(title),
                "\"belongsTo\":" + js_list(places),
                "\"tags\":" + js_list(item.tags.map(t -> t)),
                "\"fields\":[" + labels.map(l -> field_of(l, "\"\"")).join(",") + "]",
            ],
            more,
        ]
            .flat_map(x -> x),
    )
}

/// `text` as the JSON value it writes, or nothing for what does not read.
fn parsed(text: Text) -> Option<json.Value> {
    match json.parse(text) {
        .ok(value) => .some(value),
        .err(why) => .none,
    }
}

fn at(value: json.Value, key: Text) -> Option<json.Value> {
    match value {
        .object(fields) => fields.get(key),
        .array(items) => .none,
        .string(text) => .none,
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// A JSON value as the text it holds, and nothing for anything else.
fn text_of(value: json.Value) -> Text {
    match value {
        .string(text) => text,
        .object(fields) => "",
        .array(items) => "",
        .number(number) => "",
        .bool(flag) => "",
        .null => "",
    }
}

fn at_text(value: json.Value, key: Text) -> Text {
    match at(value, key) {
        .none => "",
        .some(found) => text_of(found),
    }
}

fn at_int(value: json.Value, key: Text) -> Int {
    match at(value, key) {
        .none => 0,
        .some(found) => match found {
            .number(number) => number.to_int(),
            .string(text) => 0,
            .object(fields) => 0,
            .array(items) => 0,
            .bool(flag) => 0,
            .null => 0,
        },
    }
}

fn at_flag(value: json.Value, key: Text) -> Bool {
    match at(value, key) {
        .none => false,
        .some(found) => match found {
            .bool(flag) => flag,
            .number(number) => false,
            .string(text) => false,
            .object(fields) => false,
            .array(items) => false,
            .null => false,
        },
    }
}

fn at_list(value: json.Value, key: Text) -> List<json.Value> {
    match at(value, key) {
        .none => [],
        .some(found) => match found {
            .array(items) => items,
            .object(fields) => [],
            .string(text) => [],
            .number(number) => [],
            .bool(flag) => [],
            .null => [],
        },
    }
}

fn at_texts(value: json.Value, key: Text) -> List<Text> {
    at_list(value, key).map(one -> text_of(one)).filter(one -> one != "")
}

/// Writes what the vault says of an item (`listed`, one of what it lists) into its record, where
/// it differs: its name, kind and tags, where it signs in, its fields' labels, whether it keeps a
/// note, when it changed and what is wrong with it.
action learn(item: Item, listed: json.Value) (
    icon: .refresh,
    label: "Read the item from the vault",
) {
    let title = at_text(listed, "title")
    if title != "" and title != item.title {
        item.title = title
    }
    let kind = kind_of(at_text(listed, "kind"))
    if kind != item.kind {
        item.kind = kind
    }
    let tags = at_texts(listed, "tags")
    for t in item.tags.map(t -> t) {
        if not tags.contains(t) { item.tags.remove(t) }
    }
    for t in tags {
        if not item.tags.contains(t) { item.tags.add(t) }
    }
    let places = at_texts(listed, "belongsTo")
    for p in item.places.map(p -> p) {
        if not places.contains(p) { item.places.remove(p) }
    }
    for p in places {
        if not item.places.contains(p) { item.places.add(p) }
    }
    let labels = at_texts(listed, "fields")
    for l in item.labels.map(l -> l) {
        if not labels.contains(l) { item.labels.remove(l) }
    }
    for l in labels {
        if not item.labels.contains(l) { item.labels.add(l) }
    }
    let concerns = at_texts(listed, "concerns")
    for c in item.concerns.map(c -> c) {
        if not concerns.contains(c) { item.concerns.remove(c) }
    }
    for c in concerns {
        if not item.concerns.contains(c) { item.concerns.add(c) }
    }
    let noted = at_flag(listed, "hasNotes")
    if noted != item.noted {
        item.noted = noted
    }
    let changed = at_int(listed, "changedAt")
    if changed != item.changed {
        item.changed = changed
    }
}

/// Asks the vault to keep `ask` (its add, as JSON) and writes what it answers into `item`.
/// Answers why it could not, or nothing.
action keep(item: Item, ask: Text) -> Text (icon: .key, label: "Keep the item in the vault") {
    if let arguments = parsed(ask) {
        let answer = actions.call(address: vault_at("add"), arguments: arguments)
        if answer.status == .ended {
            if let out = answer.output { learn(item, out) }
            ""
        } else if let why = answer.failure { why } else { "The vault did not keep it" }
    } else {
        "The vault was asked for something it does not read"
    }
}

/// Adds a new item to the vault from `ask` (its add, as JSON) and lists it: the item, or nothing
/// where the vault did not keep it, with why in the vault's notice.
action add_new(v: Vault, ask: Text) -> Option<Item> (icon: .plus, label: "Add to the vault") {
    if let arguments = parsed(ask) {
        let answer = actions.call(address: vault_at("add"), arguments: arguments)
        if answer.status == .ended {
            if let out = answer.output {
                let id = id_of(at_text(out, "reference"))
                if id != "" {
                    let made = v.items.insert(Item(entry: id, title: at_text(out, "title")))
                    learn(made, out)
                    return .some(made)
                }
            }
        }
        v.notice = if let why = answer.failure { why } else { "The vault did not keep it" }
    }
    .none
}

/// Starts a login: an item of the vault named by what is typed first, with a strong password
/// drawn where the vault is, which nobody types or sees. The person changes it on the item.
primary action new_item(v: Vault, title: Text = "") -> Option<Item> (
    icon: .plus,
    label: "New item",
    key: "Ctrl+N",
    did: "added an item",
) {
    add_new(
        v,
        object(
            ["\"kind\":\"login\"", "\"title\":" + js(title.trim()), "\"generate\":" + object([])],
        ),
    )
}

/// Adds a login in one step: what an agent, a script or the VM's own end-to-end check asks of the
/// vault. The person types its password on the system's card, so whoever asked never holds it,
/// and the vault keeps it. An address is checked as a web address.
action save_login(v: Vault, name: Text, origin: Text, username: Text) -> Option<Item> (
    icon: .key,
    label: "Save a login",
    did: "saved a login",
    takes: [("Password", .secret)],
) {
    require not name.trim().is_empty() "A login needs a name"
    require origin.trim().is_empty() or origin_of(origin) != "" "{origin.trim()} is not a web address, such as https://example.com"
    let places: List<Text> = if origin.trim().is_empty() { [] } else { [origin_of(origin)] }
    add_new(
        v,
        object(
            [
                "\"kind\":\"login\"",
                "\"title\":" + js(name.trim()),
                "\"username\":" + js(username.trim()),
                "\"belongsTo\":" + js_list(places),
                "\"secret\":" + typed(0),
            ],
        ),
    )
}

/// The vault's logins, by name and by the item that holds each: what a script, an agent or
/// another device's window lists. The passwords are never part of it.
action list_logins(v: Vault) -> LoginList (icon: .list, label: "List logins") {
    LoginList(
        logins: v.items
            .filter(i -> i.kind == .login and i.entry != "")
            .sort_by(i -> i.title.lower())
            .map(i -> LoginEntry(item: i.entry, name: i.title)),
    )
}

/// What a list of logins says: each by the vault item that holds it, and its name.
type LoginList {
    logins: List<LoginEntry>
}

/// One login in such a list.
type LoginEntry {
    item: Text
    name: Text
}

/// Writes the item as it is, with `title`, `kind` and `places` in place of its own and `more`
/// beside them. Answers why it could not, or nothing.
action rewrite(item: Item, title: Text, kind: Kind, places: List<Text>, more: List<Text>) -> Text (
    icon: .key,
    label: "Write the item into the vault",
) {
    keep(item, change_of(item, kind, title, places, item.labels.map(l -> l), more))
}

/// Writes who the item signs in as.
action set_username(item: Item, username: Text) -> Text (
    icon: .user,
    label: "Change the username",
) {
    require not username.trim().is_empty() "Add the username"
    rewrite(
        item,
        item.title,
        item.kind,
        item.places.map(p -> p),
        ["\"username\":" + js(username.trim())],
    )
}

/// Changes the item's password, key or secret: the person types it on the system's card, and the
/// vault keeps it. Nothing of it reaches this app's code, its records or history.
action change_secret(item: Item) -> Text (
    icon: .key,
    label: "Change the {secret_label(item.kind).lower()}",
    did: "changed a password",
    takes: [("Password", .secret)],
) {
    rewrite(item, item.title, item.kind, item.places.map(p -> p), ["\"secret\":" + typed(0)])
}

/// Writes the item's note: typed on the system's card, kept in the vault.
action write_note(item: Item) -> Text (
    icon: .file_text,
    label: "Write the note",
    did: "wrote a note",
    takes: [("Note", .secret)],
) {
    rewrite(item, item.title, item.kind, item.places.map(p -> p), ["\"notes\":" + typed(0)])
}

/// Adds a field to the item, such as a PIN: its value typed on the system's card.
action add_field(item: Item, label: Text = "") -> Text (
    icon: .plus,
    label: "Add a field",
    takes: [("Value", .secret)],
) {
    require not label.trim().is_empty() "A field needs a label"
    let kept = item.labels.map(l -> l).filter(l -> l != label.trim())
    keep(
        item,
        change_of(
            item,
            item.kind,
            item.title,
            item.places.map(p -> p),
            [],
            [
                "\"fields\":["
                    + [kept.map(l -> field_of(l, "\"\"")), [field_of(label.trim(), typed(0))]]
                        .flat_map(x -> x)
                        .join(",")
                    + "]",
            ],
        ),
    )
}

/// Takes a field off the item.
action remove_field(item: Item, label: Text) -> Text (icon: .x, label: "Remove the field") {
    keep(
        item,
        change_of(
            item,
            item.kind,
            item.title,
            item.places.map(p -> p),
            item.labels.map(l -> l).filter(l -> l != label),
            [],
        ),
    )
}

/// Draws a strong password for the login where the vault is: 20 characters by default, with a
/// capital, a small letter, a digit and a symbol. Nobody types or sees it.
action generate_password(
    item: Item,
    length: Int = 20,
    capitals: Bool = true,
    digits: Bool = true,
    symbols: Bool = true,
) -> Text (icon: .refresh, label: "Generate strong password", did: "generated a password") {
    require length >= 12 and length <= 128 "A password is 12 to 128 characters long"
    require item.kind == .login "Only a login's password is drawn by the vault"
    rewrite(
        item,
        item.title,
        item.kind,
        item.places.map(p -> p),
        [
            "\"generate\":"
                + object(
                    [
                        "\"length\":{length}",
                        "\"capitals\":{capitals}",
                        "\"digits\":{digits}",
                        "\"symbols\":{symbols}",
                    ],
                ),
        ],
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
action add_tag(item: Item, tag: Text = "") -> Text (icon: .tag, label: "Add a tag") {
    require not tag.trim().is_empty() "A tag needs a name"
    item.tags.add(tag.trim())
    rewrite(item, item.title, item.kind, item.places.map(p -> p), [])
}

/// Takes a tag off an item.
action remove_tag(item: Item, tag: Text) -> Text (icon: .x, label: "Remove the tag") {
    item.tags.remove(tag)
    rewrite(item, item.title, item.kind, item.places.map(p -> p), [])
}

/// Gives an item an address it signs in to, checked as a web address, as typed beside them.
action add_origin(item: Item, origin: Text = "") -> Text (
    icon: .globe,
    label: "Add a web address",
) {
    require not origin.trim().is_empty() "Add the address"
    require origin_of(origin) != "" "{origin.trim()} is not a web address, such as https://example.com"
    let added = origin_of(origin)
    let places = item.places.map(p -> p).filter(p -> p != added)
    rewrite(item, item.title, item.kind, [places, [added]].flat_map(x -> x), [])
}

/// Takes an address off an item.
action remove_origin(item: Item, origin: Text) -> Text (icon: .x, label: "Remove the address") {
    rewrite(item, item.title, item.kind, item.places.map(p -> p).filter(o -> o != origin), [])
}

/// Renames an item, in its record and in the vault.
action rename(item: Item, title: Text) -> Text (icon: .pencil, label: "Rename") {
    require not title.trim().is_empty() "Add a name so you can find this item"
    item.title = title.trim()
    rewrite(item, title.trim(), item.kind, item.places.map(p -> p), [])
}

/// Changes what kind of secret an item is.
action set_kind(item: Item, kind: Kind) -> Text (icon: .shield, label: "Change the kind") {
    item.kind = kind
    rewrite(item, item.title, kind, item.places.map(p -> p), [])
}

/// Puts the item's secret on the person's clipboard from the vault, which forgets it again after
/// a while. It never passes through Passwords, and the vault records the copy as a use.
action copy_secret(item: Item) -> Text (
    icon: .copy,
    label: "Copy {secret_label(item.kind).lower()}",
) {
    copy_part(item, "secret")
}

/// Puts what identifies the person to the account on their clipboard from the vault: a login's
/// username, an API key's account.
action copy_identity(item: Item) -> Text (
    icon: .user,
    label: "Copy {identity_label(item.kind).lower()}",
) {
    copy_part(
        item,
        if item.kind == .wifi and item.labels.contains("network") {
            "field:network"
        } else { "username" },
    )
}

/// Asks the vault to put `part` of the item on the person's clipboard. Answers why it could not,
/// or nothing.
action copy_part(item: Item, part: Text) -> Text (icon: .copy, label: "Copy from the vault") {
    if let arguments = parsed(object(["\"item\":" + js(reference(item)), "\"part\":" + js(part)])) {
        let answer = actions.call(address: vault_at("copy"), arguments: arguments)
        if answer.status == .ended {
            ""
        } else if let why = answer.failure { why } else { "Nothing was copied" }
    } else { "Nothing was copied" }
}

/// Copies the addresses the item signs in to, one a line.
action copy_origins(item: Item) uses clipboard (icon: .copy, label: "Copy web addresses") {
    let places = item.places.map(p -> p)
    if not places.is_empty() { clipboard.copy(places.join("\n")) }
}

/// What the vault shows of an item for a while, each part by the name the window shows it under:
/// never the secret itself, which only a view no capture reaches puts in the name's place.
type Shown {
    secret: Text
    identity: Text
    notes: Text
    fields: List<ShownField>
}

/// One field shown: its label, and the name its value is shown under.
type ShownField {
    label: Text
    value: Text
}

/// Asks the vault to show `part` of the item for a while, where `how` says (`show` for the
/// window, `hold` for the host): the name it is shown under, or nothing where the item keeps
/// none. The vault records each showing as a use.
action shown_part(item: Item, part: Text, how: Text) -> Text (
    icon: .eye,
    label: "Show from the vault",
) {
    if let arguments = parsed(
            object(["\"${how}\":true", "\"item\":" + js(reference(item)), "\"part\":" + js(part)]),
        ) {
        let answer = actions.call(address: vault_at("reveal"), arguments: arguments)
        if answer.status == .ended {
            if let out = answer.output { at_text(out, "value") } else { "" }
        } else { "" }
    } else { "" }
}

/// Shows the item's secret, who it signs in as, its note and its fields in the window, for 30
/// seconds at most: each part by a name only the window reads it under.
action reveal(item: Item) -> Shown (icon: .eye, label: "Reveal") {
    Shown(
        secret: shown_part(item, "secret", "show"),
        identity: if item.kind == .note { "" } else { shown_part(item, "username", "show") },
        notes: if item.noted and item.kind != .note {
            shown_part(item, "notes", "show")
        } else { "" },
        fields: item.labels.map(
            l -> ShownField(label: l, value: shown_part(item, "field:{l}", "show")),
        ),
    )
}

/// Deletes an item from the vault.
destructive action delete(v: Vault, item: Item) (
    icon: .trash,
    key: "Delete",
    did: "deleted an item",
) {
    if item.entry != "" {
        if let arguments = parsed(object(["\"item\":" + js(reference(item))])) {
            let answer = actions.call(address: vault_at("remove"), arguments: arguments)
            if answer.status != .ended {
                v.notice = if let why = answer.failure {
                    why
                } else { "The vault did not forget it" }
            }
        }
    }
    v.items.remove(item)
}

// ---------------------------------------------------------------- who used an item
/// Who used an item, and who may use it without its person being asked again.
type Access {
    uses: List<Use>
    leaves: List<Leave>
    /// Why the vault did not say, or nothing.
    trouble: Text
}

/// One use of an item: when, by whom, what for and where.
type Use {
    at: Int
    by: Text
    what: Text
    places: Text
}

/// One leave a person gave: who it is for, by agent or app, what for and since when.
type Leave {
    agent: Text
    for_app: Text
    by: Text
    using: Text
    since: Int
}

/// Reads who used the item, and the leaves its person gave: what the item's access page shows.
action access(item: Item) -> Access (icon: .shield_check, label: "Who used it") {
    if let arguments = parsed(object(["\"item\":" + js(reference(item))])) {
        let answer = actions.call(address: vault_at("uses"), arguments: arguments)
        if answer.status == .ended {
            if let out = answer.output {
                return Access(
                    uses: at_list(out, "uses")
                        .map(
                            u -> Use(
                                at: at_int(u, "at"),
                                by: at_text(u, "by"),
                                what: if at_text(u, "what") != "" {
                                    at_text(u, "what")
                                } else { at_text(u, "using") },
                                places: at_texts(u, "places").map(p -> host_of(p)).join(", "),
                            ),
                        )
                        .sort_by(u -> u.at, desc),
                    leaves: at_list(out, "always")
                        .map(
                            l -> Leave(
                                agent: at_text(l, "agent"),
                                for_app: at_text(l, "app"),
                                by: at_text(l, "by"),
                                using: at_text(l, "using"),
                                since: at_int(l, "since"),
                            ),
                        ),
                    trouble: "",
                )
            }
        }
        return Access(
            uses: [],
            leaves: [],
            trouble: if let why = answer.failure { why } else { "The vault did not say" },
        )
    }
    Access(uses: [], leaves: [], trouble: "The vault did not say")
}

/// Takes back a leave the item's person gave: the agent or app it names asks again next time.
action take_back(item: Item, leave: Leave) -> Access (icon: .x, label: "Take back") {
    let who = if leave.agent != "" {
        "\"agent\":" + js(leave.agent)
    } else { "\"app\":" + js(leave.for_app) }
    if let arguments = parsed(object(["\"item\":" + js(reference(item)), who])) {
        actions.call(address: vault_at("take-back"), arguments: arguments)
    }
    access(item)
}

// ---------------------------------------------------------------- keeping the list with the vault
/// Brings the vault and its items together: an item the vault keeps that is not listed (a mail
/// account's password, a connector's token, one another device added) gets a record, what the
/// vault says of each listed one reaches its record, one the vault no longer keeps leaves the
/// list, and what an older Passwords kept in its own records moves into the vault and leaves the
/// record. Runs while Passwords is open, and changes nothing when all is settled.
action settle(v: Vault) (icon: .refresh, label: "Settle the vault") {
    if not v.pending.is_empty() and v.pending.any(r -> r.title != "" and v.importing == "") {
        clear_pending(v)
    }
    for item in v.items.map(i -> i) {
        if holds_old_secrets(item) { carry(v, item) }
    }
    if let arguments = parsed(object([])) {
        let answer = actions.call(address: vault_at("list"), arguments: arguments)
        if answer.status == .ended {
            if let out = answer.output {
                let listed = at_list(out, "items")
                let ids = listed.map(one -> id_of(at_text(one, "reference")))
                for one in listed {
                    let id = id_of(at_text(one, "reference"))
                    if let item = v.items.find(i -> i.entry == id) {
                        learn(item, one)
                    } else if id != "" {
                        let made = v.items.insert(Item(entry: id, title: at_text(one, "title")))
                        learn(made, one)
                    }
                }
                for item in v.items.map(i -> i) {
                    if item.entry != ""
                        and not ids.contains(item.entry)
                        and not holds_old_secrets(item) {
                        v.items.remove(item)
                    }
                }
            }
        }
    }
}

/// The vault settles every few seconds while Passwords is open.
trigger settling(v: Vault) every 10s: settle(v)

/// Whether an item still keeps what an older Passwords put in its record.
fn holds_old_secrets(item: Item) -> Bool {
    not item.password.is_empty()
        or not item.notes.is_empty()
        or not item.username.is_empty()
        or not item.origins.is_empty()
        or not item.fields.keys().is_empty()
}

/// Moves what an item's record keeps into the vault, once, and empties the record. A record that
/// names an item the vault keeps changes that item; one that names none adds one.
action carry(v: Vault, item: Item) (icon: .key, label: "Move the secret into the vault") {
    let places = item.origins.map(o -> origin_of(o)).filter(o -> o != "")
    let secret = if item.kind == .note and item.password.is_empty() {
        item.notes
    } else { item.password }
    let notes = if item.kind == .note and item.password.is_empty() { "" } else { item.notes }
    let pairs = [
        "\"kind\":" + js(kind_word(item.kind)),
        "\"title\":" + js(item.title),
        "\"username\":" + js(item.username),
        "\"belongsTo\":" + js_list(places),
        "\"tags\":" + js_list(item.tags.map(t -> t)),
        if secret.is_empty() {
            if item.kind == .login {
                "\"generate\":" + object([])
            } else { "\"secret\":" + js("-") }
        } else { "\"secret\":" + js(secret) },
        if notes.is_empty() { "" } else { "\"notes\":" + js(notes) },
        "\"fields\":["
            + item.fields.keys().map(k -> field_of(k, js(item.fields.get(k) ?? ""))).join(",")
            + "]",
    ]
    let why = if item.entry != "" {
        keep(item, object([["\"item\":" + js(reference(item))], pairs].flat_map(x -> x)))
    } else { "no item" }
    if why != "" {
        // The vault keeps no item of that id: it is added anew, and the record names it.
        if let arguments = parsed(object(pairs)) {
            let answer = actions.call(address: vault_at("add"), arguments: arguments)
            if answer.status == .ended {
                if let out = answer.output {
                    item.entry = id_of(at_text(out, "reference"))
                    learn(item, out)
                }
            } else {
                v.notice = if let found = answer.failure {
                    found
                } else {
                    "An older item could not move into the vault"
                }
                return
            }
        }
    }
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

// ---------------------------------------------------------------- what the window reads
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

/// The one secondary fact a row shows: the site it signs in to, else its kind.
fn summary(item: Item) -> Text {
    if let site = item.places.map(p -> p).first() { host_of(site) } else { kind_label(item.kind) }
}

/// What is wrong with the item's password, in one phrase: "Reused and weak", "Not changed in a
/// year". Empty for a password nothing is wrong with.
fn reasons(item: Item) -> Text {
    let said: List<Text> = [
        if item.concerns.contains("reused") { "reused" } else { "" },
        if item.concerns.contains("weak") { "weak" } else { "" },
        if item.concerns.contains("stale") { "not changed in a year" } else { "" },
    ]
        .filter(s -> s != "")
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

/// The items in the order the window lists them, by name ignoring case: those the vault keeps.
fn listed(v: Vault) -> List<Item> {
    v.items.filter(i -> i.entry != "").sort_by(i -> i.title.lower())
}

/// When `seconds` since 1970 is.
fn moment(seconds: Int) -> Option<Time> {
    if let epoch = Time.from_wall("UTC", "1970-01-01T00:00:00") {
        .some(epoch + Duration(seconds))
    } else { .none }
}

// ---------------------------------------------------------------- the window
/// The vault window: its places in the sidebar, the items of the one chosen in the overview, and
/// the open item, the review or the transfers in the content.
view vault_window(v: Vault) {
    // The place chosen: a kind of item, the favorites, or one of the tools.
    state place: Text = "all"
    state opened: Option<Item> = listed(v).first()
    state searched: Text = ""
    // What the window shows over the open item: its secrets for a while, who used it, or it about
    // to be deleted.
    state showing: Option<Item> = .none
    state shown: Option<Shown> = .none
    state seen: Option<Access> = .none
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
                        on select(x: Item): set(opened, x)
                            .then(set(showing, .none))
                            .then(set(seen, .none))
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
                if item.entry != "" {
                    item_page(v, item, showing, shown, seen, ask_delete)
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
/// its tags, notes, custom fields and when it last changed, with the controls that write them,
/// and who used it. Its secrets show only while the person asked for them, for 30 seconds at
/// most, and never in a capture or a stream.
// ---------------------------------------------------------------- the open item
view item_page(
    v: Vault,
    item: Item,
    showing: State<Option<Item>>,
    shown: State<Option<Shown>>,
    seen: State<Option<Access>>,
    ask_delete: State<Option<Item>>,
) {
    protect capture
    state draft_title: Text = item.title
    state draft_username: Text = ""
    let open = if let s = showing { s == item } else { false }
    let said = if open { shown } else { .none }
    stack(gap: .group) {
        if open {
            after Duration(30): set(showing, .none).then(set(shown, .none))
        }
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
                    ui.Property(
                        identity_label(item.kind),
                        if let s = said {
                            if s.identity == "" { "Not set" } else { s.identity }
                        } else { "Hidden" },
                    )
                }
                ui.Property(
                    secret_label(item.kind),
                    if let s = said { s.secret } else { "••••••••••••" },
                )
                if not item.places.is_empty() {
                    ui.Property(
                        origin_label(item.kind, item.places.len()),
                        item.places.map(p -> p).join("\n"),
                    )
                }
                if not item.tags.is_empty() {
                    ui.Property("Tags", item.tags.map(t -> t).join(", "))
                }
                if item.kind != .note and item.noted {
                    ui.Property("Notes", if let s = said { s.notes } else { "Hidden" })
                }
                for label in item.labels {
                    ui.Property(
                        label,
                        if let s = said {
                            s.fields.find(f -> f.label == label)?.value ?? "••••••"
                        } else { "••••••" },
                    )
                }
            }
            if let changed = moment(item.changed) {
                row(gap: .related, align: .center) {
                    text("Last changed", role: .label)
                    ui.Time(at: changed, past: true)
                }
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Copy {secret_label(item.kind).lower()}", icon: .copy) {
                    on press: copy_secret(item)
                }
                if item.kind != .note {
                    ui.Button("Copy {identity_label(item.kind).lower()}", icon: .copy) {
                        on press: copy_identity(item)
                    }
                }
                if not item.places.is_empty() {
                    ui.Button("Copy web addresses", icon: .copy) {
                        on press: copy_origins(item)
                    }
                }
                if open {
                    ui.Button("Hide", icon: .eye) {
                        on press: set(showing, .none).then(set(shown, .none))
                    }
                } else {
                    ui.Button("Reveal", icon: .eye) {
                        on press: {
                            let answer = reveal(item)
                            set(shown, .some(answer)).then(set(showing, .some(item)))
                        }
                    }
                }
                if item.kind == .note {
                    ui.Button("Write the note", icon: .file_text) {
                        on press: write_note(item)
                    }
                } else {
                    ui.Button("Change the {secret_label(item.kind).lower()}", icon: .key) {
                        on press: change_secret(item)
                    }
                    if item.kind == .login {
                        ui.Button("Generate strong password", icon: .refresh) {
                            on press: generate_password(item)
                        }
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
                    for origin in item.places {
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
                for label in item.labels {
                    row(gap: .tight, align: .center) {
                        text(label, role: .label)
                        spacer
                        ui.Button("Remove the field", icon: .x, icon_only: true, role: .plain) {
                            on press: remove_field(item, label)
                        }
                    }
                }
                std.add_entry("Add a field, such as PIN", add_field(item))
            }
            section("Access") {
                if let a = seen {
                    access_list(item, a, seen)
                } else {
                    text(
                        "Every time the item is shown, copied or handed to an app or an agent, the vault notes it.",
                        role: .caption,
                        secondary: true,
                    )
                    ui.Button("Who used it", icon: .shield_check) {
                        on press: {
                            let read = access(item)
                            set(seen, .some(read))
                        }
                    }
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

/// Who used an item, newest first, and the leaves its person gave, each taken back from here.
view access_list(item: Item, a: Access, seen: State<Option<Access>>) {
    stack(gap: .related) {
        if a.trouble != "" {
            ui.Banner(a.trouble, kind: .warning)
        }
        if not a.leaves.is_empty() {
            text("May use it without asking", role: .label)
            for leave in a.leaves {
                row(gap: .tight, align: .center) {
                    ui.ListRow(
                        title: if leave.by != "" {
                            leave.by
                        } else if leave.for_app != "" { leave.for_app } else { leave.agent },
                        secondary: leave.using,
                        icon: .shield_check,
                    )
                    spacer
                    ui.Button("Take back", icon: .x, role: .plain) {
                        on press: {
                            let after = take_back(item, leave)
                            set(seen, .some(after))
                        }
                    }
                }
            }
        }
        if a.uses.is_empty() {
            text("Nobody used it yet.", role: .caption, secondary: true)
        } else {
            for one_use in a.uses.take(20) {
                ui.ListRow(
                    title: if one_use.by != "" { one_use.by } else { "You" },
                    secondary: if one_use.places != "" {
                        "{one_use.what} · {one_use.places}"
                    } else { one_use.what },
                    icon: .clock,
                )
            }
        }
        ui.Button("Close", role: .plain) {
            on press: set(seen, .none)
        }
    }
}

/// The review: the passwords that are reused, weak or old, each opening its item.
// ---------------------------------------------------------------- the review
view review_page(v: Vault) {
    let checked = listed(v).filter(i -> i.kind == .login or i.kind == .wifi)
    let total = checked.len()
    let reused = checked.filter(i -> i.concerns.contains("reused")).len()
    let weak = checked.filter(i -> i.concerns.contains("weak")).len()
    let old = checked.filter(i -> i.concerns.contains("stale")).len()
    let troubled = checked.filter(i -> not i.concerns.is_empty())
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
                        for item in troubled {
                            ui.ListRow(
                                title: item.title,
                                secondary: reasons(item),
                                item: item,
                                icon: item.kind.icon(),
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
                            skip: holds_login(v, row.title, row.origin),
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

/// Whether the vault already holds that login: the same name, and the same address.
fn holds_login(v: Vault, title: Text, origin: Text) -> Bool {
    let wanted = origin_of(origin)
    v.items.any(
        i -> i.title.lower() == title.trim().lower()
            and if wanted == "" { i.places.is_empty() } else { i.places.contains(wanted) },
    )
}

/// Adds an item the host read from a file the person chose: `item`, its secrets each named by the
/// token the host holds it under. Answers whether the vault kept it.
action add_read(v: Vault, item: HeldItem) -> Bool (icon: .plus, label: "Add what was read") {
    let kind = kind_of(item.kind)
    let places = item.origins.map(o -> origin_of(o)).filter(o -> o != "")
    let secret = if item.password != "" {
        item.password
    } else if kind == .note { item.notes } else { "" }
    let notes = if item.password != "" { item.notes } else { "" }
    if secret == "" {
        return false
    }
    let made = add_new(
        v,
        object(
            [
                "\"kind\":" + js(kind_word(kind)),
                "\"title\":" + js(item.title),
                "\"username\":" + js(item.username),
                "\"belongsTo\":" + js_list(places),
                "\"tags\":" + js_list(item.tags),
                "\"secret\":" + held(secret),
                if notes == "" { "" } else { "\"notes\":" + held(notes) },
                "\"fields\":["
                    + item.fields
                        .filter(f -> f.value != "")
                        .map(f -> field_of(f.label, held(f.value)))
                        .join(",")
                    + "]",
            ],
        ),
    )
    made != .none
}

/// Brings the rows the person agreed to into the vault, skipping the ones the vault holds. The
/// host reads their passwords from the file and the vault takes them from the host.
action apply_import(v: Vault) (icon: .check, label: "Import the logins") {
    let rows = v.pending.map(r -> r)
    let skip = rows
        .filter(r -> r.skip or holds_login(v, r.title, r.origin))
        .map(r -> rows.index_of(r) ?? 0)
    match engine.call("vault-read-csv", json.encode(ImportAsk(url: v.importing, skip: skip))) {
        .ok(text) => match json.decode<HeldItems>(text) {
            .ok(answer) => {
                let kept = answer.items.filter(item -> add_read(v, item)).len()
                v.notice = if skip.is_empty() {
                    if kept == 1 { "Imported 1 login" } else { "Imported {kept} logins" }
                } else {
                    "Imported {kept} logins and skipped {skip.len()}"
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

/// Every listed item with its secrets, each named by the token the host holds it under: what the
/// host writes into a file the person chose. The vault records each part handed over as a use.
action held_items(v: Vault) -> List<HeldItem> (
    icon: .download,
    label: "Read the vault for a file",
) {
    listed(v)
        .map(
            i -> HeldItem(
                kind: file_kind(i.kind),
                title: i.title,
                username: if i.kind == .note { "" } else { shown_part(i, "username", "hold") },
                origins: i.places.map(p -> p),
                tags: i.tags.map(t -> t),
                password: shown_part(i, "secret", "hold"),
                notes: if i.noted and i.kind != .note {
                    shown_part(i, "notes", "hold")
                } else { "" },
                fields: i.labels.map(
                    l -> HeldField(label: l, value: shown_part(i, "field:{l}", "hold")),
                ),
            ),
        )
}

/// The vault as a browser's CSV: the person chooses where it goes.
action export_csv(v: Vault) -> File (icon: .download, label: "Export to CSV") {
    let items = held_items(v)
    match engine.call("vault-export-csv", json.encode(ToFile(items: items, passphrase: ""))) {
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
    match engine.call("vault-hold-typed", json.encode(TypedAsk(place: 0))) {
        .ok(text) => match json.decode<TokenAnswer>(text) {
            .ok(passphrase) => {
                let items = held_items(v)
                match engine.call(
                    "vault-export-backup",
                    json.encode(ToFile(items: items, passphrase: passphrase.token)),
                ) {
                    .ok(made) => match json.decode<UrlAnswer>(made) {
                        .ok(answer) => File.at("passwords-backup.tilopw", answer.url),
                        .err(why) => File.of_text("The backup could not be made.txt", why),
                    },
                    .err(why) => File.of_text("The backup could not be made.txt", why),
                }
            }
            .err(why) => File.of_text("The backup could not be made.txt", why),
        },
        .err(why) => File.of_text("The backup could not be made.txt", why),
    }
}

/// Brings the entries a backup holds into the vault with the password the person types on the
/// system's card, and answers how many came in. An item the vault already lists under its name
/// and address is left as it is, so restoring twice adds nothing.
action restore_backup(v: Vault, file: File) -> Int (
    icon: .lock_open,
    label: "Restore from a backup",
    takes: [("Backup password", .secret)],
) {
    match engine.call("vault-read-backup", json.encode(AskedFile(url: file.url))) {
        .ok(text) => match json.decode<HeldItems>(text) {
            .ok(answer) => {
                let restored = answer.items
                    .filter(
                        item -> not holds_login(v, item.title, item.origins.first() ?? "")
                            and add_read(v, item),
                    )
                    .len()
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

/// Takes into the vault what this device's secret service holds and a workspace may share. The
/// device's authentication material and its Wi-Fi networks stay where they are, and each entry
/// leaves the device's service once the vault keeps it.
// ---------------------------------------------------------------- this device's vault
action absorb_device(v: Vault) (icon: .key, label: "Import this device's vault") {
    match engine.call("vault-device-items", "") {
        .ok(text) => match json.decode<DeviceItems>(text) {
            .ok(answer) => {
                if answer.error != "" {
                    v.notice = answer.error
                } else {
                    let carried = answer.items
                        .filter(item -> add_read(v, item))
                        .map(item -> item.id)
                    if not carried.is_empty() {
                        match engine.call(
                            "vault-device-remove",
                            json.encode(DeviceRemove(ids: carried)),
                        ) {
                            .ok(_) => {}
                            .err(why) => {
                                v.notice = why
                            }
                        }
                    }
                    if carried.len() == 1 {
                        v.notice = "Imported 1 login from this device"
                    } else if carried.len() > 1 {
                        v.notice = "Imported {carried.len()} logins from this device"
                    }
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

// ---------------------------------------------------------------- what the host is asked
/// Which thing the person typed on the card the host holds for a later step.
type TypedAsk {
    place: Int
}

/// The token the host holds a secret under.
type TokenAnswer {
    token: Text
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

/// An item read from or written to a file, its secrets each named by the token the host holds it
/// under.
type HeldItem {
    id: Text = ""
    kind: Text
    title: Text
    username: Text
    origins: List<Text>
    tags: List<Text>
    password: Text
    notes: Text
    fields: List<HeldField>
}

/// A field of such an item: its label, and the token its value is held under.
type HeldField {
    label: Text
    value: Text
}

/// The items a backup holds, as the host holds them.
type HeldItems {
    items: List<HeldItem>
}

/// What this device's own older vault holds, as the host holds it, or why it could not be read.
type DeviceItems {
    items: List<HeldItem>
    error: Text
}

/// The entries of this device's own older vault to take out of it.
type DeviceRemove {
    ids: List<Text>
}

/// What a file is made of: the items, and for a backup the token of its passphrase.
type ToFile {
    items: List<HeldItem>
    passphrase: Text
}

/// Where an engine kept what it made.
type UrlAnswer {
    url: Text
}

// ---------------------------------------------------------------- scenarios
/// A vault as a person who has used Passwords for a while finds it: logins, a key and a note, one
/// of them kept as a favorite. Every secret stays in the vault; these are the records the window
/// lists, so a capture of this device shows the vault's window with items in it.
scenario "a vault of logins" {
    let v = root Vault
    let fastmail = v.items.insert(
        Item(
            entry: "f7d2a1",
            title: "Fastmail",
            kind: .login,
            favorite: true,
            noted: true,
            changed: 1763370000,
        ),
    )
    fastmail.places.add("https://app.fastmail.com")
    fastmail.labels.add("username")
    fastmail.labels.add("password")
    fastmail.labels.add("Recovery code")
    let bank = v.items.insert(
        Item(entry: "b31c09", title: "Garden bank", kind: .login, changed: 1761120000),
    )
    bank.places.add("https://bank.example.org")
    bank.labels.add("Card number")
    bank.labels.add("PIN")
    bank.concerns.add("stale")
    let key = v.items.insert(
        Item(entry: "9a4e12", title: "OpenStreetMap key", kind: .api_key, changed: 1759320000),
    )
    key.labels.add("Token")
    let backup = v.items.insert(
        Item(entry: "5c7f30", title: "Recovery codes", kind: .note, changed: 1756670000),
    )
    backup.noted = true
    let flat = v.items.insert(
        Item(entry: "2e8b55", title: "Flat Wi-Fi", kind: .wifi, changed: 1754680000),
    )
    flat.labels.add("Password")
}
