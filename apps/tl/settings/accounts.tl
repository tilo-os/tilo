/// The mail, calendar and cloud-storage accounts the workspace connected: each with the kind of
/// service it is, the address and provider it signs in at and how its last sync went, and where
/// one is added, edited, signed in again or removed. They are the workspace's own
/// (`tilo.accounts`), kept where the workspace runs with their secrets in its vault, so every
/// member reads the same accounts and no password is ever read back. A window follows the
/// record, so a change made on another device reaches it.
view accounts_pane() {
    state form: Bool = false
    state editing: Text = ""
    state choosing: Text = ""
    state name: Text = ""
    state address: Text = ""
    state username: Text = ""
    state incoming: Text = ""
    state outgoing: Text = ""
    state password: Text = ""
    state identities: Text = ""
    let a = accounts.accounts
    ui.Section("Connected accounts") {
        if a.accounts.is_empty() {
            ui.Text(
                "You have no accounts yet. Add one below to bring this workspace its mail, calendars and files.",
                role: .caption,
                secondary: true,
            )
        }
        for account in a.accounts {
            ui.ListRow(
                account.name,
                secondary: if account.address == account.name or account.address == "" {
                    if account.kind == .mail {
                        "Mail"
                    } else if account.kind == .calendar {
                        "Calendar"
                    } else if account.kind == .cloud { "Cloud storage" } else { "Account" }
                } else if account.kind == .mail {
                    "{account.address} · Mail"
                } else if account.kind == .calendar {
                    "{account.address} · Calendar"
                } else if account.kind == .cloud {
                    "{account.address} · Cloud storage"
                } else { "{account.address} · Account" },
                icon: if account.kind == .mail {
                    .mail
                } else if account.kind == .calendar { .calendar } else { .cloud },
            ) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        ui.Chip(
                            if account.status == .connected {
                                "Up to date"
                            } else if account.status == .needs-reconnect {
                                if account.words == "" {
                                    "Needs signing in again"
                                } else {
                                    "Needs signing in again: {account.words}"
                                }
                            } else if account.status == .error {
                                if account.words == "" {
                                    "Could not sync"
                                } else { "Could not sync: {account.words}" }
                            } else if account.status == .waiting {
                                "Waiting for a connection"
                            } else if account.status == .syncing {
                                "Syncing"
                            } else { "Not synced yet" },
                            tone: if account.status == .error or account.status == .needs-reconnect {
                                .warning
                            } else { .neutral },
                        )
                        ui.MenuButton(
                            "More for {account.name}",
                            icon: .more_horizontal,
                            icon_only: true,
                        ) {
                            if account.kind == .mail {
                                ui.MenuItem("Edit") {
                                    on choose: set(editing, account.id)
                                        .then(
                                            set(name, account.name),
                                        )
                                        .then(
                                            set(address, account.address),
                                        )
                                        .then(
                                            set(username, account.username),
                                        )
                                        .then(
                                            set(incoming, account.incoming),
                                        )
                                        .then(
                                            set(outgoing, account.outgoing),
                                        )
                                        .then(
                                            set(password, ""),
                                        )
                                        .then(
                                            set(identities, account.identities.join(", ")),
                                        )
                                        .then(
                                            set(form, true),
                                        )
                                }
                                ui.MenuItem("Sign in again") {
                                    on choose: accounts.reconnect(
                                        accounts.accounts,
                                        account: account.id,
                                    )
                                }
                                ui.MenuItem("Check the server") {
                                    on choose: accounts.check(
                                        accounts.accounts,
                                        account: account.id,
                                    )
                                }
                            }
                            if account.kind == .calendar {
                                ui.MenuItem("Choose calendars") {
                                    on choose: accounts
                                        .discover_calendars(
                                            accounts.accounts,
                                            account: account.id,
                                        )
                                        .then(
                                            set(choosing, account.id),
                                        )
                                }
                            }
                            ui.MenuItem("Remove {account.name}") {
                                on choose: accounts.remove(accounts.accounts, account: account.id)
                            }
                        }
                    }
                }
            }
        }
    }
    for account in a.accounts {
        if choosing == account.id and account.kind == .calendar {
            ui.Section("Calendars of {account.name}") {
                ui.Text(
                    "The calendars that show in Calendar. The others keep their events out of it.",
                    role: .caption,
                    secondary: true,
                )
                if account.calendars.is_empty() {
                    ui.Text(
                        "Its server holds no calendars this workspace can show.",
                        role: .caption,
                        secondary: true,
                    )
                }
                for calendar in account.calendars {
                    ui.ListRow(
                        calendar.name,
                        secondary: if calendar.visible {
                            "Shown in Calendar"
                        } else { "Hidden from Calendar" },
                        icon: if calendar.visible { .check } else { .x },
                    ) {
                        trailing: {
                            ui.Button(
                                if calendar.visible { "Hide" } else { "Show" },
                                size: .small,
                            ) {
                                on press: accounts.set_calendars(
                                    accounts.accounts,
                                    account: account.id,
                                    calendars: account.calendars
                                        .filter(
                                            c -> if c.id == calendar.id {
                                                not calendar.visible
                                            } else { c.visible },
                                        )
                                        .map(c -> c.id),
                                )
                            }
                        }
                    }
                }
                ui.Button("Done") {
                    on press: set(choosing, "")
                }
            }
        }
    }
    ui.Section("Add an account") {
        if form {
            ui.Text(
                if editing == "" {
                    "An account's password is sealed in this workspace's vault. Every member reads the account, and nobody reads its password back."
                } else {
                    "Leave the password empty to keep the one the workspace's vault holds. A changed incoming server needs the password for that server."
                },
                role: .caption,
                secondary: true,
            )
            row(gap: .related, align: .center, wrap: true) {
                ui.TextInput(bind: name, placeholder: "The name shown for it, such as Work", fill)
                ui.TextInput(
                    bind: address,
                    placeholder: "The address, such as you@example.com",
                    read_only: editing != "",
                    fill,
                )
                ui.TextInput(
                    bind: username,
                    placeholder: "The sign-in name, when it is not the address",
                    fill,
                )
                ui.TextInput(
                    bind: password,
                    placeholder: "Its password or app password",
                    secret: true,
                    fill,
                )
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.TextInput(
                    bind: incoming,
                    placeholder: "The incoming server, such as imap.example.org:993",
                    fill,
                )
                ui.TextInput(
                    bind: outgoing,
                    placeholder: "The outgoing server, such as smtp.example.org:465",
                    fill,
                )
                ui.TextInput(
                    bind: identities,
                    placeholder: "Other names it sends as, separated by commas",
                    fill,
                )
            }
            ui.Text(
                "Servers left empty are worked out from the address. A server's port after a colon is used as written.",
                role: .caption,
                secondary: true,
            )
            row(gap: .related, align: .center, wrap: true) {
                ui.Button(
                    if editing == "" { "Add the account" } else { "Save the account" },
                    role: .primary,
                ) {
                    on press: accounts
                        .add_mail(
                            accounts.accounts,
                            address: address,
                            displayName: name,
                            username: if username == "" { .none } else { .some(username) },
                            incomingHost: incoming,
                            outgoingHost: outgoing,
                            password: password,
                            identities: identities.split(",").filter(i -> i != ""),
                        )
                        .then(set(form, false))
                        .then(set(editing, ""))
                        .then(set(name, ""))
                        .then(set(address, ""))
                        .then(set(username, ""))
                        .then(set(incoming, ""))
                        .then(set(outgoing, ""))
                        .then(set(password, ""))
                        .then(set(identities, ""))
                }
                ui.Button("Cancel") {
                    on press: set(form, false).then(set(editing, "")).then(set(password, ""))
                }
            }
        } else {
            ui.Text(
                "Mail, calendars and cloud storage are connected here, once, for the whole workspace.",
                role: .caption,
                secondary: true,
            )
            ui.MenuButton("Add an account", icon: .plus) {
                ui.MenuItem("Email") {
                    on choose: set(editing, "")
                        .then(set(name, ""))
                        .then(set(address, ""))
                        .then(set(username, ""))
                        .then(set(incoming, ""))
                        .then(set(outgoing, ""))
                        .then(set(password, ""))
                        .then(set(identities, ""))
                        .then(set(form, true))
                }
            }
        }
    }
}
