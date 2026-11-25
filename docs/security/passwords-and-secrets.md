# Passwords and app credentials

Passwords is the user-facing inventory for Tilo's encrypted secret store. Titles,
usernames, service origins, notes, custom fields and passwords are all inside the
authenticated vault ciphertext. The plaintext account store never contains them.

An app or a device process never opens a vault itself. Every user-managed secret
is an entry of the workspace's vault: an account's password names its entry, and a
messaging account's profile key is one too. The session releases an entry under the
person's role on Passwords, at the servers the entry belongs to, writing the use
into the entry first (`crates/session/session/src/account_passwords.rs`); whoever
may read the vault may read every entry, with no second approval per app. The
returned secret uses `Zeroizing<String>` so its buffer is cleared when dropped.

The vault is XChaCha20-Poly1305 sealed with a fresh random nonce per write.
`tilo-secret-vault` owns the envelope and migration format; `tilo-secrets` is its
shared platform key-ownership API (Secret Service on Linux, Keychain on macOS,
WebCrypto-wrapped IndexedDB on the web). Windows accounts retain their DPAPI
ownership. Existing accounts vault paths, key names and encrypted entries are
preserved. An unavailable or damaged existing key never authorizes replacement.

The person's accounts vault and the handoff credentials beside it have one
ownership on every host Tilo runs on: the platform's secret store (the Secret
Service on Linux, the Keychain on macOS) where it answers, otherwise an
owner-only `vault.key` beside the ciphertext, and
`SecretVault::ownership()` reports which so the UI never calls a key file a
keyring. An owner-only key left by that fallback migrates into the secret store,
after a verified round trip, once it answers. `SecretVault::open_in` opens a
store of someone else's root (a capture, an imported copy, a test's) under its
own private key, so it never reads or replays the person's.

The Passwords app has no network access and does not sync secrets through CRDT state.

## Native, browser and hosted callers

Provider keys and service tokens are the workspace's, entries of its vault like an
account's password; a device keeps none of its own. The Hugging Face sign-in that
downloads models with the person's account is the local model provider's key in
the workspace's `tilo.models`, set through the published `models#set-key` (from
Settings' AI page, or from the manager's own sign-in in a terminal). The model
manager is a device helper and holds no credential: the workspace's models
provider hands it the token with each download request alone.

The trusted service boundary constructs authorization from authenticated caller
context and the approved configuration, not from untrusted RPC arguments.
Credentials written without an origin binding are not read or adopted; the
person enters the key again for the bound origin. No second provider vault is
created.

Use `tilo_secrets::SecretStore::for_app` for platform-owned ownership material.
Both native and browser callers have async `get`, `put` and `delete`; secrets are
zeroized on Rust drop. `SecretStore::with_backend` accepts a host's `SecretBackend`, for
example a lightweight backend that keeps ownership outside its web frontend. Such
a backend must authenticate the caller, authorize its app namespace and use
protected storage; the storage trait itself is not an authorization protocol.
There is no plaintext-file, localStorage or network fallback.

The workspace's vault is the inventory for user-managed passwords, and the
workspace's rules decide who reads it. App-scoped key names in `tilo-secrets`
are namespacing, **not** authorization. A trusted frontend or
backend must derive caller identity and origin from its authenticated execution
context, not accept arbitrary app IDs from an untrusted request.

A browser-only frontend can use the built-in IndexedDB backend without a desktop
keyring. It requires WebCrypto and Web Locks for writes, and reports failures
rather than selecting weaker storage. Non-extractable keys prevent ordinary key
export, but do not defend against scripts in the same origin, a compromised
browser, or an attacker controlling the browser profile. Do not equate this with
hardware-backed key storage.

Browser hosts using blocking accounts APIs must await
`tilo_secrets::web::mirror()` before mounting consumers and await
`tilo_secrets::web::flush()` before reporting durable success. Blocking writes
are staged, not committed, until that flush succeeds. Missing keys or corrupted
ciphertext fail the entire mirror load instead of presenting an empty vault.
A failed write queue stays failed until reload/recovery; later flushes cannot
silently turn data loss into success. Async writes update the mirror only after
storage acknowledges persistence.

A browser page keeps no account secrets of its own: they are entries of the
workspace's vault, and the page's accounts vault is empty and takes no write.

API Tester follows the same acknowledgement rule for immutable authentication
secrets: a pending write has no reference in the collaborative document. Browser
hosts flush secrets before app files and document commits, including close.
This protects ordering, not caller authentication: all scripts sharing an origin
can invoke its WebCrypto keys and read its credentials. Use a trusted origin and
authenticated host/backend boundaries; app namespaces and browser storage locks
do not isolate mutually untrusted same-origin apps.

Native vault saves take a shared
cross-process advisory write lock, reload the authenticated inventory and merge
only explicitly edited keys. Stale account, password-manager and provider
writers therefore preserve each other's unrelated changes. A conflicting
same-key edit fails and requires reopening, rather than overwriting a changed
origin binding or resurrecting a removed credential. Atomic file replacement
alone is not sufficient for a whole-inventory transaction.

## Account secrets for sandboxed apps: the session is the owner

A sandboxed app never opens the person's account vault. Its config root is
its own app directory, the vault's files are outside the sandbox, and the key
ownership that seals them (the Secret Service, or `TILO_SECRETS_DIR` on a
headless machine) is outside it too: the app's D-Bus proxy passes portals
only, and the sandbox environment is cleared. The session process holds the
person's vault under session ownership and is the one owner.

Apps reach it with one request shape on their own certified route
(`runtime_collab::BrowserServiceSessionCommand`, the channel method that also
carries Browser's website sessions). The session's broker service
(`shell/broker_services/service_sessions.rs`) authorizes each request for the
app that sends it: the certified person's own route, Read on that app, no
agent or host connection.

- **Keeping** (`StoreAccountSecret`): only Settings, the keeper of the
  workspace's accounts. The password goes into an entry of the workspace's
  vault, bound to the servers the account names, and Settings is told the
  entry's reference (`vault.item:<id>`), which the account names as its
  `passwordItem`. The secret is a `secret` parameter: kept out of logs,
  history and confirmations, and never readable back by Settings.
- **Release**: an app's connector reads the accounts through the device's one
  dispatch, as the app its launch record says it runs (`accounts#sign-ins`
  names the account each vault login signs in for), and asks `vault#release`
  for an entry's secret on that connection. The dispatch answers from this
  device's copy of the workspace's vault, decided on the grants that name the
  app and the person's rules on Passwords, only for an enabled account that
  names that entry, only for the servers that account names now, and writes
  the use (which app signed in to which account) into the entry first. Where
  no grant lets the app read the entry, the person is asked, once, and the
  release is refused until they answer. A sign-in a provider's own page left
  is handed out as the access token the grant holds now, renewed where the
  grant is; the refresh token never leaves the vault. The app keeps the secret
  in memory for signing in and never writes it anywhere.
- **Website sessions** (`List`, `Resolve`, `Forget`): only Browser. A website
  session is one of the workspace's accounts whose `passwordItem` names an
  entry of the workspace's vault holding the cookies the person selected for a
  site (`tilo_workspace_service::workspaces::website_sessions`). The entry is no
  item anything signs in or fills a field with. The session lists and opens
  only the asking person's own selection, writes each opening into the entry's
  uses first, and forgets one only for someone who administers the workspace:
  the cookies leave the vault, then the account leaves the accounts record.

File accounts are mounted by the person's cloud service (`fs cloud-service`),
not by an app: it asks the daemon that keeps each workspace for the one
sign-in a mount uses, per request (`tilo.accounts#cloud-credential`), and
keeps no vault and no sign-in of its own. A `WebDAV` or Nextcloud account
signs in with the entry it names, decided on the person's grants on Passwords
and on Files, with the use naming Files. The secret goes to the account's
mount on its standard input and stays in memory. No password of such an
account is handed off in a file. An account its provider's page
signed in (Google Drive, OneDrive, Dropbox, iCloud Drive) keeps that grant on
this device, and only its short-lived access token or session headers pass
through the sealed handoff store, which Settings' credential helper fills.

Ownership does not sign in for the app: Mail's IMAP and SMTP sessions run in
Mail, so an account's secret is released to it. The session logs each keep,
release and refusal with the entry and app, never the value
(`tilo::account_passwords`).

Outside a session (a test, a `--data-dir` profile) an explicit config root
keeps its own vault under a local key, and Settings and Mail open that
directly.

## Remaining consumer migrations

Ownership refresh tokens are system-owned grants, not interactive
password-manager disclosures, and are not forced through Passwords consent.
Secrets never enter account JSON, CRDT state, debug output or logs. The low-level `SecretVault` API stays public for
Settings, recovery and adapters not yet migrated. Runtime channel and identity stores already
use `tilo-secrets` directly for their own app-owned material. Nothing invents a
second plaintext credential file. A remote credential HTTP protocol and its
caller authentication are not supplied by `SecretBackend`.

Password generation uses OS randomness, rejection sampling (no modulo bias), a
20-character default, and omits commonly confused characters.

The Passwords UI keeps search and edit drafts in process-local state. Reveal is
masked again after ten seconds. Copies are offered through native effect actions
and expire after thirty seconds; Tilo clears only if the clipboard still contains
the copied password, so it never erases newer user content. Editing with a blank
password preserves the existing one, and deletion requires a second explicit
confirmation.
