# Account credential storage

Calendar uses the canonical accounts inventory. A calendar account's password is the
workspace's: Settings keeps it in the workspace's vault, and the account names that
entry (`passwordItem`). The app asks the session for it on the window's own route when
it signs in, and holds it in memory only.

`accounts::AccountCredentials` is not opened for a calendar's password. Its remaining
use here is the OAuth token store the CalDAV transport refreshes a grant with.

Passwords stay in memory, never account JSON, shared CRDT state, debug output, logs or
this device's own stores.
