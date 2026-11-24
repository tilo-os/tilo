app t.totp "Authenticator" (icon: .key, category: .utility)

/// An account whose code rotates.
type Account {
    name: Text
    code: Text
}

root Account

default view account for Account

/// The account's rotating code: a capture or a stream leaves it out, wherever it is shown.
view account(a: Account) {
    protect capture
    stack {
        text(a.name)
        text(a.code)
    }
}
