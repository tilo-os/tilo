# Account credential custody

Mail uses the canonical accounts inventory and `accounts::AccountCredentials`
for encrypted account secrets. The facade deliberately retains the existing
account-ID keys so accounts configured in Settings remain interoperable;
opening it does not request interactive service-credential consent.

Normal loading and persistence use protected default custody. Explicit `_in`
seams select a recovery/test config root, never a fallback from failed protected
custody. `AccountStore::try_load` and `try_load_in` return custody errors.
Compatibility loaders report only a sanitized error and return an empty result.
Persistence propagates open/save errors before publishing account metadata.

Passwords stay in memory and encrypted custody, never account JSON, shared CRDT
state, debug output, or logs. Passwordless OAuth accounts retain their existing sealed tokens.
Direct `SecretVault` use is limited to legacy-compatibility test fixtures.
