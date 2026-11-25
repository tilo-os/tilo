/// The local login accounts this computer keeps: each one's name, whether it administers the
/// device, and whether it signs in by itself. Creating, changing and removing an account calls
/// the users resource, so every window and every agent changes the same accounts.
view users_pane(s: Settings) {
    state full_name: Text = ""
    state username: Text = ""
    state password: Text = ""
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
                                on press: device.remove(
                                    device.users,
                                    user: account.username,
                                    keep_files: true,
                                )
                            }
                        }
                    }
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
