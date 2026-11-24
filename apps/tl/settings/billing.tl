/// The person's account with the host that runs their workspaces: the plan it is on, the
/// payment method on file, what its Linux machines used in the billing period, the credit
/// it has left and the invoices the provider issued. Paying, changing a plan or adding a
/// card stays on the provider's page, which each of these opens.
view billing_pane() {
    let a = billing.account
    if a.state == .reading {
        ui.Section("Billing") {
            ui.StatusLine("Reading your account…", status: .active)
        }
    } else if a.state == .unreachable {
        ui.Section("Billing") {
            ui.StatusLine(
                if a.words == "" { "Your host could not be reached." } else { a.words },
                status: .warning,
            )
            ui.Text(
                "Your plan and invoices are still on your host's page, and this device asks again by itself.",
                role: .caption,
                secondary: true,
            )
            manage_button(a)
        }
    } else if a.state == .no-account {
        ui.Section(a.server) {
            ui.Text(
                if a.words == "" {
                    "Your workspaces run here itself, so there is nothing to pay for."
                } else { a.words },
                role: .body,
            )
            manage_button(a)
        }
    } else if a.state == .signed-out {
        ui.Section(a.server) {
            ui.Text(
                if a.words == "" {
                    "You are not signed in to this host yet."
                } else { a.words },
                role: .body,
            )
            manage_button(a)
        }
    } else {
        account_section(a)
        invoices_section(a)
    }
}

/// What the account is on and what it has used, with the way to the provider's page.
view account_section(a: billing.HostAccount) {
    ui.Section(a.server) {
        ui.PropertyList {
            ui.Property(
                "Plan",
                if a.plan_name == "" {
                    "No plan yet"
                } else if a.plan_price == "" {
                    a.plan_name
                } else { "{a.plan_name}, {a.plan_price}" },
            )
            ui.Property(
                "Card",
                if not a.has_card {
                    "None yet"
                } else if a.card_brand != "" and a.card_last4 != "" {
                    "{a.card_brand} ·· {a.card_last4}"
                } else if a.card_last4 != "" {
                    "Card ·· {a.card_last4}"
                } else if a.card_brand != "" { a.card_brand } else { "On file" },
            )
            ui.Property(
                "Linux machines this month",
                if let included = a.included_seconds {
                    "{machine_time(a.usage_seconds)} of {machine_time(included)}"
                } else if a.usage_seconds == 0 {
                    "None yet"
                } else { machine_time(a.usage_seconds) },
            )
            if a.credit != "" {
                ui.Property("Credit", a.credit)
            }
        }
        if not a.has_card {
            ui.Text(
                "You are asked for a card the first time something needs a Linux machine.",
                role: .caption,
                secondary: true,
            )
        }
        manage_button(a)
    }
}

/// The invoices the provider issued, newest first: each opens its own page.
view invoices_section(a: billing.HostAccount) {
    ui.Section("Invoices") {
        if a.invoices.is_empty() {
            ui.Text("No invoices yet.", role: .caption, secondary: true)
        }
        for invoice in a.invoices {
            ui.ListRow(
                invoice.amount,
                secondary: if let at = invoice.issued {
                    "{invoice_status(invoice.status)}, issued {at.day()}"
                } else { invoice_status(invoice.status) },
                icon: .receipt,
            ) {
                trailing: {
                    if invoice.page != "" {
                        ui.Button("Open", icon: .external_link) {
                            on press: open_invoice(a, invoice.id)
                        }
                    }
                }
                on activate: open_invoice(a, invoice.id)
            }
        }
    }
}

/// The way to the provider's page, where a card, a plan and invoices change: the
/// provider's own page, never a form Tilo draws.
view manage_button(a: billing.HostAccount) {
    if a.manage != "" {
        ui.Button(
            if a.has_card { "Manage billing" } else { "Add a card" },
            icon: .external_link,
            role: .primary,
        ) {
            on press: open_manage_page(a)
        }
    }
}

/// A length of machine time, as a person reads it: hours and minutes, minutes, or seconds.
fn machine_time(seconds: Int) -> Text {
    let hours = seconds / 3600
    let minutes = seconds % 3600 / 60
    if hours > 0 {
        if minutes > 0 { std.say("{hours} hr {minutes} min") } else { std.say("{hours} hr") }
    } else if minutes > 0 { std.say("{minutes} min") } else { std.say("{seconds} s") }
}

/// Where an invoice stands, as its row says.
fn invoice_status(status: billing.HostInvoiceStatus) -> Text {
    if status == .paid {
        std.say("Paid")
    } else if status == .past-due {
        std.say("Past due")
    } else if status == .void { std.say("Void") } else { std.say("Due") }
}

/// Opens the provider's page where a card, a plan and the invoices change, in the app that
/// opens web addresses: the provider's own page, never a form Tilo draws. The page is the one
/// the account names, so nothing an app or an agent passes can open another, and an account
/// that names none opens nothing.
action open_manage_page(account: billing.HostAccount) (label: "Open the billing page") {
    if account.manage != "" {
        match tool.open_link(account.manage) {
            .ok => {}
            .err(_) => {}
        }
    }
}

/// Opens one of the account's invoices on the provider's page. The invoice is the account's
/// own, looked up by its id, so only a page the account names opens and an id it does not
/// hold opens nothing.
action open_invoice(account: billing.HostAccount, invoice: Text) (label: "Open an invoice") {
    if let held = account.invoices.find(other -> other.id == invoice) {
        if held.page != "" {
            match tool.open_link(held.page) {
                .ok => {}
                .err(_) => {}
            }
        }
    }
}
