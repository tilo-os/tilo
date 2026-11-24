//! The kinds of held connection an app can keep (`app ... (connections: ["lsp"])`).
//!
//! A held connection is work with no end that an app keeps open to something outside it: a
//! language server, a chat gateway, an SSH or VNC session. It is never an engine job: a job ends,
//! and a held connection lives until the app or the person closes it. It runs in the app's own
//! connection process (the app's launch with no window, in its package's sandbox and with its
//! package's network grant), which the device holds whether or not a window of the app is open.
//!
//! Each kind is native code the platform carries. An action opens one by a key of its own with
//! `connection.open(kind, key, ask)`, sends it a message with `connection.send(key, message)` and
//! ends it with `connection.close(key)`. What the connection says of itself is a word of JSON a
//! view reads with `connection.said(key)`, and the view runs again when it says something new.

/// One kind of held connection the platform keeps.
#[derive(Clone, Copy, Debug)]
pub struct ConnectionKind {
    /// What a package names it by (`lsp`).
    pub name: &'static str,
    pub doc: &'static str,
    /// Whether its code releases a vault item into the app's connection process (a gateway's
    /// token, an SSH key): the package then asks for the account sign-ins it is released
    /// through.
    pub releases: bool,
}

/// Every kind of held connection.
pub const KINDS: &[ConnectionKind] = &[ConnectionKind {
    name: "lsp",
    doc: "A language server for a project's folder on this device. `open` asks \
          `{language, root}`; `send` takes `{op, path, language, text, version}` (`op` is `open`, \
          `change` or `saved`) or `{op: \"ask\", path, line, column, what}` (`what` is `complete`, \
          `hover`, `definition` or `format`). Its word is `{state, server, failure, progress, \
          diagnostics, answer}`.",
    releases: false,
}];

/// The kind called `name`.
#[must_use]
pub fn kind(name: &str) -> Option<&'static ConnectionKind> {
    KINDS.iter().find(|kind| kind.name == name)
}

/// The kinds' names, as a sentence names them.
#[must_use]
pub fn names() -> String {
    KINDS
        .iter()
        .map(|kind| format!("`{}`", kind.name))
        .collect::<Vec<_>>()
        .join(", ")
}
