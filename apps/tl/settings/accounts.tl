/// The mail, calendar and cloud-storage accounts the workspace connected: each with the address
/// it signs in at, the kind of service it is and how its last sync went, and where one is
/// connected, signed in again or removed. They are the workspace's own (`tilo.accounts`), kept
/// where the workspace runs with their secrets in its vault, so every member reads the same
/// accounts and no password is ever read back. A window follows the record, so an account
/// connected, signed in again or removed on another device reaches it.
view accounts_pane() {
    state address: Text = ""
    state username: Text = ""
    state password: Text = ""
    let a = accounts.accounts
    ui.Section("Connected accounts") {
        if a.accounts.is_empty() {
            ui.Text(
                "No mail, calendar or cloud-storage account is connected in this workspace yet.",
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
                            if reconnect_offered(account) {
                                ui.MenuItem("Sign in again") {
                                    on choose: accounts.reconnect(
                                        accounts.accounts,
                                        account: account.id,
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
    ui.Section("Connect an account") {
        ui.Text(
            "An account's password is sealed in this workspace's vault. Every member reads the account, and nobody reads its password back.",
            role: .caption,
            secondary: true,
        )
        row(gap: .related, align: .center, wrap: true) {
            ui.TextInput(bind: address, placeholder: "The address, such as you@example.com", fill)
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
            ui.Button("Connect", role: .primary) {
                on press: accounts
                    .add_mail(
                        accounts.accounts,
                        address: address,
                        username: if username == "" { .none } else { .some(username) },
                        password: password,
                    )
                    .then(set(address, ""))
                    .then(set(username, ""))
                    .then(set(password, ""))
            }
        }
    }
}

/// Whether signing an account in again is something this pane offers: only a mail account's
/// sign-in is proved where the workspace runs, and only once its last one no longer works.
fn reconnect_offered(account: accounts.AccountView) -> Bool {
    if account.kind == .mail {
        account.status == .needs-reconnect or account.status == .error
    } else { false }
}
