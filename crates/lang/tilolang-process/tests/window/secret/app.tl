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
            ui.Button("Reveal") {
                on press: set(revealed, true)
            }
            ui.Button("Hide") {
                on press: set(revealed, false)
            }
        }
        ui.PropertyList {
            ui.Property(
                "One-time code",
                if revealed { "482913" } else { "Hidden" },
                secret: revealed,
            )
        }
        text(
            if revealed { "SOLO-SECRET-CODE" } else { "Hidden" },
            secret: revealed,
        )
    }
}
