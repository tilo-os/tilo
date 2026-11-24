app so.tilo.totp "Authenticator" (
    icon: .shield_check,
    category: .utility,
    tagline: "Makes the one-time codes a service asks for",
    keywords: ["authenticator", "two-factor", "2fa", "totp", "otp", "one-time code", "verification code", "security key", "authentifizierung", "zwei-faktor", "einmalcode", "sicherheitsschlüssel"],
    splash_budget: 250,
)

/// One account a service asks a one-time code for: its name and the code it makes now.
///
/// The key the service gave is not here. It stays in this device's secret store under the
/// account's id, and the engine makes each code from it, so a document never holds a key.
type Account (title: issuer, untitled: "Account") {
    /// The service it is for, which names the account.
    issuer: Text = ""
    /// Who it signs in as: an email, a username.
    label: Text = ""
    /// The hash the code is made with: `SHA1`, `SHA256` or `SHA512`.
    algorithm: Text = "SHA1"
    digits: Int = 6
    /// The seconds a code lasts: 30 for nearly every service.
    period: Int = 30
    /// A colour slot the card shows it by.
    color: Int = 0
    /// The code as of `at`, grouped in threes.
    code: Text = ""
    /// The instant the code changes, and the one it was made at, in milliseconds since 1970.
    next: Time = Time("1970-01-01T00:00:00Z")
    changed: Time = Time("1970-01-01T00:00:00Z")

    search {
        sort issuer a_z
    }
}

/// The workspace's Authenticator: the accounts its members keep, and what the window last said.
type Vault {
    accounts: Collection<Account>
    notice: Text = ""
    notice_kind: Text = "info"
}

root vault: Vault

default view vault_window for Vault

/// Every second: each account's code as the clock makes it now. A code is written only when it
/// changed, so the changes a code makes are one per period.
trigger watch(v: Vault) every 1s: refresh(v)

// ---------------------------------------------------------------- the device's answers
type CodeAsk {
    account: Text
    algorithm: Text = "SHA1"
    digits: Int = 6
    period: Int = 30
    at_ms: Int = 0
    at: Text = ""
}

type CodeAnswer {
    code: Text = ""
    left: Float = 0.0
}

type ParseAsk {
    uri: Text
}

type ParsedAnswer {
    issuer: Text = ""
    label: Text = ""
    secret: Text = ""
    algorithm: Text = "SHA1"
    digits: Int = 6
    period: Int = 30
}

type ScanAsk {
    address: Text
}

type ScanAnswer {
    uri: Text = ""
}

type CheckAsk {
    secret: Text
}

type CheckAnswer {
    ok: Bool = false
}

type KeepAsk {
    account: Text
    secret: Text
}

type SeedAsk {
    account: Text
}

type SeedAnswer {
    secret: Text = ""
}

type ForgetAsk {
    account: Text
}

/// The code an account's key makes now, and the instant it changes. The key is this device's:
/// the engine reads it from the secret store under the account's id.
fn code_at(a: Account, algorithm: Text, digits: Int, period: Int, at: Time) -> Option<CodeAnswer> {
    let ask = CodeAsk(
        account: a.id(),
        algorithm: algorithm,
        digits: digits,
        period: period,
        at: "{at.iso()}",
    )
    match engine.call("code", json.encode(ask)) {
        .ok(answer) => match json.decode<CodeAnswer>(answer) {
            .ok(code) => .some(code),
            .err(_) => .none,
        },
        .err(_) => .none,
    }
}

/// Keeps the key the service gave for `a` in this device's secret store, answering the words a
/// person reads when it could not be kept.
fn keep_seed(a: Account, secret: Text) -> Text {
    match engine.call("keep", json.encode(KeepAsk(account: a.id(), secret: secret))) {
        .ok(_) => "",
        .err(why) => why,
    }
}

/// The key this device keeps for `a`: what the form shows when the account is changed.
fn read_seed(a: Account) -> Text {
    match engine.call("read", json.encode(SeedAsk(account: a.id()))) {
        .ok(answer) => match json.decode<SeedAnswer>(answer) {
            .ok(found) => found.secret,
            .err(_) => "",
        },
        .err(_) => "",
    }
}

/// Lets go of the key this device kept for `a`, as deleting the account does.
fn forget_seed(a: Account) -> Bool {
    match engine.call("forget", json.encode(ForgetAsk(account: a.id()))) {
        .ok(_) => true,
        .err(_) => false,
    }
}

/// Whether a key reads as base32: what the form checks before it saves.
fn valid_secret(secret: Text) -> Bool {
    match engine.call("check", json.encode(CheckAsk(secret: secret))) {
        .ok(answer) => match json.decode<CheckAnswer>(answer) {
            .ok(check) => check.ok,
            .err(_) => false,
        },
        .err(_) => false,
    }
}

/// Starts an account from what the person typed, or what a setup link said: what an agent, a
/// script or the form asks.
// ---------------------------------------------------------------- keeping the accounts
action add_account(
    v: Vault,
    issuer: Text,
    label: Text = "",
    secret: Text,
    algorithm: Text = "SHA1",
    digits: Int = 6,
    period: Int = 30,
) -> Account (icon: .plus, label: "Add account", did: "added an account") {
    require issuer.trim() != "" "The account needs a service name"
    require secret.trim() != "" "The account needs the key the service gave"
    require digits >= 4 and digits <= 10 "A code has 4 to 10 digits"
    require period >= 5 and period <= 300 "A code changes every 5 to 300 seconds"
    let made = v.accounts.insert(
        Account(
            issuer: issuer.trim(),
            label: label.trim(),
            algorithm: algorithm,
            digits: digits,
            period: period,
        ),
    )
    let why = keep_seed(made, secret.trim())
    if why != "" {
        v.accounts.remove(made)
        return refuse(v, why)
    }
    refresh_one(made)
    made
}

/// Adds the account a setup link names: what the QR scan and the paste field both end in.
action import_uri(v: Vault, uri: Text) -> Account (
    icon: .link,
    label: "Add from a setup link",
    did: "added an account from a setup link",
) {
    let asked = parse_uri(uri)
    if asked == .none {
        return refuse(v, "That is not a setup link this app reads")
    }
    let setup = asked ?? ParsedAnswer()
    add_account(
        v,
        setup.issuer,
        setup.label,
        setup.secret,
        setup.algorithm,
        setup.digits,
        setup.period,
    )
}

/// Adds the account a picture's QR code names.
action import_picture(v: Vault, file: File) -> Account (
    icon: .qr_code,
    label: "Add from a picture",
    did: "added an account from a picture",
) {
    match engine.call("scan", json.encode(ScanAsk(address: file.url))) {
        .ok(answer) => match json.decode<ScanAnswer>(answer) {
            .ok(scan) => import_uri(v, scan.uri),
            .err(why) => refuse(v, why),
        },
        .err(why) => refuse(v, why),
    }
}

fn parse_uri(uri: Text) -> Option<ParsedAnswer> {
    match engine.call("parse", json.encode(ParseAsk(uri: uri))) {
        .ok(answer) => match json.decode<ParsedAnswer>(answer) {
            .ok(setup) => .some(setup),
            .err(_) => .none,
        },
        .err(_) => .none,
    }
}

/// Changes what an account says, its key included (a service that reissued it).
action update_account(
    a: Account,
    issuer: Text,
    label: Text = "",
    secret: Text,
    algorithm: Text = "SHA1",
    digits: Int = 6,
    period: Int = 30,
) (icon: .pencil, label: "Save the account", did: "changed an account") {
    require issuer.trim() != "" "The account needs a service name"
    require secret.trim() != "" "The account needs the key the service gave"
    require digits >= 4 and digits <= 10 "A code has 4 to 10 digits"
    require period >= 5 and period <= 300 "A code changes every 5 to 300 seconds"
    require keep_seed(a, secret.trim()) == "" "The key could not be kept on this device"
    a.issuer = issuer.trim()
    a.label = label.trim()
    a.algorithm = algorithm
    a.digits = digits
    a.period = period
    refresh_one(a)
}

/// Moves an account one place up the list.
action move_up(v: Vault, a: Account) (
    icon: .arrow_up,
    label: "Move up",
    did: "moved an account up",
) {
    let at = v.accounts.index_of(a) ?? 0
    if at > 0 { v.accounts.move(a, to: at - 1) }
}

/// Moves an account one place down the list.
action move_down(v: Vault, a: Account) (
    icon: .arrow_down,
    label: "Move down",
    did: "moved an account down",
) {
    let at = v.accounts.index_of(a) ?? 0
    if at + 1 < v.accounts.len() { v.accounts.move(a, to: at + 1) }
}

/// Deletes an account and the key this device kept for it.
destructive action delete_account(v: Vault, a: Account) (
    icon: .trash,
    label: "Delete the account",
    did: "deleted an account",
) {
    v.accounts.remove(a)
    forget_seed(a)
}

/// Copies an account's current code to the clipboard.
action copy_code(a: Account) uses clipboard (
    icon: .copy,
    label: "Copy code",
    did: "copied a code",
) {
    if a.code == "" { refresh_one(a) }
    if a.code != "" { clipboard.copy(a.code.replace(" ", "")) }
}

/// Makes every account's current code: what the window's clock trigger runs each second, and
/// what the list reads. A code is written only when it changed, so the changes a code makes are
/// one per period, not one per second.
action refresh(v: Vault) uses time {
    for a in v.accounts {
        let code = code_at(a, a.algorithm, a.digits, a.period, now)
        if let made = code {
            let bare = made.code
            if a.code.replace(" ", "") != bare {
                a.code = group_code(bare)
                a.changed = now
                a.next = now + Duration(a.period)
            }
        }
    }
}

action refresh_one(a: Account) uses time {
    if let made = code_at(a, a.algorithm, a.digits, a.period, now) {
        a.code = group_code(made.code)
        a.changed = now
        a.next = now + Duration(a.period)
    }
}

/// A code in threes, as a person reads it out: `123 456`.
fn group_code(code: Text) -> Text {
    let digits = code.split("")
    let groups = digits.map((d, i) -> if i > 0 and i % 3 == 0 { " {d}" } else { d })
    groups.join("")
}

action refuse(v: Vault, why: Text) -> Account {
    v.notice = why
    v.notice_kind = "warning"
    Account()
}

// ---------------------------------------------------------------- the window
view vault_window(v: Vault) {
    // What the window shows: the list, or the sheet that starts or changes an account.
    state screen: Text = "list"
    state editing: Option<Account> = .none
    // What the form is being typed with.
    state issuer: Text = ""
    state label: Text = ""
    state secret: Text = ""
    state algorithm: Text = "SHA1"
    state digits: Int = 6
    state period: Int = 30
    state link: Text = ""
    ui.NavShell(content_kind: .list, title: "Authenticator", count: v.accounts.len()) {
        actions: {
            row(gap: .related, align: .center) {
                if screen == "list" {
                    ui.Button("Add account", icon: .plus, role: .primary, key: "Ctrl+N") {
                        on press: set(editing, .none)
                            .then(set(issuer, ""))
                            .then(set(label, ""))
                            .then(set(secret, ""))
                            .then(set(algorithm, "SHA1"))
                            .then(set(digits, 6))
                            .then(set(period, 30))
                            .then(set(screen, "form"))
                    }
                } else {
                    ui.Button("Back", icon: .chevron_left, role: .plain) {
                        on press: set(screen, "list")
                    }
                    ui.Button("Scan a QR image", icon: .qr_code) {
                        on press: file.choose(import_picture(v), accept: ["image/*"])
                    }
                    ui.Button("Save", icon: .check, role: .primary) {
                        on press: save_form(
                                v,
                                editing,
                                issuer,
                                label,
                                secret,
                                algorithm,
                                digits,
                                period,
                            )
                            .then(set(screen, "list"))
                    }
                }
            }
        }
        content: {
            if screen == "form" {
                account_form(v, editing, issuer, label, secret, algorithm, digits, period, link)
            } else if v.accounts.is_empty() {
                ui.EmptyState(
                    "No accounts yet",
                    icon: .shield_check,
                    message: "Scan a QR code or paste a setup link to see its one-time codes here.",
                    action: "Add account",
                ) {
                    on act: set(editing, .none)
                        .then(set(issuer, ""))
                        .then(set(label, ""))
                        .then(set(secret, ""))
                        .then(set(algorithm, "SHA1"))
                        .then(set(digits, 6))
                        .then(set(period, 30))
                        .then(set(screen, "form"))
                }
            } else {
                grid(min: 20rem) {
                    for a in v.accounts {
                        account_card(
                            v,
                            a,
                            screen,
                            editing,
                            issuer,
                            label,
                            secret,
                            algorithm,
                            digits,
                            period,
                        )
                    }
                }
            }
        }
        band: {
            status(v)
        }
    }
}

/// Saves the form: starts the account, or changes the one it was opened over.
action save_form(
    v: Vault,
    editing: Option<Account>,
    issuer: Text,
    label: Text,
    secret: Text,
    algorithm: Text,
    digits: Int,
    period: Int,
) {
    if issuer.trim() == "" {
        v.notice = "The account needs a service name"
        v.notice_kind = "warning"
        return
    }
    if not valid_secret(secret) {
        v.notice = "That key is not base32; copy it from the service again"
        v.notice_kind = "warning"
        return
    }
    if let a = editing {
        update_account(a, issuer, label, secret, algorithm, digits, period)
    } else {
        add_account(v, issuer, label, secret, algorithm, digits, period)
    }
    v.notice = ""
    v.notice_kind = "info"
}

/// One account's card: its code, how long that code lasts, and what a press can do with it.
view account_card(
    v: Vault,
    a: Account,
    screen: State<Text>,
    editing: State<Option<Account>>,
    issuer: State<Text>,
    label: State<Text>,
    secret: State<Text>,
    algorithm: State<Text>,
    digits: State<Int>,
    period: State<Int>,
) {
    ui.Card(title: if a.issuer == "" { "Account" } else { a.issuer }, item: a) {
        if a.label != "" {
            ui.Text(a.label, role: .caption, secondary: true, truncate: true)
        }
        row(gap: .section, align: .center) {
            ui.Text(if a.code == "" { "—" } else { a.code }, role: .display, numeric: true)
            ui.Countdown(
                at: a.next,
                total: a.period.to_float(),
                label: "Code lifetime",
                size: .small,
            )
        }
        row(gap: .related, align: .center) {
            ui.Button("Copy", icon: .copy, role: .primary, size: .small) {
                on press: copy_code(a)
            }
            ui.Button("Edit", icon: .pencil, size: .small) {
                on press: set(editing, .some(a))
                    .then(set(issuer, a.issuer))
                    .then(set(label, a.label))
                    .then(set(secret, read_seed(a)))
                    .then(set(algorithm, a.algorithm))
                    .then(set(digits, a.digits))
                    .then(set(period, a.period))
                    .then(set(screen, "form"))
            }
            ui.MenuButton("More", icon: .ellipsis, icon_only: true, role: .plain) {
                ui.MenuItem("Move up", icon: .arrow_up) {
                    on choose: move_up(v, a)
                }
                ui.MenuItem("Move down", icon: .arrow_down) {
                    on choose: move_down(v, a)
                }
                ui.MenuSeparator()
                ui.MenuItem("Delete account", icon: .trash, destructive: true) {
                    on choose: delete_account(v, a)
                }
            }
        }
    }
}

/// The form that starts or changes an account: a QR scan, a setup link, or the details typed.
view account_form(
    v: Vault,
    editing: Option<Account>,
    issuer: State<Text>,
    label: State<Text>,
    secret: State<Text>,
    algorithm: State<Text>,
    digits: State<Int>,
    period: State<Int>,
    link: State<Text>,
) {
    stack(gap: .related, padding: .group, max: 40rem) {
        ui.Text(if editing == .none { "Add account" } else { "Edit account" }, role: .title)
        ui.Text(
            "Scan a QR code, paste a setup link, or type the details the service gave.",
            role: .caption,
            secondary: true,
        )
        row(gap: .related, align: .center) {
            ui.TextInput(bind: link, placeholder: "Setup link", entry: true, fill: 1.0) {
                on submit(uri): import_uri(v, uri).then(set(link, ""))
            }
            ui.Button("Add from the link", icon: .link) {
                on press: import_uri(v, link).then(set(link, ""))
            }
        }
        ui.SectionDivider()
        ui.TextInput(bind: issuer, placeholder: "Service name", title: true)
        ui.TextInput(bind: label, placeholder: "Email or username")
        ui.TextInput(bind: secret, placeholder: "Key from the service", secret: true)
        ui.DisclosureRow(title: "Advanced options") {
            stack(gap: .related) {
                ui.ToggleGroup(
                    selected: if algorithm == "SHA256" {
                        1
                    } else if algorithm == "SHA512" { 2 } else { 0 },
                    caption: "Algorithm",
                ) {
                    ui.Segment("SHA1")
                    ui.Segment("SHA256")
                    ui.Segment("SHA512")
                    on change(i): set(
                        algorithm,
                        if i == 1 { "SHA256" } else if i == 2 { "SHA512" } else { "SHA1" },
                    )
                }
                ui.NumberInput(bind: digits, label: "Digits", min: 4, max: 10)
                ui.NumberInput(bind: period, label: "Period in seconds", min: 5, max: 300)
            }
        }
        if v.notice != "" {
            ui.Banner(
                v.notice,
                kind: if v.notice_kind == "warning" { .warning } else { .neutral },
                icon: if v.notice_kind == "warning" { .triangle_alert } else { .info },
                action: "Dismiss",
            ) {
                on act: clear_notice(v)
            }
        }
    }
}

action clear_notice(v: Vault) {
    v.notice = ""
}

/// The bar under the window: how many accounts the vault holds.
view status(v: Vault) {
    ui.StatusLine(count(v.accounts.len()), status: .idle)
}

fn count(n: Int) -> Text {
    if n == 1 { "1 account" } else { "{n} accounts" }
}
