app so.tilo.totp "Authenticator" (
    icon: .shield_check,
    category: .utility,
    tagline: "Makes the one-time codes a service asks for",
    keywords: ["authenticator", "authenticator app", "two-factor", "two factor", "2fa", "mfa", "totp", "otp", "one-time code", "one-time password", "login code", "verification code", "security key", "authentifizierung", "authenticator-app", "zwei-faktor", "einmalcode", "einmalpasswort", "anmeldecode", "sicherheitsschlüssel"],
    splash_budget: 250,
)

use so.tilo.vault as vault
use so.tilo.device as device

op "check"
op "parse"
op "scan"

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
    /// When its codes were last made differently: the moment the last recode ran. The settings
    /// themselves are the vault item's, so this is what a change of them writes here.
    tuned_at: Time = Time("1970-01-01T00:00:00Z")

    search {
        sort issuer a_z
    }
}

/// The workspace's Authenticator: the accounts its members keep, what the window last said, and
/// the account a picture read, waiting for a person to save or drop it.
type Vault {
    accounts: Collection<Account>
    notice: Text = ""
    notice_kind: Text = "info"
    /// The vault item a picture's key was kept in, until the person saves the account or drops
    /// it. No key is here: only the item that holds it.
    pending: Text = ""
    /// What the picture said the account is, for the form to show and a person to correct.
    pending_issuer: Text = ""
    pending_label: Text = ""
    pending_algorithm: Text = "SHA1"
    pending_digits: Int = 6
    pending_period: Int = 30
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

/// Reads the account a picture's QR code names, and keeps its key in the vault for the form to
/// show: nothing is added until the person reads what the picture said and saves it.
action scan_picture(v: Vault, file: File) (icon: .qr_code, label: "Read a picture") {
    // A review left over from an earlier picture gives way to this one; nothing pending means
    // nothing to drop, and no change to say so.
    if v.pending != "" { drop_pending(v) }
    v.notice = ""
    match engine.call("scan", json.encode(ScanAsk(address: file.url))) {
        .ok(answer) => match json.decode<ScanAnswer>(answer) {
            .ok(scan) => match parse_uri(scan.uri) {
                .some(setup) => {
                    if setup.secret == "" {
                        refuse(v, "Couldn't read a code from that image")
                    } else {
                        v.pending = vault.keep(
                            item: .none,
                            issuer: setup.issuer,
                            label: setup.label,
                            secret: setup.secret,
                            algorithm: setup.algorithm,
                            digits: setup.digits,
                            period: setup.period,
                        )
                        v.pending_issuer = setup.issuer
                        v.pending_label = setup.label
                        v.pending_algorithm = setup.algorithm
                        v.pending_digits = setup.digits
                        v.pending_period = setup.period
                        notice("QR code read", kind: .success)
                    }
                }
                .none => refuse(v, "Couldn't read a code from that image"),
            },
            .err(why) => refuse(v, why),
        },
        .err(why) => refuse(v, why),
    }
}

/// Adds the account the picture read, with the key the vault already holds for it.
action add_pending(v: Vault) -> Account (
    icon: .plus,
    label: "Add account",
    did: "added an account",
) {
    require v.pending != "" "There is no account read from a picture to add"
    let made = Account(issuer: v.pending_issuer, label: v.pending_label, item: v.pending, color: 0)
    v.accounts.insert(made)
    v.pending = ""
    v.pending_issuer = ""
    v.pending_label = ""
    made
}

/// Lets the picture's account go: the key the vault kept for it is forgotten too.
action drop_pending(v: Vault) (
    icon: .trash,
    label: "Drop the picture's account",
    did: "dropped a picture's account",
) {
    if v.pending != "" { vault.forget(item: v.pending) }
    v.pending = ""
    v.pending_issuer = ""
    v.pending_label = ""
    v.pending_algorithm = "SHA1"
    v.pending_digits = 6
    v.pending_period = 30
}

/// Renames an account's service: what a person changing the name it shows did. The codes'
/// settings come from the form, which read them when it opened: an action never reads the
/// vault, so what it keeps is what the window showed the person.
action rename_account(
    a: Account,
    issuer: Text,
    algorithm: Text,
    digits: Int,
    period: Int,
) uses secret (icon: .pencil, label: "Rename", did: "renamed an account") {
    require issuer.trim() != "" "The account needs a service name"
    let reference = vault.keep(
        item: if a.item == "" { .none } else { .some(a.item) },
        issuer: issuer.trim(),
        label: a.label,
        secret: "",
        algorithm: algorithm,
        digits: digits,
        period: period,
    )
    a.issuer = issuer.trim()
    a.item = reference
}

/// Changes who an account signs in as: what a person changing its name did. Its codes' settings
/// come from the form, as a rename's do.
action relabel_account(
    a: Account,
    label: Text,
    algorithm: Text,
    digits: Int,
    period: Int,
) uses secret (icon: .pencil, label: "Change the account name", did: "changed the account's name") {
    let reference = vault.keep(
        item: if a.item == "" { .none } else { .some(a.item) },
        issuer: a.issuer,
        label: label.trim(),
        secret: "",
        algorithm: algorithm,
        digits: digits,
        period: period,
    )
    a.label = label.trim()
    a.item = reference
}

/// Gives an account a colour of its own: what a person recolouring it did.
action recolor_account(a: Account, color: Int) (
    icon: .pencil,
    label: "Recolour",
    did: "recoloured an account",
) {
    a.color = color
}

/// One account as a script or an agent reads it: the service it is for, who it signs in as, and
/// the vault item that holds its key. No key and no code is part of it.
type AccountEntry (title: issuer) {
    issuer: Text
    label: Text
    item: Text
}

/// The accounts the workspace keeps, in the order the window lists them: what a script, an agent
/// or another app asks for. The keys stay in the vault, and no code is part of the answer.
action accounts(v: Vault) -> List<AccountEntry> (icon: .list, label: "List accounts") {
    v.accounts.map(a -> AccountEntry(issuer: a.issuer, label: a.label, item: a.item))
}

/// Changes the key an account makes its codes from, and the algorithm they are made with: what a
/// person typing a new key or changing the algorithm did.
action recode_account(
    a: Account,
    algorithm: Text,
    digits: Int,
    period: Int,
    secret: Text = "",
) uses secret, time (
    icon: .key,
    label: "Change how the codes are made",
    did: "changed how an account makes its codes",
) {
    require secret.trim() == "" or valid_secret(secret) "That key is not base32; copy it from the service again"
    let reference = vault.keep(
        item: if a.item == "" { .none } else { .some(a.item) },
        issuer: a.issuer,
        label: a.label,
        secret: secret.trim(),
        algorithm: algorithm,
        digits: digits,
        period: period,
    )
    a.item = reference
    // The settings live in the vault item; the account records when they last changed, so the
    // change is the account's and its history names the action that made it.
    a.tuned_at = now
}

/// Changes how many digits an account's codes have: history says which account shows how many.
action set_digits(a: Account, digits: Int, algorithm: Text, period: Int) uses secret, time (
    icon: .key,
    label: "Change the digits",
    did: "changed how many digits an account's codes have",
) {
    require digits >= 4 and digits <= 10 "A code has 4 to 10 digits"
    let reference = vault.keep(
        item: if a.item == "" { .none } else { .some(a.item) },
        issuer: a.issuer,
        label: a.label,
        secret: "",
        algorithm: algorithm,
        digits: digits,
        period: period,
    )
    a.item = reference
    a.tuned_at = now
}

/// Changes how long an account's codes last: history says which account changes its code every
/// how many seconds.
action set_period(a: Account, period: Int, algorithm: Text, digits: Int) uses secret, time (
    icon: .key,
    label: "Change how long a code lasts",
    did: "changed how long an account's code lasts",
) {
    require period >= 5 and period <= 300 "A code changes every 5 to 300 seconds"
    let reference = vault.keep(
        item: if a.item == "" { .none } else { .some(a.item) },
        issuer: a.issuer,
        label: a.label,
        secret: "",
        algorithm: algorithm,
        digits: digits,
        period: period,
    )
    a.item = reference
    a.tuned_at = now
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
) uses time (icon: .pencil, label: "Save the account", did: "changed an account") {
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
    state color: Int = 0
    // What the codes' settings were when the form opened: a save compares them with what the
    // form holds now to tell a change of how the codes are made from a change of anything else.
    state was_algorithm: Text = "SHA1"
    state was_digits: Int = 6
    state was_period: Int = 30
    state link: Text = ""
    ui.NavShell(content_kind: .list, title: window_title(v), count: v.accounts.len()) {
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
                            .then(set(color, 0))
                            .then(set(was_algorithm, "SHA1"))
                            .then(set(was_digits, 6))
                            .then(set(was_period, 30))
                            .then(set(advanced, false))
                            .then(set(screen, "form"))
                    }
                } else {
                    ui.Button("Back", icon: .chevron_left, role: .plain) {
                        on press: set(screen, "list")
                    }
                    ui.Button(
                        "Scan a QR image",
                        icon: .qr_code,
                        description: "Read the code from a screenshot or photo",
                    ) {
                        // The picture is read into the form: the person sees what it said and
                        // saves it, so a picture that names the wrong service can be corrected.
                        on press: file.choose(scan_picture(v), accept: ["image/*"])
                    }
                    ui.Button("Save", icon: .check, role: .primary) {
                        // The list is where a person goes once the account is saved, and nowhere
                        // else: a form that keeps what was typed open is what a refusal leaves.
                        // A number out of range is what sends them to the options it is under.
                        on press: {
                            // A picture that was read is what Save saves: the person corrected
                            // its words above, and the vault already holds its key.
                            let saved = save_account(
                                v,
                                editing,
                                issuer,
                                label,
                                secret,
                                algorithm,
                                digits,
                                period,
                                color,
                                was_algorithm,
                                was_digits,
                                was_period,
                            )
                            if saved {
                                set(screen, "list").then(set(advanced, false))
                            } else {
                                // Nothing moves: the form stays as it was, with its refusal
                                // said in the bar. An action either way, so the press is one.
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
                    color,
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
                        .then(set(was_algorithm, "SHA1"))
                        .then(set(was_digits, 6))
                        .then(set(was_period, 30))
                        .then(set(advanced, false))
                        .then(set(screen, "form"))
                }
            } else {
                stack(gap: .related) {
                    // The window's own second, drawn while it lists accounts: the vault answers
                    // each code again, so the digits and their countdown move. Nothing is kept.
                    every 1s: set(second, second + 1)
                    clock_warning()
                    text(
                        "Codes are hidden until you ask to see one; they stay readable for ten seconds.",
                        role: .caption,
                        secondary: true,
                    )
                    ui.CardGrid(name: "Accounts", card_width: .regular) {
                        // The selection the arrow keys move is the grid's, and the keys a card
                        // answers are its own: Enter reveals, F2 edits, Delete deletes.
                        on edit(a: Account): set(editing, .some(a))
                            .then(set(issuer, a.issuer))
                            .then(set(label, a.label))
                            .then(set(secret, ""))
                            .then(set(algorithm, algorithm_of(a)))
                            .then(set(digits, digits_of(a)))
                            .then(set(period, period_of(a)))
                            .then(set(was_algorithm, algorithm_of(a)))
                            .then(set(was_digits, digits_of(a)))
                            .then(set(was_period, period_of(a)))
                            .then(set(color, a.color))
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
                                color,
                                advanced,
                                was_algorithm,
                                was_digits,
                                was_period,
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
action save_account(
    v: Vault,
    editing: Option<Account>,
    issuer: Text,
    label: Text,
    secret: Text,
    algorithm: Text,
    digits: Int,
    period: Int,
    color: Int = 0,
    was_algorithm: Text = "SHA1",
    was_digits: Int = 6,
    was_period: Int = 30,
) -> Bool uses secret {
    // A picture that was read is what this saves: the person corrected its words in the form,
    // and the vault already holds its key.
    if v.pending != "" {
        add_pending(v)
        return true
    }
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
        // Only what the person changed is an action, so history says which part of the account
        // it was: a rename is a rename, a new key is the key, and how many digits or how long a
        // code lasts says so in its own sentence. The codes' previous settings come from the
        // form, which read them when it opened: an action never reads the vault.
        // The key and the algorithm are the same thing to a person: how the codes are made.
        let recoded = secret.trim() != "" or was_algorithm != algorithm
        if recoded {
            recode_account(a, algorithm, digits, period, secret)
        }
        if was_digits != digits { set_digits(a, digits, algorithm, period) }
        if was_period != period { set_period(a, period, algorithm, digits) }
        if a.issuer != issuer.trim() {
            rename_account(a, issuer, algorithm, digits, period)
        }
        if a.label != label.trim() {
            relabel_account(a, label, algorithm, digits, period)
        }
        if a.color != color { recolor_account(a, color) }
    } else {
        add_account(v, issuer, label, secret, algorithm, digits, period, color)
    }
    v.notice = ""
    v.notice_kind = "info"
    true
}

/// Whether an account's key is on this device at all: an account another device set up makes no
/// code here, and the form takes its key.
fn unkeyed(editing: Option<Account>) -> Bool {
    if let a = editing {
        if let made = vault_code(a.item) { false } else { true }
    } else { false }
}

/// The algorithm an account's codes are made with, as the vault keeps it.
fn algorithm_of(a: Account) -> Text {
    match vault_code(a.item) {
        .some(made) => made.algorithm,
        .none => "SHA1",
    }
}

/// How many digits an account's codes have, as the vault keeps them.
fn digits_of(a: Account) -> Int {
    match vault_code(a.item) {
        .some(made) => made.digits,
        .none => 6,
    }
}

/// How long an account's code lasts, as the vault keeps it.
fn period_of(a: Account) -> Int {
    match vault_code(a.item) {
        .some(made) => made.period,
        .none => 30,
    }
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
    color: State<Int>,
    advanced: State<Bool>,
    was_algorithm: State<Text>,
    was_digits: State<Int>,
    was_period: State<Int>,
) {
    let made = vault_code(a.item)
    let unveiled = shown == a.id and second < shown_until
    ui.CardGridCard(item: a, title: if a.issuer == "" { "Account" } else { a.issuer }) {
        // Enter on the card that is already showing its code copies it; on any other it shows
        // the code, so the keyboard does the two things the card's one button does.
        on activate: if shown == a.id and second < shown_until {
            copy_code(a)
        } else {
            set(shown, a.id).then(set(shown_until, second + 10))
        }
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
                        ui.Text(
                            group_code(generated.code),
                            role: .display,
                            numeric: true,
                            secret: true,
                        )
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
                    ui.Text("Not set up on this device", role: .caption, secondary: true)
                }
            }
            row(gap: .related, align: .center) {
                if made == .none {
                    // The account is the workspace's; its key is on another device, or nowhere
                    // yet. The form takes it, with the options open where a key is typed.
                    ui.Button("Set up", icon: .key, role: .primary, size: .small) {
                        on press: set(editing, .some(a))
                            .then(set(issuer, a.issuer))
                            .then(set(label, a.label))
                            .then(set(secret, ""))
                            .then(set(algorithm, algorithm_of(a)))
                            .then(set(digits, digits_of(a)))
                            .then(set(period, period_of(a)))
                            .then(set(was_algorithm, algorithm_of(a)))
                            .then(set(was_digits, digits_of(a)))
                            .then(set(was_period, period_of(a)))
                            .then(set(color, a.color))
                            .then(set(advanced, true))
                            .then(set(screen, "form"))
                    }
                } else if unveiled {
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
                        .then(set(algorithm, algorithm_of(a)))
                        .then(set(digits, digits_of(a)))
                        .then(set(period, period_of(a)))
                        .then(set(was_algorithm, algorithm_of(a)))
                        .then(set(was_digits, digits_of(a)))
                        .then(set(was_period, period_of(a)))
                        .then(set(color, a.color))
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
                    ui.Text(group_code(generated.code), role: .display, numeric: true, secret: true)
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
    color: State<Int>,
    link: State<Text>,
    advanced: State<Bool>,
) {
    stack(gap: .section, padding: .group, max: 40rem) {
        ui.Text(
            if editing == .none {
                "Add account"
            } else if unkeyed(editing) { "Set up on this device" } else { "Edit account" },
            role: .title,
        )
        // A picture that was read fills this form: what it said stands here to be corrected,
        // and Save adds it. Nothing else is offered until it is saved or dropped.
        if v.pending != "" {
            ui.Section("Read from a picture") {
                ui.TextInput(bind: v.pending_issuer, placeholder: "Service name", title: true)
                ui.TextInput(bind: v.pending_label, placeholder: "Email or username")
                row(gap: .related, align: .center) {
                    ui.Text(
                        "The key it read is kept in the vault until you save it.",
                        role: .caption,
                        secondary: true,
                        fill: 1.0,
                    )
                    ui.Button("Drop", icon: .trash, role: .plain) {
                        on press: drop_pending(v)
                    }
                }
            }
        } else {
            ui.Text(
                "Scan a QR code, paste a setup link, or type the details the service gave.",
                role: .caption,
                secondary: true,
            )
            ui.Section("Read a code") {
                row(gap: .related, align: .center) {
                    ui.TextInput(bind: link, placeholder: "Setup link", entry: true, fill: 1.0) {
                        on submit(uri): set(link, uri)
                    }
                    ui.Button(
                        "Fill from the link",
                        icon: .link,
                        description: "Fills in the service, account and key",
                    ) {
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
            }
            ui.Section("Details") {
                ui.TextInput(bind: issuer, placeholder: "Service name", title: true)
                ui.TextInput(bind: label, placeholder: "Email or username")
                ui.TextInput(
                    bind: secret,
                    placeholder: if editing == .none or unkeyed(editing) {
                        "Key from the service"
                    } else {
                        "New key, or leave empty to keep the one the vault holds"
                    },
                    secret: true,
                )
                ui.ColorSwatchPicker(
                    swatches: [
                        "#3f7fbf",
                        "#7a5cc4",
                        "#b0568f",
                        "#bf5b45",
                        "#a8792a",
                        "#4f8f4a",
                        "#2f8f88",
                        "#6b7280",
                    ],
                    columns: 8,
                    selected: .some(color),
                    label: "Colour",
                ) {
                    on change(i): set(color, i)
                }
            }
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

/// The window's title: the accounts it shows, named. A title that said only how many there are
/// would leave a person with several windows unable to tell them apart.
fn window_title(v: Vault) -> Text {
    if v.accounts.is_empty() {
        "Authenticator"
    } else if v.accounts.len() == 1 {
        name_of(v.accounts.first() ?? Account())
    } else {
        "{name_of(v.accounts.first() ?? Account())} and {v.accounts.len() - 1} more"
    }
}

/// An account's name in the words a window says it in.
fn name_of(a: Account) -> Text {
    if a.issuer == "" { "Account" } else { a.issuer }
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

/// A few accounts, as a capture of the window shows them: their services, names and colours.
/// Their keys are the device's, and a capture is never the person's device, so no code is drawn
/// and each card says it is not set up here.
scenario "a few accounts" {
    let v = root Vault
    v.accounts.insert(Account(issuer: "Acme", label: "ada@acme.example", color: 0))
    v.accounts.insert(Account(issuer: "GitHub", label: "ada@example.com", color: 2))
    v.accounts.insert(Account(issuer: "Hetzner", label: "ops@rivera.example", color: 4))
}
