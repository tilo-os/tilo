# Passwords and app credentials

Passwords is the user-facing inventory for Tilo's encrypted secret store. Titles,
usernames, service origins, notes, custom fields and passwords are all inside the
authenticated vault ciphertext. The plaintext account store never contains them.

Applications requesting user-managed passwords should use
`accounts::CredentialBroker`, not open `SecretVault`.
Every request names the calling app, item, purpose, and (for web sign-ins) exact
origin. The broker validates the origin before prompting and supports one-time,
session and durable consent. Session grants are memory-only and disappear at
logout; durable grants are encrypted in the vault and can be inspected and
revoked. Users can revoke an app immediately. The returned password uses `Zeroizing<String>` so its
buffer is cleared when dropped.

The vault is XChaCha20-Poly1305 sealed with a fresh random nonce per write.
`tilo-secret-vault` owns the envelope and migration format; `tilo-secrets` is its
shared platform key-custody API (Secret Service on Linux, Keychain on macOS,
WebCrypto-wrapped IndexedDB on the web). Windows accounts retain their DPAPI
custody. Existing accounts vault paths, key names and encrypted entries are
preserved. An unavailable or damaged existing key never authorizes replacement.

New default accounts vaults refuse unavailable protected storage rather than
silently creating a local key. Existing owner-only local keys remain readable and
migrate through the shared vault layer when protected custody becomes available.
`SecretVault::open_in` explicitly selects recovery/test custody (local keys on
Unix, DPAPI on Windows); callers must surface that weaker Unix custody honestly.
The Passwords app has no network access and does not sync secrets through CRDT state.

## Native, browser and hosted callers

Provider and service credentials use `accounts::ServiceCredentials`, alongside
password-manager items in the existing accounts vault. `ServiceAuthorization`
binds the authenticated service, purpose and canonical endpoint origin;
`CredentialRef` is the opaque, serializable reference stored in non-secret
configuration. `status`, `store`, `remove` and `resolve` require that binding.
Changing a profile's endpoint never authorizes its previous key at the new
origin. Native writes return durable storage failures, not best-effort success.
Resolution returns `Zeroizing<String>`; status never returns the key.

The trusted service boundary constructs authorization from authenticated caller
context and the approved configuration, not from untrusted RPC arguments.
`migrate_agent_provider` adopts existing `agent-provider-<profile>` entries only
when the supplied original endpoint matches the authorization. Old entries have
no origin metadata: callers must retain the original approved endpoint or ask
for explicit adoption; they must not infer it from a newly edited profile.
Adoption and old-entry removal share one vault write; retries preserve a newer
bound entry. No second provider vault is created.

Use `tilo_secrets::SecretStore::for_app` for platform-owned custody material.
Both native and browser callers have async `get`, `put` and `delete`; secrets are
zeroized on Rust drop. `SecretStore::with_backend` accepts a host's `SecretBackend`, for
example a lightweight backend that keeps custody outside its web frontend. Such
a backend must authenticate the caller, authorize its app namespace and use
protected storage; the storage trait itself is not an authorization protocol.
There is no plaintext-file, localStorage or network fallback.

Accounts' encrypted container and `CredentialBroker` remain the inventory and
consent layer for user-managed passwords. App-scoped key names in `tilo-secrets`
are namespacing, **not** a replacement for broker consent. A trusted frontend or
backend must derive caller identity and origin from its authenticated execution
context, not accept arbitrary app IDs from an untrusted request.

A browser-only frontend can use the built-in IndexedDB backend without a desktop
keyring. It requires WebCrypto and Web Locks for writes, and reports failures
rather than selecting weaker storage. Non-extractable keys prevent ordinary key
export, but do not defend against scripts in the same origin, a compromised
browser, or an attacker controlling the browser profile. Do not equate this with
hardware-backed key custody.

Browser hosts using blocking accounts APIs must await
`tilo_secrets::web::mirror()` before mounting consumers and await
`tilo_secrets::web::flush()` before reporting durable success. Blocking writes
are staged, not committed, until that flush succeeds. Missing keys or corrupted
ciphertext fail the entire mirror load instead of presenting an empty vault.
A failed write queue stays failed until reload/recovery; later flushes cannot
silently turn data loss into success. Async writes update the mirror only after
storage acknowledges persistence.

Accounts page edits use an encrypted JSON inventory in the same durable secret
backend. The entire reload/compare/merge/write holds the app's Web Lock, not just
key creation. Existing encrypted page files seed migration only when that
inventory is absent; unrelated entries survive. Each changed entry carries its
original value, so stale conflicting edits fail without replacing custody.
The synchronous save reports pending until the transaction commits: retain the
draft and retry the same save after browser storage completes. Only an
acknowledged save may publish dependent metadata. Storage failures require
recovery/reload, not treating a queued write as success.

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

## Remaining consumer migrations

Mail, Calendar and system account custody use `accounts::AccountCredentials`,
the shared account-owner facade alongside `ServiceCredentials`. It adopts legacy
account-ID password and OAuth entries in place, without copying or renaming them;
Settings can still edit passwords, refresh grants and remove credentials at the
canonical key. Custody refresh tokens are system-owned grants, not interactive
password-manager disclosures, and are not forced through Passwords consent.
Only trusted account owners and refreshers may use this facade. Opening damaged
or unavailable custody returns an error; writes retain the sealed vault's atomic,
locked, conflict-checked persistence. Secrets never enter account JSON, CRDT
state, debug output or logs. The low-level `SecretVault` API stays public for
Settings, recovery and adapters not yet migrated. Runtime channel and identity stores already
use `tilo-secrets` directly for their own app-owned material. Provider configuration
and model gateways use the typed service-credential facade, never open the vault
directly or invent a second plaintext credential file. A remote credential HTTP protocol and its
caller authentication are not supplied by `SecretBackend`.

Password generation uses OS randomness, rejection sampling (no modulo bias), a
20-character default, and omits commonly confused characters.

The Passwords UI keeps search and edit drafts in process-local state. Reveal is
masked again after ten seconds. Copies are offered through native effect actions
and expire after thirty seconds; Tilo clears only if the clipboard still contains
the copied password, so it never erases newer user content. Editing with a blank
password preserves the existing one, and deletion requires a second explicit
confirmation.
