# App Publisher

Manages the publisher key that signs your Tilo apps and applies to the
registry for the namespaces your app ids live in. It is the graphical side of
`tilo-app keys` and `tilo-app apply`, and reads and creates the same key: one
secret in this app's secret store (the system keychain), which `tilo-app`
addresses under this app's id.

The key never enters the app's resource, which is synced to collaborators and
kept in history; only the namespaces, contact and registry do. Creating the
key and copying it for CI need the owner's role and a confirmation, through
the buttons and through the action door alike.

The profile resource's actions set the domain, forge account, contact and
registry; its effects act on the key and the registry: `create-key`,
`copy-proof`, `copy-secret`, `apply-for-namespaces`, `check-status`. Each is
offered only while the key on this machine allows it (no second key, nothing
to copy or apply with before there is one).

See [docs/publishing.md](../../docs/publishing.md) for the whole flow.
