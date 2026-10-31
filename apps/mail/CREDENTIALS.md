# Account credential custody

Mail uses the canonical accounts inventory and `accounts::AccountCredentials`
for encrypted account secrets. The facade deliberately retains the existing
account-ID keys so accounts configured in Settings remain interoperable;
opening it does not request interactive service-credential consent.

In a session Mail is sandboxed and never opens the person's vault: the
session's account custody releases it each email account's password on
Mail's own route (`src/custody.rs`; docs/security/passwords-and-secrets.md,
"the session is the custodian"). Settings seals the password when it adds the
account.

Outside a session, loading and persistence use the profile's own vault. Explicit `_in`
seams select a recovery/test config root, never a fallback from failed protected
custody. `AccountStore::try_load` and `try_load_in` return custody errors.
Compatibility loaders report only a sanitized error and return an empty result.
Persistence propagates open/save errors before publishing account metadata.

Passwords stay in memory and encrypted custody, never account JSON, shared CRDT
state, debug output, or logs. Passwordless OAuth accounts retain their existing sealed tokens.
Direct `SecretVault` use is limited to legacy-compatibility test fixtures.
