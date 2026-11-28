/// The local login accounts this computer keeps: each one's name, whether it administers the
/// device, and whether it signs in by itself. Creating, changing and removing an account calls
/// the users resource, so every window and every agent changes the same accounts.
view users_pane(s: Settings) {
    state full_name: Text = ""
    state username: Text = ""
    state password: Text = ""
    state removing: Text = ""
    let users = device.users
    ui.Section("Login accounts") {
        if let error = users.read_error {
            ui.StatusLine(error, status: .error)
        } else if users.accounts.is_empty() {
            ui.EmptyState("No login account", icon: .user, inline: true)
        }
        for account in users.accounts {
            ui.ListRow(
                account.full_name,
                secondary: account_words(account),
                selected: account.current,
                icon: .user,
            ) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        ui.Button(
                            if account.admin { "Administrator" } else { "Standard" },
                            size: .small,
                            toggled: account.admin,
                        ) {
                            on press: device.set_admin(
                                device.users,
                                user: account.username,
                                admin: not account.admin,
                            )
                        }
                        ui.Button(
                            if account.automatic_login {
                                "Signs in by itself"
                            } else { "Asks for a password" },
                            size: .small,
                            toggled: account.automatic_login,
                        ) {
                            on press: device.set_automatic_login(
                                device.users,
                                user: account.username,
                                on: not account.automatic_login,
                            )
                        }
                        if not account.current {
                            ui.Button(
                                "Remove this account",
                                icon: .trash,
                                icon_only: true,
                                role: .destructive,
                                size: .small,
                            ) {
                                on press: set(removing, account.username)
                            }
                        }
                    }
                }
            }
            ui.ModalDialog(
                title: "Remove {account.full_name}?",
                secondary: "Cancel",
                primary: "Remove, keeping the files",
                destructive: true,
                open: removing == account.username,
            ) {
                on act_primary: device
                    .remove(
                        device.users,
                        user: account.username,
                        keep_files: true,
                    )
                    .then(set(removing, ""))
                on act_secondary: set(removing, "")
                on dismiss: set(removing, "")
                ui.Text(
                    "The home folder stays. Sign this account out before removing it.",
                    role: .caption,
                )
                ui.Button("Remove the account and its files", role: .destructive) {
                    on press: device
                        .remove(
                            device.users,
                            user: account.username,
                            keep_files: false,
                        )
                        .then(set(removing, ""))
                }
            }
        }
    }
    ui.Section("Add an account") {
        ui.TextInput(bind: full_name, placeholder: "The person's full name")
        ui.TextInput(bind: username, placeholder: "The login name, lowercase")
        ui.TextInput(bind: password, placeholder: "The password")
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Create the account", icon: .plus, role: .primary) {
                on press: device.add(
                    device.users,
                    username: username,
                    full_name: full_name,
                    password: password,
                )
            }
        }
    }
    ui.Section("Your Tilo identities") {
        if s.identities_problem != "" {
            ui.StatusLine(s.identities_problem, status: .error)
        }
        let known = known_identities(s)
        if known.is_empty() and s.identities_problem == "" {
            ui.Text(
                "No signing identity yet. You set one up in your own session.",
                role: .caption,
                secondary: true,
            )
        }
        for identity in known {
            ui.ListRow(
                identity.display_name,
                secondary: identity_words(identity),
                selected: identity.active,
                icon: .key,
            ) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        if identity.active {
                            ui.Chip("Active", tone: .success)
                        } else {
                            ui.Button("Use", size: .small) {
                                on press: change_identity("useIdentity", identity.id)
                            }
                        }
                        if identity.protection == "LegacyPlaintext" {
                            ui.Button("Protect now", size: .small, role: .primary) {
                                on press: change_identity("protectIdentity", identity.id)
                            }
                        }
                        ui.Button(
                            "Remove identity",
                            icon: .trash,
                            icon_only: true,
                            role: .destructive,
                            size: .small,
                        ) {
                            on press: change_identity("removeIdentity", identity.id)
                        }
                    }
                }
            }
        }
    }
    ui.Section("You") {
        let me = device.people.me
        row(gap: .related, align: .center, wrap: true) {
            ui.Avatar(name: me.name, picture: me.photo)
            ui.Text(me.name, role: .title)
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button(
                if me.photo != "" { "Change your picture…" } else { "Choose a picture…" },
                icon: .image,
            ) {
                on press: file.choose(
                    set_own_picture(s),
                    accept: ["image/png", "image/jpeg", "image/webp"],
                )
            }
            if me.photo != "" {
                ui.Button("Remove the picture", icon: .trash, role: .destructive, size: .small) {
                    on press: device.set_photo(device.people, picture: "")
                }
            }
        }
        ui.Text(
            "Your picture is kept with your identity, so every device and every app that shows you draws it.",
            role: .caption,
            secondary: true,
        )
    }
    if let problem = users.problem {
        ui.StatusLine(problem, status: .error)
    }
    if not users.serving {
        ui.Text(
            "This computer's account service is not running; accounts cannot be changed here.",
            role: .caption,
            secondary: true,
        )
    }
}

/// What an account is, in a line: its login name, its role and whether it signs in by itself.
fn account_words(account: device.LocalAccount) -> Text {
    let role = if account.admin { "administrator" } else { "standard" }
    let start = if account.automatic_login { "signs in by itself" } else { "asks for a password" }
    if account.current {
        "{account.username} - {role}, this session"
    } else if let error = account.automatic_login_error {
        "{account.username} - {role}; {error}"
    } else { "{account.username} - {role}, {start}" }
}

/// Give this person the picture `file` holds. The session keeps it in their identity record,
/// where every device and every app that shows them reads it.
action set_own_picture(s: Settings, file: File) (
    icon: .image,
    label: "Choose a picture…",
    did: "changed the picture",
    opens: ["image/png", "image/jpeg", "image/webp"],
) {
    device.set_photo(device.people, picture: file.url)
}

/// One signing identity this device keeps, as the daemon lists it: never its key.
type TiloIdentity {
    id: Text
    display_name: Text
    threshold: Int
    total_shares: Int
    active: Bool
    protection: Text
}

/// The identities the daemon last listed, read from the answer the pane keeps.
fn known_identities(s: Settings) -> List<TiloIdentity> {
    if s.identities == "" {
        return []
    }
    match json.decode<List<TiloIdentity>>(s.identities) {
        .ok(list) => list,
        .err(why) => [],
    }
}

/// Who an identity is and how its key is kept, in a line: the shares that sign, and what holds
/// the key.
fn identity_words(identity: TiloIdentity) -> Text {
    let shares = if identity.threshold == identity.total_shares {
        "{identity.threshold} of {identity.total_shares} shares"
    } else {
        "{identity.threshold} of {identity.total_shares} shares sign"
    }
    "{shares}; {identity_protection_words(identity.protection)}"
}

/// How an identity's key is protected, in words a person reads.
fn identity_protection_words(protection: Text) -> Text {
    if protection == "SecretService" {
        "protected by your keyring"
    } else if protection == "BrokerFile" {
        "stored in a private file"
    } else if protection == "HardwareBacked" {
        "protected by hardware"
    } else if protection == "LegacyPlaintext" {
        "some keys sit in a private file that is not encrypted; unlock your keyring to protect them"
    } else if protection == "DeviceCertificate" {
        "signs with this device's certificate"
    } else { "protection not available" }
}

/// Asks the device's identity store which identities it keeps, and keeps its answer for the pane.
/// The daemon holds the store: no window reads a key or signs with one.
action read_identities(s: Settings) (icon: .refresh, label: "Read your identities") {
    if let empty = empty_ask() {
        let answer = actions.call(address: "tilo.device/devices#identities", arguments: empty)
        if answer.status == .ended {
            let listed = if let out = answer.output { json.encode(out) } else { "" }
            if s.identities != listed {
                s.identities = listed
            }
            if s.identities_problem != "" {
                s.identities_problem = ""
            }
        } else if let why = answer.failure {
            if s.identities_problem != why {
                s.identities_problem = why
            }
        } else if s.identities_problem != "Your identities can't be changed in this session." {
            s.identities_problem = "Your identities can't be changed in this session."
        }
    }
}

/// Asks the device's identity store to take `step` for identity `id`: use it for new apps,
/// protect its key, or remove it. The store confirms the change with the person.
action change_identity(step: Text, id: Text) (icon: .key, label: "Change an identity") {
    if let arguments = ask_identity(id) {
        actions.call(address: "tilo.device/devices#{step}", arguments: arguments)
    }
}

/// What an identity step is called with: the identity, and the person's confirmation.
fn ask_identity(id: Text) -> Option<json.Value> {
    let text = "\{\"id\":" + json.encode(id) + ",\"confirm\":true\}"
    match json.parse(text) {
        .ok(value) => .some(value),
        .err(why) => .none,
    }
}

/// The pane reads the identities while it is open, so a step taken here or on another device
/// shows as it lands.
trigger listing_identities(s: Settings) every 10s: read_identities(s)
