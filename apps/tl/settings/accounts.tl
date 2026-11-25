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
    state cloud_kind: Text = ""
    state cloud_name: Text = ""
    state cloud_server: Text = ""
    state cloud_username: Text = ""
    state cloud_password: Text = ""
    state cloud_read_only: Bool = false
    state sign_in: Text = ""
    state sign_in_open: Text = ""
    state sign_in_prompt: Text = ""
    state sign_in_code: Text = ""
    state sign_in_item: Text = ""
    state sign_in_choices: List<accounts.MailSignIn> = []
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
                        if account.services.calendar { "Mail and Calendar" } else { "Mail" }
                    } else if account.kind == .calendar {
                        "Calendar"
                    } else if account.kind == .cloud { "Cloud storage" } else { "Account" }
                } else if account.kind == .mail {
                    if account.services.calendar {
                        "{account.address} · Mail and Calendar"
                    } else { "{account.address} · Mail" }
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
                            tone: if account.status == .error
                                or account.status == .needs-reconnect
                                or account.paused {
                                .warning
                            } else { .neutral },
                        )
                        if account.paused {
                            ui.Chip("Paused", tone: .warning)
                        }
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
                                if account.provider == "google" or account.provider == "microsoft" {
                                    ui.MenuItem(
                                        if account.provider == "google" {
                                            "Sign in again on Google's page"
                                        } else { "Sign in again on Microsoft's page" },
                                    ) {
                                        on choose: {
                                            let step = accounts.begin_sign_in(
                                                accounts.accounts,
                                                provider: account.provider,
                                                address: account.address,
                                                account: account.id,
                                                mail: true,
                                            )
                                            set(sign_in, step.id).then(set(sign_in_open, step.open))
                                        }
                                    }
                                } else {
                                    ui.MenuItem("Sign in again") {
                                        on choose: accounts.reconnect(
                                            accounts.accounts,
                                            account: account.id,
                                        )
                                    }
                                }
                                ui.MenuItem("Check the server") {
                                    on choose: accounts.check(
                                        accounts.accounts,
                                        account: account.id,
                                    )
                                }
                                if account.provider == "google" or account.provider == "icloud" {
                                    ui.MenuItem(
                                        "Use it for Calendar",
                                        checked: account.services.calendar,
                                    ) {
                                        on choose: accounts.set_services(
                                            accounts.accounts,
                                            account: account.id,
                                            mail: account.services.mail,
                                            calendar: not account.services.calendar,
                                            files: account.services.files,
                                        )
                                    }
                                }
                            }
                            if account.kind == .cloud {
                                ui.MenuItem(
                                    "Mount its files in Files",
                                    checked: account.services.files,
                                ) {
                                    on choose: accounts.set_services(
                                        accounts.accounts,
                                        account: account.id,
                                        mail: account.services.mail,
                                        calendar: account.services.calendar,
                                        files: not account.services.files,
                                    )
                                }
                                ui.MenuItem(
                                    "Keep its files in sync",
                                    checked: account.files_mode == "sync",
                                ) {
                                    on choose: accounts.set_files_mode(
                                        accounts.accounts,
                                        account: account.id,
                                        mode: "sync",
                                    )
                                }
                                ui.MenuItem(
                                    "Import its files once",
                                    checked: account.files_mode == "import",
                                ) {
                                    on choose: accounts
                                        .set_files_mode(
                                            accounts.accounts,
                                            account: account.id,
                                            mode: "import",
                                        )
                                        .then(
                                            engine.choose(
                                                source: import_source(account.provider),
                                                account: account.id,
                                                keys: ["path:"],
                                                only: true,
                                            ),
                                        )
                                        .then(
                                            engine.start(
                                                source: import_source(account.provider),
                                                account: account.id,
                                            ),
                                        )
                                }
                                ui.MenuItem("Sync now") {
                                    on choose: accounts.sync_now(
                                        accounts.accounts,
                                        account: account.id,
                                    )
                                }
                                if account.paused {
                                    ui.MenuItem("Resume sync") {
                                        on choose: accounts.resume_sync(
                                            accounts.accounts,
                                            account: account.id,
                                        )
                                    }
                                } else {
                                    ui.MenuItem("Pause sync") {
                                        on choose: accounts.pause_sync(
                                            accounts.accounts,
                                            account: account.id,
                                        )
                                    }
                                }
                                ui.MenuItem("Open its files in Files") {
                                    on choose: session.open_app(app: "so.tilo.file-explorer")
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
            ui.Button("Use a password from Passwords") {
                on press: {
                    let found = accounts.sign_ins(accounts.accounts)
                    set(sign_in_choices, found.sign_ins.filter(choice -> is_mail_login(choice)))
                }
            }
            if editing == "" and address != "" {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text(
                        "Or approve this workspace on your mail provider's own page, with no password typed here.",
                        role: .caption,
                        secondary: true,
                    )
                    ui.Button("Sign in with Google", size: .small) {
                        on press: {
                            let step = accounts.begin_sign_in(
                                accounts.accounts,
                                provider: "google",
                                address: address,
                                name: name,
                                mail: true,
                            )
                            set(sign_in, step.id)
                                .then(set(sign_in_open, step.open))
                                .then(
                                    set(sign_in_prompt, step.code),
                                )
                        }
                    }
                    ui.Button("Sign in with Microsoft", size: .small) {
                        on press: {
                            let step = accounts.begin_sign_in(
                                accounts.accounts,
                                provider: "microsoft",
                                address: address,
                                name: name,
                                mail: true,
                            )
                            set(sign_in, step.id)
                                .then(set(sign_in_open, step.open))
                                .then(
                                    set(sign_in_prompt, step.code),
                                )
                        }
                    }
                }
                if sign_in != "" and cloud_kind == "" {
                    if sign_in_open != "" {
                        ui.Link(sign_in_open, label: "Open the sign-in page")
                    }
                    ui.Text(
                        if sign_in_prompt == "" {
                            "Waiting for you to approve it in your browser. The account appears here once you do."
                        } else { sign_in_prompt },
                        role: .caption,
                        secondary: true,
                    )
                    ui.Button("Cancel the sign-in") {
                        on press: accounts
                            .cancel_sign_in(
                                accounts.accounts,
                                signIn: sign_in,
                            )
                            .then(set(sign_in, ""))
                            .then(set(sign_in_open, ""))
                            .then(set(sign_in_prompt, ""))
                    }
                }
            }
            for choice in sign_in_choices {
                ui.ListRow(
                    choice.title,
                    secondary: if choice.username == "" {
                        choice.places.join(", ")
                    } else {
                        "{choice.username} · {choice.places.join(", ")}"
                    },
                    icon: .key,
                ) {
                    trailing: {
                        ui.Button(
                            if sign_in_item == choice.id { "Chosen" } else { "Use it" },
                            size: .small,
                        ) {
                            on press: set(sign_in_item, choice.id)
                                .then(set(address, choice.title))
                                .then(
                                    set(username, choice.username),
                                )
                                .then(
                                    set(incoming, imap_place(choice.places)),
                                )
                                .then(
                                    set(outgoing, smtp_place(choice.places)),
                                )
                        }
                    }
                }
            }
            row(gap: .related, align: .center, wrap: true) {
                if sign_in_item != "" {
                    ui.Button("Add from Passwords", role: .primary) {
                        on press: accounts
                            .add_mail_with_item(
                                accounts.accounts,
                                address: address,
                                displayName: name,
                                username: if username == "" { .none } else { .some(username) },
                                incomingHost: incoming,
                                outgoingHost: outgoing,
                                password: sign_in_item,
                            )
                            .then(set(form, false))
                            .then(set(editing, ""))
                            .then(set(sign_in_item, ""))
                            .then(set(sign_in_choices, []))
                            .then(set(address, ""))
                            .then(set(username, ""))
                            .then(set(incoming, ""))
                            .then(set(outgoing, ""))
                            .then(set(password, ""))
                            .then(set(identities, ""))
                    }
                } else {
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
                }
                ui.Button("Cancel") {
                    on press: set(form, false)
                        .then(set(editing, ""))
                        .then(set(password, ""))
                        .then(set(sign_in_item, ""))
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
                ui.MenuItem("Calendar server (CalDAV)") {
                    on choose: set(cloud_kind, "caldav")
                        .then(set(cloud_name, ""))
                        .then(set(cloud_server, ""))
                        .then(set(cloud_username, ""))
                        .then(set(cloud_password, ""))
                        .then(set(cloud_read_only, false))
                        .then(set(sign_in, ""))
                        .then(set(sign_in_open, ""))
                        .then(set(sign_in_prompt, ""))
                        .then(set(sign_in_code, ""))
                }
                for storage in [
                    "nextcloud",
                    "google-drive",
                    "onedrive",
                    "dropbox",
                    "icloud-drive",
                    "webdav",
                ] {
                    ui.MenuItem(
                        if storage == "nextcloud" {
                            "Nextcloud"
                        } else if storage == "google-drive" {
                            "Google Drive"
                        } else if storage == "onedrive" {
                            "OneDrive"
                        } else if storage == "dropbox" {
                            "Dropbox"
                        } else if storage == "icloud-drive" { "iCloud Drive" } else { "WebDAV" },
                    ) {
                        on choose: set(cloud_kind, storage)
                            .then(set(cloud_name, ""))
                            .then(set(cloud_server, ""))
                            .then(set(cloud_username, ""))
                            .then(set(cloud_password, ""))
                            .then(set(sign_in, ""))
                            .then(set(sign_in_open, ""))
                            .then(set(sign_in_prompt, ""))
                            .then(set(sign_in_code, ""))
                    }
                }
            }
        }
    }
    if cloud_kind != "" {
        ui.Section(
            if cloud_kind == "nextcloud" {
                "Connect a Nextcloud server"
            } else if cloud_kind == "google-drive" {
                "Connect Google Drive"
            } else if cloud_kind == "onedrive" {
                "Connect OneDrive"
            } else if cloud_kind == "dropbox" {
                "Connect Dropbox"
            } else if cloud_kind == "icloud-drive" {
                "Connect iCloud Drive"
            } else if cloud_kind == "caldav" {
                "Connect a calendar server"
            } else { "Connect a WebDAV server" },
        ) {
            ui.Text(
                if cloud_kind == "caldav" {
                    "The server's address, its user name and its password. Its calendars show in Calendar."
                } else if cloud_kind == "webdav" {
                    "The server's address, its user name and its password. Its files mount in Files."
                } else if cloud_kind == "icloud-drive" {
                    "Your Apple ID and its password. Apple sends a verification code to your devices; iCloud Drive's files mount in Files."
                } else if cloud_kind == "nextcloud" {
                    "The server's address. Approve this workspace in the page Nextcloud opens; its files and calendars are kept."
                } else {
                    "Approve this workspace in the page the provider opens. Its files mount in Files."
                },
                role: .caption,
                secondary: true,
            )
            row(gap: .related, align: .center, wrap: true) {
                if cloud_kind == "nextcloud" or cloud_kind == "webdav" {
                    ui.TextInput(
                        bind: cloud_server,
                        placeholder: "The server's address, such as https://cloud.example.org",
                        fill,
                    )
                }
                if cloud_kind == "caldav" {
                    ui.TextInput(
                        bind: cloud_server,
                        placeholder: "The server's address, such as dav.example.org",
                        fill,
                    )
                }
                if cloud_kind == "icloud-drive" or cloud_kind == "webdav" or cloud_kind == "caldav" {
                    ui.TextInput(
                        bind: cloud_username,
                        placeholder: if cloud_kind == "icloud-drive" {
                            "Your Apple ID"
                        } else { "The server's user name" },
                        fill,
                    )
                    ui.TextInput(
                        bind: cloud_password,
                        placeholder: "Its password or app password",
                        secret: true,
                        fill,
                    )
                }
                ui.TextInput(bind: cloud_name, placeholder: "The name shown for it", fill)
            }
            if cloud_kind == "caldav" {
                row(gap: .related, align: .center) {
                    ui.Toggle(bind: cloud_read_only)
                    ui.Text("Read only: nothing is ever written to this server")
                }
            }
            if sign_in_open == "" and sign_in_prompt == "" {
                row(gap: .related, align: .center, wrap: true) {
                    if cloud_kind == "caldav" {
                        ui.Button("Connect", role: .primary) {
                            on press: accounts
                                .add_calendar(
                                    accounts.accounts,
                                    server: cloud_server,
                                    username: cloud_username,
                                    password: cloud_password,
                                    name: cloud_name,
                                    readOnly: cloud_read_only,
                                )
                                .then(set(cloud_password, ""))
                                .then(set(cloud_kind, ""))
                        }
                    } else if cloud_kind == "nextcloud" {
                        ui.Button("Continue", role: .primary) {
                            on press: {
                                let step = accounts.begin_sign_in(
                                    accounts.accounts,
                                    provider: "nextcloud",
                                    server: cloud_server,
                                    name: cloud_name,
                                    mail: false,
                                    calendar: true,
                                    files: true,
                                )
                                set(sign_in, step.id)
                                    .then(
                                        set(sign_in_open, step.open),
                                    )
                                    .then(
                                        set(sign_in_prompt, step.code),
                                    )
                            }
                        }
                    } else {
                        ui.Button("Continue", role: .primary) {
                            on press: {
                                let step = accounts.begin_cloud_sign_in(
                                    accounts.accounts,
                                    kind: cloud_kind,
                                    name: cloud_name,
                                    server: cloud_server,
                                    username: cloud_username,
                                    password: cloud_password,
                                )
                                set(sign_in, step.id)
                                    .then(
                                        set(sign_in_open, step.open),
                                    )
                                    .then(
                                        set(sign_in_prompt, step.code),
                                    )
                                    .then(
                                        set(cloud_password, ""),
                                    )
                                    .then(
                                        set(
                                            cloud_kind,
                                            if step.id == "" { "" } else { cloud_kind },
                                        ),
                                    )
                            }
                        }
                    }
                    ui.Button("Cancel") {
                        on press: set(cloud_kind, "")
                            .then(set(sign_in, ""))
                            .then(set(sign_in_open, ""))
                            .then(set(sign_in_prompt, ""))
                            .then(set(sign_in_code, ""))
                    }
                }
            }
            if sign_in_open != "" {
                ui.Link(sign_in_open, label: "Open the sign-in page")
            }
            if sign_in != "" {
                ui.Text(
                    if sign_in_prompt == "" {
                        "Waiting for you to approve it in your browser. Nothing more is kept until you do."
                    } else { sign_in_prompt },
                    role: .caption,
                    secondary: true,
                )
            }
            if sign_in_prompt != "" {
                row(gap: .related, align: .center, wrap: true) {
                    ui.TextInput(bind: sign_in_code, placeholder: "The code you were sent", fill)
                    ui.Button("Finish signing in", role: .primary) {
                        on press: accounts
                            .submit_sign_in_code(
                                accounts.accounts,
                                signIn: sign_in,
                                code: sign_in_code,
                            )
                            .then(set(cloud_kind, ""))
                            .then(set(sign_in, ""))
                            .then(set(sign_in_open, ""))
                            .then(set(sign_in_prompt, ""))
                            .then(set(sign_in_code, ""))
                            .then(set(cloud_password, ""))
                    }
                }
            }
            if sign_in != "" {
                ui.Button("Cancel the sign-in") {
                    on press: accounts
                        .cancel_sign_in(accounts.accounts, signIn: sign_in)
                        .then(set(sign_in, ""))
                        .then(set(sign_in_open, ""))
                        .then(set(sign_in_prompt, ""))
                        .then(set(sign_in_code, ""))
                }
            }
        }
    }
}

/// The migration source a cloud-storage account's provider names: what copies its files into
/// the workspace once.
fn import_source(provider: Text) -> Text {
    if provider == "google-drive" {
        "gdrive"
    } else if provider == "onedrive" {
        "onedrive"
    } else if provider == "dropbox" {
        "dropbox"
    } else if provider == "icloud-drive" {
        "icloud"
    } else if provider == "nextcloud" { "nextcloud" } else { "webdav" }
}

/// The incoming server a vault logins's places name, without its scheme.
fn imap_place(places: List<Text>) -> Text {
    for place in places {
        if place.starts_with("imap://") {
            return place.replace("imap://", "")
        }
    }
    ""
}

/// The outgoing server a vault login's places name, without its scheme.
fn smtp_place(places: List<Text>) -> Text {
    for place in places {
        if place.starts_with("smtp://") {
            return place.replace("smtp://", "")
        }
    }
    ""
}

/// Whether a vault login belongs to a mail account: it signs in to an `imap` server and to an
/// `smtp` one. The vault lists every login it keeps; the ones for a mail server are Mail's.
fn is_mail_login(choice: accounts.MailSignIn) -> Bool {
    imap_place(choice.places) != "" and smtp_place(choice.places) != ""
}
