app so.tilo.totp "Authenticator" (
    icon: .shield_check,
    category: .utility,
    tagline: "Makes the one-time codes a service asks for",
    keywords: ["authenticator", "authenticator app", "two-factor", "2fa", "mfa", "totp", "otp", "one-time code", "one-time password", "login code", "verification code", "security key", "authentifizierung", "authenticator-app", "zwei-faktor", "einmalcode", "einmalpasswort", "anmeldecode", "sicherheitsschlüssel"],
    splash_budget: 250,
)

use so.tilo.vault as vault
use so.tilo.device as device

/// One account a service asks a one-time code for: its name, and the vault item that holds the
/// key the service gave.
///
/// The key is not here and never leaves the vault. The workspace's vault keeps it with the
/// account's hash, digits and period; this app keeps the names and the order, and shows each
/// code where the vault makes it.
type Account (title: issuer, untitled: "Account") {
    /// The service it is for, which names the account.
    issuer: Text = ""
    /// Who it signs in as: an email, a username.
    label: Text = ""
    /// The vault item that holds the seed, by its reference (`vault.item:<id>`).
    item: Text = ""
    /// A colour slot the card shows it by.
    color: Int = 0

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

// ---------------------------------------------------------------- the device's answers
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

/// The key this device keeps, validated as base32 before it is kept: what the form checks.
fn valid_secret(secret: Text) -> Bool {
    match engine.call("check", json.encode(CheckAsk(secret: secret))) {
        .ok(answer) => match json.decode<CheckAnswer>(answer) {
            .ok(check) => check.ok,
            .err(_) => false,
        },
        .err(_) => false,
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

/// Starts an account from what the person typed, or what a setup link said: what an agent, a
/// script or the form asks. The vault keeps the seed; the account keeps the words.
// ---------------------------------------------------------------- keeping the accounts
primary action add_account(
    v: Vault,
    issuer: Text,
    label: Text = "",
    secret: Text,
    algorithm: Text = "SHA1",
    digits: Int = 6,
    period: Int = 30,
    color: Int = 0,
) -> Account (icon: .plus, label: "Add account", did: "added an account") {
    require issuer.trim() != "" "The account needs a service name"
    require secret.trim() != "" "The account needs the key the service gave"
    require digits >= 4 and digits <= 10 "A code has 4 to 10 digits"
    require period >= 5 and period <= 300 "A code changes every 5 to 300 seconds"
    let reference = vault.keep(
        item: .none,
        issuer: issuer.trim(),
        label: label.trim(),
        secret: secret.trim(),
        algorithm: algorithm,
        digits: digits,
        period: period,
    )
    v.accounts.insert(
        Account(issuer: issuer.trim(), label: label.trim(), item: reference, color: color),
    )
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

/// Adds the account a picture's QR code names, and says the code was read: the person picked a
/// picture and the window shows what came of it.
action import_picture(v: Vault, file: File) -> Account (
    icon: .qr_code,
    label: "Add from a picture",
    did: "added an account from a picture",
) {
    match engine.call("scan", json.encode(ScanAsk(address: file.url))) {
        .ok(answer) => match json.decode<ScanAnswer>(answer) {
            .ok(scan) => {
                let made = import_uri(v, scan.uri)
                if made.issuer != "" { notice("QR code read", kind: .success) }
                made
            }
            .err(why) => refuse(v, why),
        },
        .err(why) => refuse(v, why),
    }
}

/// Changes what an account says, and its key and how its codes are made. A key left out keeps
/// the one the vault holds: the service's key never leaves it, so changing an account's details
/// can never show it.
action update_account(
    a: Account,
    issuer: Text,
    label: Text = "",
    secret: Text = "",
    algorithm: Text = "SHA1",
    digits: Int = 6,
    period: Int = 30,
    color: Int = 0,
) (icon: .pencil, label: "Save the account", did: "changed an account") {
    require issuer.trim() != "" "The account needs a service name"
    require digits >= 4 and digits <= 10 "A code has 4 to 10 digits"
    require period >= 5 and period <= 300 "A code changes every 5 to 300 seconds"
    let reference = vault.keep(
        item: if a.item == "" { .none } else { .some(a.item) },
        issuer: issuer.trim(),
        label: label.trim(),
        secret: secret.trim(),
        algorithm: algorithm,
        digits: digits,
        period: period,
    )
    a.issuer = issuer.trim()
    a.label = label.trim()
    a.color = color
    a.item = reference
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

/// Deletes an account and the key the vault kept for it.
destructive action delete_account(v: Vault, a: Account) (
    icon: .trash,
    label: "Delete the account",
    did: "deleted an account",
) {
    // The key is read before the account goes: a record that left the list answers nothing.
    let item = a.item
    v.accounts.remove(a)
    if item != "" { vault.forget(item: item) }
}

/// Copies an account's current code to the clipboard, and says so: the person asked for it, and
/// a window that shows nothing would leave them guessing whether it worked.
action copy_code(a: Account) uses secret, clipboard (
    icon: .copy,
    label: "Copy code",
    did: "copied a code",
) {
    if let made = vault_code(a.item) {
        if clipboard.copy(made.code) {
            notice("Copied {a.issuer} code", kind: .success)
        } else {
            // The clipboard did not take it: the person asked for the code, and the bar says it
            // is not there rather than leaving it looking copied.
            notice("Could not copy the code", kind: .error)
        }
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
    protect capture
    // Every code this window draws is left out of a capture: a screenshot of the window, or of
    // the screen it stands on, shows the accounts and never the digits.
    // What the window shows: the list, or the sheet that starts or changes an account.
    state screen: Text = "list"
    // The window's own second: drawing with it asks the vault for each account's code again, so
    // the code and its countdown move while the window is shown. Nothing of the code is kept.
    state second: Int = 0
    state editing: Option<Account> = .none
    // The account whose code this window shows until the second in `shown_until`: a code is
    // veiled at rest, and a press shows it for ten seconds.
    state shown: Text = ""
    state shown_until: Int = 0
    // The form's fields, and whether its advanced options are open.
    state advanced: Bool = false
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
                            .then(set(advanced, false))
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
                        // The list is where a person goes once the account is saved, and nowhere
                        // else: a form that keeps what was typed open is what a refusal leaves.
                        // A number out of range is what sends them to the options it is under.
                        on press: {
                            let saved = save_form(
                                v,
                                editing,
                                issuer,
                                label,
                                secret,
                                algorithm,
                                digits,
                                period,
                            )
                            if saved {
                                set(screen, "list").then(set(advanced, false))
                            } else {
                                // Nothing moves: the form stays as it was, with its refusal said
                                // in the bar. An action either way, so the press is one.
                                set(advanced, advanced)
                            }
                        }
                    }
                }
            }
        }
        content: {
            if screen == "form" {
                account_form(
                    v,
                    editing,
                    issuer,
                    label,
                    secret,
                    algorithm,
                    digits,
                    period,
                    link,
                    advanced,
                )
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
                        .then(set(advanced, false))
                        .then(set(screen, "form"))
                }
            } else {
                stack(gap: .related) {
                    // The window's own second, drawn while it lists accounts: the vault answers
                    // each code again, so the digits and their countdown move. Nothing is kept.
                    every 1s: set(second, second + 1)
                    clock_warning()
                    ui.CardGrid(name: "Accounts", card_width: .regular) {
                        // The selection the arrow keys move is the grid's, and the keys a card
                        // answers are its own: Enter reveals, F2 edits, Delete deletes.
                        on edit(a: Account): set(editing, .some(a))
                            .then(set(issuer, a.issuer))
                            .then(set(label, a.label))
                            .then(set(secret, ""))
                            .then(set(algorithm, "SHA1"))
                            .then(set(digits, 6))
                            .then(set(period, 30))
                            .then(set(advanced, false))
                            .then(set(screen, "form"))
                        on delete(a: Account): delete_account(v, a)
                        for a in v.accounts {
                            account_card(
                                v,
                                a,
                                second,
                                shown,
                                shown_until,
                                screen,
                                editing,
                                issuer,
                                label,
                                secret,
                                algorithm,
                                digits,
                                period,
                                advanced,
                            )
                        }
                    }
                }
            }
        }
        band: {
            status(v)
        }
    }
}

/// Saves the form: starts the account, or changes the one it was opened over, answering whether
/// it saved. A refusal says why in the bar and leaves the form open with what was typed, so a
/// window only leaves the form when this answered that it saved.
action save_form(
    v: Vault,
    editing: Option<Account>,
    issuer: Text,
    label: Text,
    secret: Text,
    algorithm: Text,
    digits: Int,
    period: Int,
) -> Bool {
    if issuer.trim() == "" {
        v.notice = "The account needs a service name"
        v.notice_kind = "warning"
        return false
    }
    if digits < 4 or digits > 10 {
        v.notice = "A code has 4 to 10 digits"
        v.notice_kind = "warning"
        return false
    }
    if period < 5 or period > 300 {
        v.notice = "A code changes every 5 to 300 seconds"
        v.notice_kind = "warning"
        return false
    }
    if editing == .none and secret.trim() == "" {
        v.notice = "The account needs the key the service gave"
        v.notice_kind = "warning"
        return false
    }
    if secret.trim() != "" and not valid_secret(secret) {
        v.notice = "That key is not base32; copy it from the service again"
        v.notice_kind = "warning"
        return false
    }
    if let a = editing {
        update_account(a, issuer, label, secret, algorithm, digits, period, a.color)
    } else {
        add_account(v, issuer, label, secret, algorithm, digits, period)
    }
    v.notice = ""
    v.notice_kind = "info"
    true
}

/// One account's card: its code as the vault makes it now, how long that code lasts, and what a
/// press can do with it. The code is veiled at rest; a press shows it for ten seconds, and the
/// same control copies it then. Nothing of the code is kept: the window reads it again each time
/// the vault publishes a new one.
view account_card(
    v: Vault,
    a: Account,
    second: Int,
    shown: State<Text>,
    shown_until: State<Int>,
    screen: State<Text>,
    editing: State<Option<Account>>,
    issuer: State<Text>,
    label: State<Text>,
    secret: State<Text>,
    algorithm: State<Text>,
    digits: State<Int>,
    period: State<Int>,
    advanced: State<Bool>,
) {
    let made = vault_code(a.item)
    let unveiled = shown == a.id and second < shown_until
    ui.CardGridCard(item: a, title: if a.issuer == "" { "Account" } else { a.issuer }) {
        on activate: set(shown, a.id).then(set(shown_until, second + 10))
        body: {
            row(gap: .related, align: .center) {
                ui.Avatar(name: a.issuer, colour: account_colour(a.color))
                if a.label != "" {
                    ui.Text(a.label, role: .caption, secondary: true, truncate: true)
                }
            }
            row(gap: .section, align: .center) {
                if let generated = made {
                    if unveiled {
                        ui.Text(group_code(generated.code), role: .display, numeric: true)
                    } else {
                        ui.Text("••• •••", role: .display, numeric: true)
                    }
                    ui.Countdown(
                        at: generated.until,
                        total: generated.period.to_float(),
                        label: "Code lifetime",
                        size: .small,
                    )
                } else {
                    ui.Text("—", role: .display, numeric: true)
                }
            }
            row(gap: .related, align: .center) {
                if unveiled {
                    ui.Button(
                        "Copy",
                        icon: .copy,
                        role: .primary,
                        size: .small,
                        accessible_name: "Copy code for {a.issuer}",
                    ) {
                        on press: copy_code(a)
                    }
                } else {
                    ui.Button(
                        "Reveal",
                        icon: .eye,
                        role: .primary,
                        size: .small,
                        accessible_name: "Reveal code for {a.issuer}",
                    ) {
                        on press: set(shown, a.id).then(set(shown_until, second + 10))
                    }
                }
                ui.Button("Edit", icon: .pencil, size: .small) {
                    on press: set(editing, .some(a))
                        .then(set(issuer, a.issuer))
                        .then(set(label, a.label))
                        .then(set(secret, ""))
                        .then(set(algorithm, made?.algorithm ?? "SHA1"))
                        .then(set(digits, made?.digits ?? 6))
                        .then(set(period, made?.period ?? 30))
                        .then(set(advanced, false))
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
}

/// The colour an account's avatar wears, by the slot it was given: the same eight hues for
/// every account, so the list reads as one set of marks rather than a rainbow of hashes.
fn account_colour(slot: Int) -> Text {
    if slot == 1 {
        "#7a5cc4"
    } else if slot == 2 {
        "#b0568f"
    } else if slot == 3 {
        "#bf5b45"
    } else if slot == 4 {
        "#a8792a"
    } else if slot == 5 {
        "#4f8f4a"
    } else if slot == 6 { "#2f8f88" } else if slot == 7 { "#6b7280" } else { "#3f7fbf" }
}

default view account_window for Account

view account_window(a: Account) {
    protect capture
    // The code this window draws is left out of a capture, as the list's are.
    state second: Int = 0
    state shown_until: Int = 0
    let made = vault_code(a.item)
    let unveiled = second < shown_until
    stack(gap: .related, padding: .group, max: 40rem) {
        every 1s: set(second, second + 1)
        clock_warning()
        row(gap: .related, align: .center) {
            ui.Avatar(name: a.issuer, colour: account_colour(a.color))
            stack(gap: .tight) {
                ui.Text(if a.issuer == "" { "Account" } else { a.issuer }, role: .title)
                if a.label != "" {
                    ui.Text(a.label, role: .caption, secondary: true)
                }
            }
        }
        row(gap: .section, align: .center) {
            if let generated = made {
                if unveiled {
                    ui.Text(group_code(generated.code), role: .display, numeric: true)
                } else {
                    ui.Text("••• •••", role: .display, numeric: true)
                }
                ui.Countdown(
                    at: generated.until,
                    total: generated.period.to_float(),
                    label: "Code lifetime",
                    size: .small,
                )
            } else {
                ui.Text("—", role: .display, numeric: true)
            }
        }
        row(gap: .related, align: .center) {
            if unveiled {
                ui.Button(
                    "Copy",
                    icon: .copy,
                    role: .primary,
                    accessible_name: "Copy code for {a.issuer}",
                ) {
                    on press: copy_code(a)
                }
            } else {
                ui.Button(
                    "Reveal",
                    icon: .eye,
                    role: .primary,
                    accessible_name: "Reveal code for {a.issuer}",
                ) {
                    on press: set(shown_until, second + 10)
                }
            }
        }
    }
}

/// Says so when the device's clock is not synchronised. A code made from a seed and a clock is
/// only as good as the clock: a service refuses one that is out by more than a step, and a
/// device that cannot say whether its clock is kept says nothing.
view clock_warning() {
    if let kept = device.clock.synchronised {
        if not kept {
            ui.Banner(
                "This device's clock is not synchronised, so a service may reject these codes. Turn on automatic time, then try again.",
                kind: .warning,
                icon: .triangle_alert,
            )
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
    advanced: State<Bool>,
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
                on submit(uri): set(link, uri)
            }
            ui.Button("Fill from the link", icon: .link) {
                on press: if parse_uri(link) == .none {
                    refuse(v, "That is not a setup link this app reads")
                } else {
                    set(issuer, (parse_uri(link) ?? ParsedAnswer()).issuer)
                        .then(
                            set(label, (parse_uri(link) ?? ParsedAnswer()).label),
                        )
                        .then(
                            set(secret, (parse_uri(link) ?? ParsedAnswer()).secret),
                        )
                        .then(
                            set(algorithm, (parse_uri(link) ?? ParsedAnswer()).algorithm),
                        )
                        .then(
                            set(digits, (parse_uri(link) ?? ParsedAnswer()).digits),
                        )
                        .then(
                            set(period, (parse_uri(link) ?? ParsedAnswer()).period),
                        )
                        .then(set(link, ""))
                        .then(set(advanced, false))
                }
            }
        }
        ui.SectionDivider()
        ui.TextInput(bind: issuer, placeholder: "Service name", title: true)
        ui.TextInput(bind: label, placeholder: "Email or username")
        ui.TextInput(
            bind: secret,
            placeholder: if editing == .none {
                "Key from the service"
            } else {
                "New key, or leave empty to keep the one the vault holds"
            },
            secret: true,
        )
        // A number out of range is under these options: they stay open while it is, so the
        // field that refused the save is the one in front of the person.
        ui.DisclosureRow(
            title: "Advanced options",
            expanded: advanced or period < 5 or period > 300 or digits < 4 or digits > 10,
        ) {
            on toggle: set(advanced, not advanced)
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
