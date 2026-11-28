app t.totp "Authenticator" (icon: .key, category: .utility)

/// An account whose code rotates.
type Account {
    name: Text
    code: Text
}

root Account

default view account for Account

/// What the account shows: a code beside its name, revealed only when asked for. A capture or a
/// stream leaves it out wherever it is shown, and the window hides it again when it loses focus.
view account(a: Account) {
    protect capture
    state revealed: Bool = false
    stack {
        text(a.name)
        row {
            if revealed {
                ui.Button("Hide") {
                    on press: set(revealed, false)
                }
            } else {
                ui.Button("Reveal") {
                    on press: set(revealed, true)
                }
            }
        }
        ui.PropertyList {
            // The window hid a secret when it lost the focus: the reveal goes, so the item reads
            // "Hidden" and its control reads "Reveal" again.
            on hidden: set(revealed, false)
            ui.Property(
                "One-time code",
                if revealed { "482913" } else { "Hidden" },
                secret: revealed,
            )
        }
        ui.Text(
            if revealed { "SOLO-SECRET-CODE" } else { "Hidden" },
            secret: revealed,
        ) {
            on hidden: set(revealed, false)
        }
    }
}
